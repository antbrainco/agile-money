//! Telegram I/O: approval buttons, payment notices and the chat handlers.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use teloxide::prelude::*;
use teloxide::types::{
    BotCommand, ChatAction, InlineKeyboardButton, InlineKeyboardMarkup, MessageId,
};
use warden_agent_core::{Agent, Message as Turn, RunObserver};
use warden_domain::ports::ReadModel;
use warden_domain::{
    Approval, ApprovalId, Intent, Proposed, RailRef, Recipient, Untrusted, UserId,
};
use warden_executor::{Notice, Notifier, PauseSwitch};
use warden_policy::{Approver, Payments, ProposeOutcome, Resolution};
use warden_rail_sol::{Cluster, SolRail};
use warden_store::PgStore;

/// Turns of chat history the agent sees.
const HISTORY: usize = 10;
/// Telegram allows about one edit a second per chat.
const EDIT_EVERY: Duration = Duration::from_millis(1200);
/// Below Telegram's 4096-character message limit.
const MAX_TEXT: usize = 4000;

pub struct App {
    pub owner: ChatId,
    pub owner_user: UserId,
    pub agent: Agent,
    pub payments: Payments,
    pub store: PgStore,
    pub rail: Arc<SolRail>,
    pub pause: PauseSwitch,
    pub cluster: Cluster,
    pub contacts: Vec<Recipient>,
    pub turn: ChatTurn,
    pub history: Mutex<Vec<Turn>>,
}

/// Held while the agent answers the owner. Payment notices wait for it, so a
/// confirmation never lands above the reply that announced the payment.
#[derive(Debug, Clone, Default)]
pub struct ChatTurn(Arc<tokio::sync::Mutex<()>>);

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
    pub turn: ChatTurn,
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
        // Sent from a task so the executor doesn't wait out the agent's turn.
        let (bot, chat, turn) = (self.bot.clone(), self.chat, self.turn.clone());
        tokio::spawn(async move {
            let _turn = turn.0.lock().await;
            if let Err(e) = bot.send_message(chat, text).await {
                tracing::warn!(error = %e, "notice not delivered");
            }
        });
    }
}

const HELP: &str = "Hi, I'm Tilly. Tell me who to pay and how much, e.g. \"pay Acme 0.1 SOL for the logo\".\n\n\
Payments above your approval threshold wait for your tap. Commands:\n\
/contacts - who I can pay\n\
/balance - wallet address and balance\n\
/pause - stop all payments\n\
/resume - allow payments again\n\
/help - this message";

/// The menu Telegram shows next to the message box. Mirrors `HELP`.
pub fn commands() -> Vec<BotCommand> {
    vec![
        BotCommand::new("contacts", "Who I can pay"),
        BotCommand::new("balance", "Wallet address and balance"),
        BotCommand::new("pause", "Stop all payments"),
        BotCommand::new("resume", "Allow payments again"),
        BotCommand::new("help", "What I can do"),
    ]
}

/// `/contacts@AgileMoneyBot extra` -> `/contacts`; anything else unchanged.
fn command(text: &str) -> &str {
    let text = text.trim();
    if !text.starts_with('/') {
        return text;
    }
    let word = text.split_whitespace().next().unwrap_or(text);
    word.split_once('@').map_or(word, |(cmd, _)| cmd)
}

async fn balances(app: &App) -> String {
    let mut lines = vec![format!("Wallet {}", app.rail.address())];
    for asset in app.rail.assets() {
        lines.push(match app.rail.balance_of(&asset).await {
            Ok(minor) => format!("{}", warden_domain::Amount::new(minor, asset)),
            Err(e) => format!("{}: couldn't read the balance ({e})", asset.code),
        });
    }
    if matches!(app.cluster, Cluster::Devnet | Cluster::Testnet) {
        lines.push(
            "\nTest funds: SOL at https://faucet.solana.com, USDC at https://faucet.circle.com"
                .into(),
        );
    }
    lines.join("\n")
}

fn contact_list(contacts: &[Recipient]) -> String {
    if contacts.is_empty() {
        return "No saved contacts yet. Add them under [[contacts]] in warden.toml.".to_owned();
    }
    let lines: Vec<String> = contacts
        .iter()
        .map(|c| format!("{}\n{}", c.label, c.address))
        .collect();
    format!("I can pay these contacts:\n\n{}", lines.join("\n\n"))
}

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
    let reply = match command(text) {
        "/start" | "/help" => HELP.to_owned(),
        "/contacts" => contact_list(&app.contacts),
        "/balance" => balances(&app).await,
        "/pause" => {
            app.pause.pause();
            "Paused. Nothing will be sent until /resume.".to_owned()
        }
        "/resume" => {
            app.pause.resume();
            "Resumed.".to_owned()
        }
        _ => {
            // Until the reply is final; payment notices queue behind it.
            let _turn = app.turn.0.lock().await;
            let _ = bot.send_chat_action(app.owner, ChatAction::Typing).await;
            let history = app
                .history
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let live = LiveReply::new(bot.clone(), app.owner);
            let reply = match app
                .agent
                .run_observed(&history, Untrusted::new(text.to_owned()), &live)
                .await
            {
                Ok(report) => {
                    remember(&app, text, &report.text, &report.proposals);
                    // From the pause switch, not the model: it can't see it.
                    let queued = report
                        .proposals
                        .iter()
                        .any(|p| matches!(p, ProposeOutcome::Queued { .. }));
                    if queued && app.pause.is_paused() {
                        format!("{}\n\n{PAUSED_NOTE}", report.text)
                    } else {
                        report.text
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "agent run failed");
                    "Sorry, something went wrong on my side. Try again in a moment.".to_owned()
                }
            };
            return live.finish(&reply).await;
        }
    };
    bot.send_message(app.owner, reply).await?;
    Ok(())
}

