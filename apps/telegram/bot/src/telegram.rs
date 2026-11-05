//! Telegram I/O: approval buttons, payment notices and the chat handlers.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agile_money_agent_core::{Agent, Message as Turn, RunObserver};
use agile_money_domain::ports::ReadModel;
use agile_money_domain::{Approval, ApprovalId, Intent, Proposed, RailRef, Untrusted, UserId};
use agile_money_executor::{Notice, Notifier, PauseSwitch};
use agile_money_policy::{Approver, Payments, ProposeOutcome, Resolution, fingerprint};
use agile_money_rail_sol::{Cluster, SolRail};
use agile_money_store::{ContactSource, ContactState, PgStore};
use async_trait::async_trait;
use teloxide::prelude::*;
use teloxide::types::{
    BotCommand, ChatAction, InlineKeyboardButton, InlineKeyboardMarkup, MessageId,
};

use crate::contacts::Desk;

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
    pub contacts: Arc<Desk>,
    pub turn: ChatTurn,
    pub history: Mutex<Vec<Turn>>,
    pub cards: Cards,
    /// Wallet sign-in challenges and the Telegram link from the dashboard.
    pub profile: crate::profile::Linking,
}

/// Held while the agent answers the owner. Payment notices wait for it, so a
/// confirmation never lands above the reply that announced the payment.
#[derive(Debug, Clone, Default)]
pub struct ChatTurn(Arc<tokio::sync::Mutex<()>>);

impl ChatTurn {
    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.0.lock().await
    }
}

impl App {
    /// The kill switch, from either channel. Pausing always works, takes effect
    /// before it is stored (failing closed), and survives a restart. Resuming
    /// loosens, so it needs the owner's code once codes are set up.
    pub async fn set_paused(&self, paused: bool, code: Option<&str>) -> Result<String, String> {
        let agent = crate::setup::agent_id(self.owner.0);
        if paused {
            self.pause.pause();
            self.store.set_paused(agent, true).await.map_err(|e| {
                format!("Paused, but not saved, so a restart would resume payments: {e}")
            })?;
            tracing::info!("payments paused");
            return Ok("Paused. Nothing will be sent until you resume.".into());
        }
        let guard = self.contacts.guard();
        if guard.enabled() {
            let code = code.map(str::trim).filter(|c| !c.is_empty()).ok_or(
                "Resuming needs a code: send /resume and the 6-digit code from your authenticator app.",
            )?;
            guard
                .check(code, jiff::Timestamp::now())
                .map_err(|e| e.to_string())?;
        }
        self.store
            .set_paused(agent, false)
            .await
            .map_err(|e| e.to_string())?;
        self.pause.resume();
        tracing::info!("payments resumed");
        Ok("Resumed.".into())
    }
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// The open approval cards in the owner's chat, so an answer given on the
/// dashboard or an expiry can close the card too (rule 8: the first answer
/// wins). Stored, so a restart doesn't leave stale buttons up.
#[derive(Debug, Clone)]
pub struct Cards(pub PgStore);

impl Cards {
    async fn open(&self, approval: ApprovalId, message: MessageId) {
        if let Err(e) = self.0.open_card(approval.as_uuid(), message.0).await {
            tracing::warn!(error = %e, "approval card not remembered");
        }
    }

