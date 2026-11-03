//! Telegram I/O: approval buttons, payment notices and the chat handlers.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, InlineKeyboardButton, InlineKeyboardMarkup};
use warden_agent_core::{Agent, Message as Turn};
use warden_domain::ports::ReadModel;
use warden_domain::{Approval, ApprovalId, Intent, Proposed, RailRef, Untrusted, UserId};
use warden_executor::{Notice, Notifier, PauseSwitch};
use warden_policy::{Approver, Payments, Resolution};
use warden_rail_sol::{Cluster, SolRail};
use warden_store::PgStore;

/// Turns of chat history the agent sees.
const HISTORY: usize = 10;

pub struct App {
    pub owner: ChatId,
    pub owner_user: UserId,
    pub agent: Agent,
    pub payments: Payments,
    pub store: PgStore,
    pub rail: Arc<SolRail>,
    pub pause: PauseSwitch,
    pub cluster: Cluster,
    pub history: Mutex<Vec<Turn>>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// Sends approval requests as a message with Approve and Reject buttons.
#[derive(Debug, Clone)]
pub struct TelegramApprover {
    pub bot: Bot,
    pub chat: ChatId,
}

#[async_trait]
impl Approver for TelegramApprover {
    async fn request(
        &self,
        approval: &Approval,
        intent: &Intent<Proposed>,
    ) -> Result<(), warden_policy::Error> {
        let id = approval.id.as_uuid().simple();
        let keyboard = InlineKeyboardMarkup::new([[
            InlineKeyboardButton::callback("Approve", format!("a:{id}")),
            InlineKeyboardButton::callback("Reject", format!("r:{id}")),
        ]]);
        let memo = intent
            .memo
            .as_deref()
            .map(|m| format!("\nMemo: {m}"))
            .unwrap_or_default();
        let text = format!(
            "Approve this payment?\n\n{} to {}{memo}\n\nExpires in 15 minutes.",
            intent.amount, intent.to.label
        );
        self.bot
            .send_message(self.chat, text)
            .reply_markup(keyboard)
            .await
            .map_err(|e| warden_policy::Error::Channel(e.to_string()))?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TelegramNotifier {
    pub bot: Bot,
    pub chat: ChatId,
    pub cluster: Cluster,
}

impl TelegramNotifier {
    fn link(&self, rail_ref: &RailRef) -> String {
        match rail_ref {
            RailRef::Sol {
                signature: Some(sig),
                ..
            } => self
                .cluster
                .explorer(sig)
                .map(|url| format!("\n{url}"))
                .unwrap_or_default(),
            _ => String::new(),
        }
    }
}

#[async_trait]
impl Notifier for TelegramNotifier {
    async fn notify(&self, notice: Notice) {
        let text = match &notice {
            // Solana confirms within the submit call; the confirmation notice follows at once.
            Notice::Sent { .. } => return,
            Notice::Confirmed {
                summary, rail_ref, ..
            } => format!("Paid {summary}.{}", self.link(rail_ref)),
            Notice::Failed {
                summary,
                reason,
                abandoned: true,
                ..
            } => format!("Payment of {summary} failed and won't be retried: {reason}"),
            Notice::Failed {
                summary, reason, ..
            } => format!("Payment of {summary} hit a problem ({reason}). Retrying shortly."),
        };
        if let Err(e) = self.bot.send_message(self.chat, text).await {
            tracing::warn!(error = %e, "notice not delivered");
        }
    }
}

const HELP: &str = "Hi, I'm Tilly. Tell me who to pay and how much, e.g. \"pay Acme 0.1 SOL for the logo\".\n\n\
Payments above your approval threshold wait for your tap. Commands:\n\
/balance - wallet address and balance\n\
/pause - stop all payments\n\
/resume - allow payments again";

pub async fn on_message(bot: Bot, msg: Message, app: Arc<App>) -> ResponseResult<()> {
    if msg.chat.id != app.owner {
        // Lets a new owner find their chat id; strangers learn nothing else.
        if msg.text() == Some("/start") {
            bot.send_message(
                msg.chat.id,
                format!(
                    "This Tilly belongs to someone else. Your chat id is {}.",
                    msg.chat.id
                ),
            )
            .await?;
        }
        return Ok(());
    }
    let Some(text) = msg.text() else {
        return Ok(());
    };
    let reply = match text.trim() {
        "/start" | "/help" => HELP.to_owned(),
        "/balance" => match app.rail.balance().await {
            Ok(lamports) => format!(
                "Wallet {}\nBalance: {} SOL{}",
                app.rail.address(),
                warden_domain::Amount::new(u128::from(lamports), warden_rail_sol::sol()).major(),
                if matches!(app.cluster, Cluster::Devnet | Cluster::Testnet) {
                    "\nNeed devnet SOL? https://faucet.solana.com"
                } else {
                    ""
                }
            ),
            Err(e) => format!("Couldn't read the balance: {e}"),
        },
        "/pause" => {
            app.pause.pause();
            "Paused. Nothing will be sent until /resume.".to_owned()
        }
        "/resume" => {
            app.pause.resume();
            "Resumed.".to_owned()
        }
        _ => {
            let _ = bot.send_chat_action(app.owner, ChatAction::Typing).await;
            let history = app
                .history
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            match app
                .agent
                .run(&history, Untrusted::new(text.to_owned()))
                .await
            {
                Ok(report) => {
                    remember(&app, text, &report.text);
                    report.text
                }
                Err(e) => {
                    tracing::error!(error = %e, "agent run failed");
                    "Sorry, something went wrong on my side. Try again in a moment.".to_owned()
                }
            }
        }
    };
    bot.send_message(app.owner, reply).await?;
    Ok(())
}

fn remember(app: &App, user: &str, assistant: &str) {
    let mut history = app.history.lock().unwrap_or_else(|e| e.into_inner());
    history.push(Turn::User {
        text: Untrusted::new(user.to_owned()),
    });
    history.push(Turn::Assistant {
        text: assistant.to_owned(),
    });
    let excess = history.len().saturating_sub(HISTORY);
    history.drain(..excess);
}

pub async fn on_callback(bot: Bot, q: CallbackQuery, app: Arc<App>) -> ResponseResult<()> {
    let from_owner = q.from.id.0 as i64 == app.owner.0;
    let message = q.message.as_ref().map(|m| (m.chat().id, m.id()));
    let outcome = match (from_owner, q.data.as_deref()) {
        (false, _) => Err("Only the owner can answer this.".to_owned()),
        (true, Some(data)) => resolve(&app, data).await,
        (true, None) => Err("Unknown button.".to_owned()),
    };
    let text = match &outcome {
        Ok(text) | Err(text) => text.clone(),
    };
    bot.answer_callback_query(q.id.clone())
        .text(text.clone())
        .await?;
    if let (Ok(_), Some((chat, id))) = (&outcome, message) {
        bot.edit_message_text(chat, id, text).await?;
    }
    Ok(())
}

async fn resolve(app: &App, data: &str) -> Result<String, String> {
    let (action, id) = data.split_once(':').ok_or("Unknown button.")?;
    let approval = ApprovalId::new(id.parse().map_err(|_| "Unknown button.")?);
    let intent = app
        .store
        .approval_intent(approval)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("That approval no longer exists.")?;
    let result = match action {
        "a" => app.payments.grant(intent, approval, app.owner_user).await,
        "r" => app.payments.reject(intent, approval, app.owner_user).await,
        _ => return Err("Unknown button.".into()),
    };
    match result {
        Ok(Resolution::Granted) => Ok("Approved. Sending now.".into()),
        Ok(Resolution::Rejected) => Ok("Rejected. Nothing was sent.".into()),
        Ok(Resolution::Expired) => Ok("This approval expired. Nothing was sent.".into()),
        Ok(Resolution::Superseded) => {
            Ok("The payment changed after you were asked. Nothing was sent.".into())
        }
        Err(warden_policy::Error::NotAwaiting(_)) => Err("Already answered.".into()),
        Err(e) => Err(format!("Couldn't do that: {e}")),
    }
}
