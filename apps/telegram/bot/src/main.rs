//! Tilly on Telegram: chat with the agent and answer approval requests. The
//! executor and the approval sweeper run in the same process.

mod config;
mod dashboard;
mod invoices;
mod setup;
mod telegram;
mod webhook;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use secrecy::ExposeSecret;
use teloxide::prelude::*;
use tracing_subscriber::EnvFilter;
use warden_agent_core::tools::{ListContacts, PaymentStatus, ProposePayment, RequestPayment};
use warden_agent_core::{Agent, DynTool};
use warden_domain::ports::{IdGen, Rail, SystemClock, UuidV7};
use warden_executor::{Executor, PauseSwitch};
use warden_policy::Payments;
use warden_rail_paypal::PaypalRail;
use warden_rail_sol::SolRail;
use warden_store::PgStore;

use crate::config::Config;
use crate::invoices::{TelegramBiller, Watcher};
use crate::telegram::{App, Cards, ChatTurn, TelegramApprover, TelegramNotifier};
use crate::webhook::Webhook;

/// How often expired approvals are swept.
const SWEEP_EVERY: Duration = Duration::from_secs(30);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = Config::load()?;
    // Checked before anything connects, so a bad URL or secret fails fast.
    let webhook = Webhook::resolve(config.telegram.webhook.as_ref())?;

    let store = PgStore::connect(&config.database).await?;
    if config.database.migrate_on_start {
        store.migrate().await?;
    }
    store.ping().await?;
    let assets = config.assets()?;
    let policy = setup::seed_policy(&store, &config, &assets).await?;

    let bot = Bot::new(config.telegram.token.expose_secret());
    let owner = ChatId(config.telegram.owner_chat_id);
    let tokens = assets
        .iter()
        .map(|a| a.asset.clone())
        .filter(|a| a.contract.is_some())
        .collect();
    let rail = Arc::new(SolRail::new(&config.rail.sol.conn)?.with_tokens(tokens)?);
    let cluster = rail.cluster().await?;
    config.check_cluster(cluster)?;
    rail.check_tokens().await?;
    let paypal = match &config.rail.paypal {
        Some(conf) => {
            let paypal = Arc::new(PaypalRail::new(conf)?);
            paypal.check().await?;
            tracing::info!(live = conf.live, "PayPal requests on");
            Some(paypal)
        }
        None => None,
    };
    let mut requestable = rail.assets();
    if paypal.is_some() {
        requestable.push(warden_rail_paypal::usd());
    }
    let clock = Arc::new(SystemClock);
    let ids: Arc<dyn IdGen> = Arc::new(UuidV7);
    let store_arc = Arc::new(store.clone());
    let cards = Cards::default();

    let payments = Payments::new(
        store_arc.clone(),
        store_arc.clone(),
        Arc::new(TelegramApprover {
            bot: bot.clone(),
            chat: owner,
            cards: cards.clone(),
        }),
        clock.clone(),
        ids.clone(),
    );

    let llm = config.agent.llm.client()?;
    let (model, endpoint) = (llm.model().to_owned(), llm.endpoint().clone());
    let llm = Arc::new(llm);
    let contacts = setup::recipients(&config.contacts);
    let tools: Vec<Arc<dyn DynTool>> = vec![
        Arc::new(ProposePayment::new(
            contacts.clone(),
            rail.assets(),
            ids.clone(),
        )),
        Arc::new(RequestPayment::new(
            requestable,
            Arc::new(TelegramBiller {
                rail: rail.clone(),
                paypal: paypal.clone(),
                store: store.clone(),
                bot: bot.clone(),
                chat: owner,
            }),
            ids.clone(),
        )),
        Arc::new(ListContacts::new(contacts.clone())),
        Arc::new(PaymentStatus::new(payments.clone())),
    ];
    let pause = PauseSwitch::default();
    let agent = Agent::new(
        llm,
        tools,
        payments.clone(),
        setup::agent_id(owner.0),
        clock,
        ids,
    )
    .with_step_limit(config.agent.step_limit)
    .with_briefing(setup::briefing(&config, &assets))
    .with_pause({
        let pause = pause.clone();
        Arc::new(move || pause.is_paused())
    });

    let turn = ChatTurn::default();
    let executor = Executor::new(
        store_arc.clone(),
        store_arc,
        vec![rail.clone() as Arc<dyn Rail>],
        pause.clone(),
        Arc::new(TelegramNotifier {
            bot: bot.clone(),
            chat: owner,
            cluster,
            turn: turn.clone(),
        }),
        config.executor.clone(),
    );
    let worker = tokio::spawn(executor.run());

    let (sweeper_payments, sweeper_bot, sweeper_cards) =
        (payments.clone(), bot.clone(), cards.clone());
    let sweeper = tokio::spawn(async move {
        let mut every = tokio::time::interval(SWEEP_EVERY);
        loop {
            every.tick().await;
            let expired = match sweeper_payments.expire_stale().await {
                Ok(expired) => expired,
                Err(e) => {
                    tracing::warn!(error = %e, "approval sweep failed");
                    continue;
                }
            };
            if !expired.is_empty() {
                tracing::info!(expired = expired.len(), "approvals expired");
            }
            // Take the buttons off, so an expired card can't look answerable.
            for card in expired.into_iter().filter_map(|a| sweeper_cards.close(a)) {
                let text = "This approval expired. Nothing was sent.";
                if let Err(e) = sweeper_bot.edit_message_text(owner, card, text).await {
                    tracing::warn!(error = %e, "expired approval card not closed");
                }
            }
        }
    });

    let watcher = tokio::spawn(
        Watcher {
            rail: rail.clone(),
            paypal,
            store: store.clone(),
            bot: bot.clone(),
            chat: owner,
            cluster,
            turn: turn.clone(),
        }
        .run(),
    );

    tracing::info!(
        %model,
        // Host and path only: a custom base_url may carry a key in its query.
        llm = %format_args!("{}{}", endpoint.host_str().unwrap_or_default(), endpoint.path()),
        ?cluster,
        wallet = %rail.address(),
        policy_version = policy.version,
        "Tilly bot starting"
    );
    let app = Arc::new(App {
        owner,
        owner_user: setup::owner_id(owner.0),
        agent,
        payments,
        store: store.clone(),
        rail,
        pause,
        cluster,
        contacts,
        turn,
        history: Mutex::default(),
        cards,
    });
    if let Some(conf) = &config.dashboard {
        dashboard::serve(conf, app.clone(), bot.clone()).await?;
    }
    if let Err(e) = bot.set_my_commands(telegram::commands()).await {
        tracing::warn!(error = %e, "command menu not set");
    }
    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(telegram::on_message))
        .branch(Update::filter_callback_query().endpoint(telegram::on_callback));
    let mut dispatcher = Dispatcher::builder(bot.clone(), handler)
        .dependencies(dptree::deps![app])
        .enable_ctrlc_handler()
        .build();
    match webhook {
        Some(hook) => {
            let listener = hook.serve().await?;
            hook.register(&bot).await?;
            tracing::info!(listen = %hook.listen, "receiving updates by webhook");
            dispatcher
                .dispatch_with_listener(
                    listener,
                    LoggingErrorHandler::with_custom_text("webhook update failed"),
                )
                .await;
        }
        None => dispatcher.dispatch().await,
    }

    worker.abort();
    sweeper.abort();
    watcher.abort();
    store.close().await;
    Ok(())
}
