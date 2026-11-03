//! Tilly on Telegram: chat with the agent and answer approval requests. The
//! executor and the approval sweeper run in the same process.

mod config;
mod setup;
mod telegram;
mod webhook;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use secrecy::ExposeSecret;
use teloxide::prelude::*;
use tracing_subscriber::EnvFilter;
use warden_agent_core::tools::{ListContacts, PaymentStatus, ProposePayment};
use warden_agent_core::{Agent, DynTool};
use warden_domain::ports::{IdGen, Rail, SystemClock, UuidV7};
use warden_executor::{Executor, PauseSwitch};
use warden_policy::Payments;
use warden_rail_sol::SolRail;
use warden_store::PgStore;

use crate::config::Config;
use crate::telegram::{App, ChatTurn, TelegramApprover, TelegramNotifier};
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
    let clock = Arc::new(SystemClock);
    let ids: Arc<dyn IdGen> = Arc::new(UuidV7);
    let store_arc = Arc::new(store.clone());

    let payments = Payments::new(
        store_arc.clone(),
        store_arc.clone(),
        Arc::new(TelegramApprover {
            bot: bot.clone(),
            chat: owner,
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
        Arc::new(ListContacts::new(contacts.clone())),
        Arc::new(PaymentStatus::new(payments.clone())),
    ];
    let agent = Agent::new(
        llm,
        tools,
        payments.clone(),
        setup::agent_id(owner.0),
        clock,
        ids,
    )
    .with_step_limit(config.agent.step_limit)
    .with_briefing(setup::briefing(&config, &assets));

    let pause = PauseSwitch::default();
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

    let sweeper_payments = payments.clone();
    let sweeper = tokio::spawn(async move {
        let mut every = tokio::time::interval(SWEEP_EVERY);
        loop {
            every.tick().await;
            match sweeper_payments.expire_stale().await {
                Ok(0) => {}
                Ok(n) => tracing::info!(expired = n, "approvals expired"),
                Err(e) => tracing::warn!(error = %e, "approval sweep failed"),
            }
        }
    });

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
    });
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
    store.close().await;
    Ok(())
}