    /// The card's message, once; later calls get `None`.
    pub async fn close(&self, approval: ApprovalId) -> Option<MessageId> {
        match self.0.close_card(approval.as_uuid()).await {
            Ok(message) => message.map(MessageId),
            Err(e) => {
                tracing::warn!(error = %e, "approval card not looked up");
                None
            }
        }
    }
}

/// Sends approval requests as a message with Approve and Reject buttons.
#[derive(Debug, Clone)]
pub struct TelegramApprover {
    pub bot: Bot,
    pub chat: ChatId,
    pub cards: Cards,
}

#[async_trait]
impl Approver for TelegramApprover {
    async fn request(
        &self,
        approval: &Approval,
        intent: &Intent<Proposed>,
    ) -> Result<(), agile_money_policy::Error> {
        let id = approval.id.as_uuid().simple();
        let keyboard = InlineKeyboardMarkup::new([[
            InlineKeyboardButton::callback("Approve", format!("a:{id}")),
            InlineKeyboardButton::callback("Reject", format!("r:{id}")),
        ]]);
        // The memo is the model's words: shown as such, after the facts.
        let memo = intent
            .memo
            .as_deref()
            .map(|m| format!("\nMemo (written by the agent): {m}"))
            .unwrap_or_default();
        let text = format!(
            "Approve this payment?\n\n{} to {}\nAddress: {}{memo}\n\nExpires in 15 minutes.",
            intent.amount,
            intent.to.label,
            fingerprint(&intent.to.address)
        );
        let sent = self
            .bot
            .send_message(self.chat, text)
            .reply_markup(keyboard)
            .await
            .map_err(|e| agile_money_policy::Error::Channel(e.to_string()))?;
        self.cards.open(approval.id, sent.id).await;
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

const HELP: &str = "Hi, I'm Agile Money. Tell me who to pay and how much, e.g. \"pay Acme 0.1 SOL for the logo\".\n\n\
To get paid, ask for a payment request, e.g. \"request 5 USDC from Dana for the website\", \
or in USD by PayPal if it's set up. I'll send you a QR code to forward and tell you when it's paid.\n\n\
Payments above your approval threshold, and the first payment to anyone, wait for your tap. Commands:\n\
/contacts - who I can pay\n\
/addcontact Name Address - save a payee (needs your authenticator code)\n\
/confirm 123456 - confirm it with the code\n\
/removecontact Name - stop paying someone, at once\n\
/balance - wallet address and balance\n\
/pause - stop all payments\n\
/resume 123456 - allow payments again\n\
/help - this message";

/// The menu Telegram shows next to the message box. Mirrors `HELP`.
pub fn commands() -> Vec<BotCommand> {
    vec![
        BotCommand::new("contacts", "Who I can pay"),
        BotCommand::new("addcontact", "Save a payee: name and address"),
        BotCommand::new("confirm", "Confirm with your authenticator code"),
        BotCommand::new("removecontact", "Stop paying someone"),
        BotCommand::new("balance", "Wallet address and balance"),
        BotCommand::new("pause", "Stop all payments"),
        BotCommand::new("resume", "Allow payments again"),
        BotCommand::new("help", "What I can do"),
    ]
}

/// What follows a command: `/confirm 123 456` -> `123 456`.
fn args(text: &str) -> &str {
    let text = text.trim();
    text.split_once(char::is_whitespace)
        .map_or("", |(_, rest)| rest.trim())
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

/// `/start <token>` from the dashboard's Profile link, in the owner chat.
async fn link_telegram(app: &App, msg: &Message, token: &str) -> String {
    if !app.profile.redeem(token, jiff::Timestamp::now()) {
        return "That link has expired. Open Profile in the dashboard for a new one.".into();
    }
    let (name, username) = msg.from.as_ref().map_or_else(
        || (msg.chat.first_name().unwrap_or("Owner").to_owned(), None),
        |u| (u.full_name(), u.username.clone()),
    );
    let agent = crate::setup::agent_id(app.owner.0);
    match app
        .store
        .link_telegram(agent, &name, username.as_deref())
        .await
    {
        Ok(()) => {
            tracing::info!("Telegram confirmed from the dashboard link");
            "Linked. Your dashboard's Profile now shows this account.".into()
        }
        Err(e) => format!("Couldn't save the link: {e}"),
    }
}

async fn balances(app: &App) -> String {
    let mut lines = vec![format!("Wallet {}", app.rail.address())];
    if let Some(vault) = app.rail.vault() {
        lines.push(format!(
            "SOL is held in vault {}, which enforces its own caps; the wallet pays fees and tokens.",
            vault.address
        ));
    }
    for asset in app.rail.assets() {
        lines.push(match app.rail.balance_of(&asset).await {
            Ok(minor) => format!("{}", agile_money_domain::Amount::new(minor, asset)),
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

async fn contact_list(app: &App) -> String {
    let contacts = match app.contacts.list().await {
        Ok(c) => c,
        Err(e) => return format!("Couldn't read the contacts: {e}"),
    };
    if contacts.is_empty() {
        return "No saved contacts yet. Add one with /addcontact Name Address.".to_owned();
    }
    let line = |c: &agile_money_store::Contact| {
        let when = match (c.state, c.active_at) {
            (ContactState::Confirmed, Some(t)) => {
                format!(" (can be paid from {} UTC)", t.strftime("%b %-d %H:%M"))
            }
            (ContactState::Proposed, _) => " (waiting for your /confirm code)".to_owned(),
            _ => String::new(),
        };
        format!("{}{when}\n{}", c.label, c.address)
    };
    let (active, pending): (Vec<_>, Vec<_>) = contacts
        .iter()
        .partition(|c| c.state == ContactState::Active);
    let mut text = format!(
        "I can pay these contacts:\n\n{}",
        active
            .iter()
            .map(|c| line(c))
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    if !pending.is_empty() {
        text.push_str(&format!(
            "\n\nNot payable yet:\n\n{}",
            pending
                .iter()
                .map(|c| line(c))
                .collect::<Vec<_>>()
                .join("\n\n")
        ));
    }
    text
}

/// Removes a message that carried a code; codes work once, but needn't linger.
async fn forget(bot: &Bot, msg: &Message) {
    if let Err(e) = bot.delete_message(msg.chat.id, msg.id).await {
        tracing::debug!(error = %e, "code message not deleted");
    }
}

pub async fn on_message(bot: Bot, msg: Message, app: Arc<App>) -> ResponseResult<()> {
    if msg.chat.id != app.owner {
        // Lets a new owner find their chat id; strangers learn nothing else.
        if msg.text().map(command) == Some("/start") {
            bot.send_message(
                msg.chat.id,
                format!(
                    "This Agile Money bot belongs to someone else. Your chat id is {}.",
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
    let rest = args(text);
    let reply = match command(text) {
        "/start" if !rest.is_empty() => link_telegram(&app, &msg, rest).await,
        "/start" | "/help" => HELP.to_owned(),
        "/contacts" => contact_list(&app).await,
        "/balance" => balances(&app).await,
        "/pause" => app.set_paused(true, None).await.unwrap_or_else(|e| e),
        "/resume" => {
            if !rest.is_empty() {
                forget(&bot, &msg).await;
            }
            app.set_paused(false, Some(rest))
                .await
                .unwrap_or_else(|e| e)
        }
        "/addcontact" => match rest.rsplit_once(char::is_whitespace) {
            Some((label, address)) => {
                match app
                    .contacts
                    .propose(label, address, ContactSource::Telegram)
                    .await
                {
                    // The desk sent the request card.
                    Ok(_) => return Ok(()),
                    Err(e) => e,
                }
            }
            None => "Send /addcontact, the name, then the full Solana address.".to_owned(),
        },
        "/confirm" => {
            forget(&bot, &msg).await;
            match app.contacts.confirm(rest).await {
                Ok((note, cancel)) => {
                    app.contacts.notify(note, cancel).await;
                    return Ok(());
                }
                Err(e) => e,
            }
        }
        "/removecontact" => match app.contacts.find_active(rest).await {
            Ok(Some(contact)) => app.contacts.remove(contact.id).await.unwrap_or_else(|e| e),
            Ok(None) => format!("{rest:?} isn't a saved contact. /contacts lists them."),
            Err(e) => e,
        },
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
    if action == "cx" {
        let contact = uuid::Uuid::parse_str(id).map_err(|_| "Unknown button.")?;
        return app.contacts.cancel(contact).await;
    }
    let approval = ApprovalId::new(id.parse().map_err(|_| "Unknown button.")?);
    let grant = match action {
        "a" => true,
        "r" => false,
        _ => return Err("Unknown button.".into()),
    };
    let answer = answer(app, approval, grant).await;
    // This card is edited by the caller.
    app.cards.close(approval).await;
    answer
}

/// Grants or rejects an approval for the owner, from either channel. `Ok` is
/// what to show once it's answered; `Err` leaves the approval as it was.
pub async fn answer(app: &App, approval: ApprovalId, grant: bool) -> Result<String, String> {
    let intent = app
        .store
        .approval_intent(approval)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("That approval no longer exists.")?;
    let result = if grant {
        app.payments.grant(intent, approval, app.owner_user).await
    } else {
        app.payments.reject(intent, approval, app.owner_user).await
    };
    match result {
        Ok(Resolution::Granted) => Ok("Approved. Sending now.".into()),
        Ok(Resolution::Rejected) => Ok("Rejected. Nothing was sent.".into()),
        Ok(Resolution::Expired) => Ok("This approval expired. Nothing was sent.".into()),
        Ok(Resolution::Superseded) => {
            Ok("The payment changed after you were asked. Nothing was sent.".into())
        }
        Err(agile_money_policy::Error::NotAwaiting(_)) => Err("Already answered.".into()),
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
        let intent = agile_money_domain::IntentId::new(uuid::Uuid::nil());
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