/// Shows the answer while the model writes it: one message, edited at most
/// every `EDIT_EVERY`, and set to the final text at the end.
struct LiveReply {
    bot: Bot,
    chat: ChatId,
    state: tokio::sync::Mutex<Live>,
}

#[derive(Default)]
struct Live {
    message: Option<MessageId>,
    step: u32,
    text: String,
    shown: String,
    last: Option<Instant>,
}

fn clip(text: &str) -> String {
    match text.char_indices().nth(MAX_TEXT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

impl LiveReply {
    fn new(bot: Bot, chat: ChatId) -> Self {
        Self {
            bot,
            chat,
            state: tokio::sync::Mutex::new(Live::default()),
        }
    }

    async fn show(&self, live: &mut Live, text: String) -> ResponseResult<()> {
        if text.trim().is_empty() || text == live.shown {
            return Ok(());
        }
        match live.message {
            Some(id) => {
                self.bot
                    .edit_message_text(self.chat, id, text.clone())
                    .await?;
            }
            None => live.message = Some(self.bot.send_message(self.chat, text.clone()).await?.id),
        }
        live.shown = text;
        live.last = Some(Instant::now());
        Ok(())
    }

    async fn finish(self, reply: &str) -> ResponseResult<()> {
        let mut live = self.state.lock().await;
        self.show(&mut live, clip(reply)).await
    }
}

#[async_trait]
impl RunObserver for LiveReply {
    async fn text(&self, step: u32, delta: &str) {
        let mut live = self.state.lock().await;
        if live.step != step {
            live.step = step;
            live.text.clear();
        }
        live.text.push_str(delta);
        if live.last.is_some_and(|t| t.elapsed() < EDIT_EVERY) {
            return;
        }
        let text = clip(&live.text);
        if let Err(e) = self.show(&mut live, text).await {
            tracing::debug!(error = %e, "live reply edit skipped");
        }
    }

    async fn tool_call(&self, _step: u32, _name: &str) {
        let _ = self
            .bot
            .send_chat_action(self.chat, ChatAction::Typing)
            .await;
    }
}

const PAUSED_NOTE: &str = "Payments are paused, so this waits until you send /resume.";

/// History keeps only text, so each reply carries what its turn really did.
/// Without it the model sees "pay Bob" answered with "on its way" and can
/// repeat the answer for a new request without calling propose_payment.
fn record(proposals: &[ProposeOutcome]) -> String {
    if proposals.is_empty() {
        return "[record: no payment was proposed in this turn]".to_owned();
    }
    let statuses: Vec<&str> = proposals
        .iter()
        .map(|p| match p {
            ProposeOutcome::Queued { .. } => "allowed",
            ProposeOutcome::AwaitingApproval { .. } => "awaiting_approval",
            ProposeOutcome::Denied { .. } => "denied",
            ProposeOutcome::Duplicate { .. } => "duplicate",
        })
        .collect();
    format!(
        "[record: propose_payment called {} time(s) in this turn: {}]",
        statuses.len(),
        statuses.join(", ")
    )
}

fn remember(app: &App, user: &str, assistant: &str, proposals: &[ProposeOutcome]) {
    let mut history = app.history.lock().unwrap_or_else(|e| e.into_inner());
    history.push(Turn::User {
        text: Untrusted::new(user.to_owned()),
    });
    history.push(Turn::Assistant {
        text: format!("{assistant}\n\n{}", record(proposals)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_drop_the_bot_name_and_arguments() {
        assert_eq!(command("/contacts"), "/contacts");
        assert_eq!(command(" /contacts@AgileMoneyBot "), "/contacts");
        assert_eq!(command("/balance now"), "/balance");
        assert_eq!(command("pay Bob 0.01 SOL"), "pay Bob 0.01 SOL");
    }

    #[test]
    fn record_says_what_the_turn_did() {
        assert_eq!(
            record(&[]),
            "[record: no payment was proposed in this turn]"
        );
        let intent = warden_domain::IntentId::new(uuid::Uuid::nil());
        assert_eq!(
            record(&[
                ProposeOutcome::Queued { intent },
                ProposeOutcome::Denied {
                    intent,
                    reasons: vec![]
                }
            ]),
            "[record: propose_payment called 2 time(s) in this turn: allowed, denied]"
        );
    }

    #[test]
    fn help_lists_every_menu_command() {
        for c in commands() {
            assert!(HELP.contains(&format!("/{}", c.command)), "{}", c.command);
        }
    }
}
