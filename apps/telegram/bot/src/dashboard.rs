//! The dashboard's JSON API: pending approvals, intent and invoice history, and
//! the owner's limits. Served on a local address next to the bot, because an
//! answer here must go through the same `Payments` and close the Telegram card
//! (rule 8). Every request carries the bearer token from `DASHBOARD_TOKEN`; the
//! dashboard's Worker holds it and is the only intended caller.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use jiff::{SignedDuration, Timestamp};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use teloxide::prelude::*;
use uuid::Uuid;
use warden_agent_core::tools::parse_major;
use warden_domain::ports::{EventStore, ReadModel};
use warden_domain::{
    Amount, ApprovalId, Asset, Decision, Event, EventEnvelope, IntentId, IntentState, Policy,
    RailId, RailRef, Rule, StreamId,
};
use warden_store::{PolicySource, PolicyVersion};

use crate::setup;
use crate::telegram::{self, App};

/// `[dashboard]`. On when `DASHBOARD_TOKEN` (or `WARDEN_DASHBOARD__TOKEN`) is set.
#[derive(Debug, Clone, Deserialize)]
pub struct DashboardConfig {
    /// At least 32 characters. From env only.
    pub token: SecretString,
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8787))
}

/// Rows the history lists return.
const HISTORY: i64 = 50;

#[derive(Clone)]
struct Api {
    app: Arc<App>,
    bot: Bot,
    token: Arc<SecretString>,
}

/// Binds the listener and serves the API until the process ends.
pub async fn serve(config: &DashboardConfig, app: Arc<App>, bot: Bot) -> anyhow::Result<()> {
    if config.token.expose_secret().len() < 32 {
        anyhow::bail!("DASHBOARD_TOKEN must be at least 32 characters");
    }
    let api = Api {
        app,
        bot,
        token: Arc::new(config.token.clone()),
    };
    let router = Router::new()
        .route("/api/overview", get(overview))
        .route("/api/approvals", get(approvals))
        .route("/api/approvals/{id}/{action}", post(answer))
        .route("/api/intents", get(intents))
        .route("/api/invoices", get(invoices))
        .route("/api/policy", get(policy).put(save_policy))
        .route("/api/pause", post(pause))
        .layer(middleware::from_fn_with_state(api.clone(), authorize))
        .with_state(api);
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(listen = %config.listen, "dashboard API on");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = %e, "dashboard API stopped");
        }
    });
    Ok(())
}

async fn authorize(
    State(api): State<Api>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !same(given.as_bytes(), api.token.expose_secret().as_bytes()) {
        return failure(StatusCode::UNAUTHORIZED, "missing or wrong token").into_response();
    }
    next.run(request).await
}

/// Compares in time independent of where the inputs differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// An error reply: a status and a message for the owner.
#[derive(Debug)]
struct Failure(StatusCode, String);

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

fn failure(status: StatusCode, message: impl Into<String>) -> Failure {
    Failure(status, message.into())
}

fn internal(e: impl std::fmt::Display) -> Failure {
    tracing::warn!(error = %e, "dashboard request failed");
    failure(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

type Reply<T> = Result<Json<T>, Failure>;

#[derive(Serialize)]
struct Overview {
    wallet: String,
    cluster: String,
    paused: bool,
    policy_version: u32,
    awaiting_approval: usize,
}

async fn overview(State(api): State<Api>) -> Reply<Overview> {
    let app = &api.app;
    let agent = setup::agent_id(app.owner.0);
    let policy = app.store.policy(agent).await.map_err(internal)?;
    let awaiting = app.store.awaiting_approval().await.map_err(internal)?;
    Ok(Json(Overview {
        wallet: app.rail.address().to_string(),
        cluster: format!("{:?}", app.cluster).to_lowercase(),
        paused: app.pause.is_paused(),
        policy_version: policy.map_or(0, |p| p.version),
        awaiting_approval: awaiting.len(),
    }))
}

#[derive(Serialize)]
struct Money {
    /// In the asset's major unit, exact.
    amount: String,
    asset: String,
}

impl From<&Amount> for Money {
    fn from(a: &Amount) -> Self {
        Self {
            amount: a.major(),
            asset: a.asset.code.as_str().to_owned(),
        }
    }
}

#[derive(Serialize)]
struct IntentRow {
    id: IntentId,
    created_at: Timestamp,
    updated_at: Timestamp,
    state: &'static str,
    money: Money,
    to: String,
    address: String,
    memo: Option<String>,
    /// Why policy asked for approval or denied it.
    reasons: Vec<String>,
    approval: Option<ApprovalRow>,
    failure: Option<String>,
    /// Explorer link once the payment is on chain.
    link: Option<String>,
}

#[derive(Serialize)]
struct ApprovalRow {
    id: ApprovalId,
    expires_at: Timestamp,
    /// `granted`, `rejected` or `expired` once answered.
    outcome: Option<&'static str>,
}

fn intent_row(app: &App, id: IntentId, history: &[EventEnvelope]) -> Option<IntentRow> {
    let state = IntentState::replay(history.iter().map(|e| &e.event)).ok()?;
    let mut row = history.iter().find_map(|e| match &e.event {
        Event::IntentProposed { proposal, .. } => Some(IntentRow {
            id,
            created_at: proposal.created_at,
            updated_at: e.at,
            state: state.name(),
            money: Money::from(&proposal.amount),
            to: proposal.to.label.clone(),
            address: proposal.to.address.to_string(),
            memo: proposal.memo.clone(),
            reasons: vec![],
            approval: None,
            failure: None,
            link: None,
        }),
        _ => None,
    })?;
    for envelope in history {
        row.updated_at = envelope.at;
        match &envelope.event {
            Event::IntentDecided {
                decision: Decision::RequireApproval { reasons } | Decision::Deny { reasons },
                ..
            } => row.reasons.clone_from(reasons),
            Event::ApprovalRequested {
                approval,
                expires_at,
                ..
            } => {
                row.approval = Some(ApprovalRow {
                    id: *approval,
                    expires_at: *expires_at,
                    outcome: None,
                });
            }
            Event::ApprovalGranted { .. } => set_outcome(&mut row, "granted"),
            Event::ApprovalRejected { expired, .. } => {
                set_outcome(&mut row, if *expired { "expired" } else { "rejected" });
            }
            Event::PaymentFailed { reason, .. } => row.failure = Some(reason.clone()),
            Event::PaymentSubmitted { rail_ref, .. } | Event::PaymentConfirmed { rail_ref, .. } => {
                if let RailRef::Sol {
                    signature: Some(sig),
                    ..
                } = rail_ref
                {
                    row.link = app.cluster.explorer(sig);
                }
            }
            _ => {}
        }
    }
    Some(row)
}

fn set_outcome(row: &mut IntentRow, outcome: &'static str) {
    if let Some(approval) = &mut row.approval {
        approval.outcome = Some(outcome);
    }
}

async fn approvals(State(api): State<Api>) -> Reply<Vec<IntentRow>> {
    let app = &api.app;
    let mut rows = vec![];
    for id in app.store.awaiting_approval().await.map_err(internal)? {
        let history = app
            .store
            .load(&StreamId::Intent(id))
            .await
            .map_err(internal)?;
        rows.extend(intent_row(app, id, &history));
    }
    Ok(Json(rows))
}

#[derive(Serialize)]
struct Answered {
    message: String,
}

async fn answer(
    State(api): State<Api>,
    Path((id, action)): Path<(Uuid, String)>,
) -> Reply<Answered> {
    let grant = match action.as_str() {
        "approve" => true,
        "reject" => false,
        _ => return Err(failure(StatusCode::NOT_FOUND, "approve or reject")),
    };
    let approval = ApprovalId::new(id);
    let message = telegram::answer(&api.app, approval, grant)
        .await
        .map_err(|e| failure(StatusCode::CONFLICT, e))?;
    // The first answer wins; close the card so it can't be pressed too.
    if let Some(card) = api.app.cards.close(approval) {
        let text = format!("{message} (answered on the dashboard)");
        if let Err(e) = api.bot.edit_message_text(api.app.owner, card, text).await {
            tracing::warn!(error = %e, "approval card not closed");
        }
    }
    Ok(Json(Answered { message }))
}

#[derive(Deserialize)]
struct Limit {
    limit: Option<i64>,
}

fn limit(query: &Limit) -> i64 {
    query.limit.unwrap_or(HISTORY).clamp(1, 200)
}

async fn intents(State(api): State<Api>, Query(query): Query<Limit>) -> Reply<Vec<IntentRow>> {
    let app = &api.app;
    let agent = setup::agent_id(app.owner.0);
    let streams = app
        .store
        .recent_intents(agent, limit(&query))
        .await
        .map_err(internal)?;
    Ok(Json(
        streams
            .iter()
            .filter_map(|(id, history)| intent_row(app, *id, history))
            .collect(),
    ))
}

#[derive(Serialize)]
struct InvoiceRow {
    id: String,
    created_at: Timestamp,
    client: String,
    money: Money,
    rail: RailId,
    url: String,
    /// `open`, `paid` or `cancelled`.
    state: &'static str,
    received: Option<Money>,
    reason: Option<String>,
}

fn invoice_row(history: &[EventEnvelope]) -> Option<InvoiceRow> {
    let mut row = history.iter().find_map(|e| match &e.event {
        Event::InvoiceIssued {
            invoice,
            details,
            request,
        } => Some(InvoiceRow {
            id: invoice.to_string(),
            created_at: e.at,
            client: details.to.name.clone(),
            money: Money::from(&details.total),
            rail: details.total.asset.rail,
            url: request.url.clone(),
            state: "open",
            received: None,
            reason: None,
        }),
        _ => None,
    })?;
    for envelope in history {
        match &envelope.event {
            Event::InvoicePaid { received, .. } => {
                row.state = "paid";
                row.received = Some(Money::from(received));
            }
            Event::InvoiceCancelled { reason, .. } => {
                row.state = "cancelled";
                row.reason = Some(reason.clone());
            }
            _ => {}
        }
    }
    Some(row)
}

async fn invoices(State(api): State<Api>, Query(query): Query<Limit>) -> Reply<Vec<InvoiceRow>> {
    let streams = api
        .app
        .store
        .recent_invoices(limit(&query))
        .await
        .map_err(internal)?;
    Ok(Json(
        streams.iter().filter_map(|h| invoice_row(h)).collect(),
    ))
}

/// One asset's limits, in its major unit.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Limits {
    asset: String,
    max_per_tx: String,
    approval_above: String,
    max_per_day: String,
}

#[derive(Serialize)]
struct PolicyView {
    version: u32,
    limits: Vec<Limits>,
    recipients: usize,
    versions: Vec<PolicyVersion>,
}

/// The day window the bot seeds; the dashboard edits only that window.
const DAY: SignedDuration = SignedDuration::from_hours(24);

fn limits_of(policy: &Policy) -> Vec<Limits> {
    let allowed = policy.rules.iter().find_map(|r| match r {
        Rule::AllowAssets { assets } => Some(assets.as_slice()),
        _ => None,
    });
    let find = |asset: &Asset, pick: fn(&Rule) -> Option<&Amount>| {
        policy
            .rules
            .iter()
            .filter_map(pick)
            .find(|a| &a.asset == asset)
            .map(Amount::major)
            .unwrap_or_default()
    };
    allowed
        .unwrap_or_default()
        .iter()
        .map(|asset| Limits {
            asset: asset.code.as_str().to_owned(),
            max_per_tx: find(asset, |r| match r {
                Rule::MaxPerTx { amount } => Some(amount),
                _ => None,
            }),
            approval_above: find(asset, |r| match r {
                Rule::ApprovalAbove { amount } => Some(amount),
                _ => None,
            }),
            max_per_day: find(asset, |r| match r {
                Rule::MaxPerWindow { amount, window } if *window == DAY => Some(amount),
                _ => None,
            }),
        })
        .collect()
}

async fn policy_view(app: &App) -> Result<PolicyView, Failure> {
    let agent = setup::agent_id(app.owner.0);
    let policy = app
        .store
        .policy(agent)
        .await
        .map_err(internal)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no policy yet"))?;
    let recipients = policy
        .rules
        .iter()
        .find_map(|r| match r {
            Rule::AllowRecipients { addresses } => Some(addresses.len()),
            _ => None,
        })
        .unwrap_or_default();
    Ok(PolicyView {
        version: policy.version,
        limits: limits_of(&policy),
        recipients,
        versions: app.store.policy_versions(agent).await.map_err(internal)?,
    })
}

async fn policy(State(api): State<Api>) -> Reply<PolicyView> {
    Ok(Json(policy_view(&api.app).await?))
}

#[derive(Deserialize)]
struct PolicyEdit {
    /// The version the edit was made against, so two tabs can't overwrite each other.
    version: u32,
    limits: Vec<Limits>,
}

/// Saves the owner's limits as a new policy version (rule 9). Recipients and
/// allowed assets stay as they are; those come from warden.toml.
async fn save_policy(State(api): State<Api>, Json(edit): Json<PolicyEdit>) -> Reply<PolicyView> {
    let app = &api.app;
    let agent = setup::agent_id(app.owner.0);
    let current = app
        .store
        .policy(agent)
        .await
        .map_err(internal)?
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "no policy yet"))?;
    if current.version != edit.version {
        return Err(failure(
            StatusCode::CONFLICT,
            format!(
                "the policy changed meanwhile; now version {}",
                current.version
            ),
        ));
    }
    let rules = edited(&current, &edit.limits).map_err(|e| failure(StatusCode::BAD_REQUEST, e))?;
    if rules == current.rules {
        return Ok(Json(policy_view(app).await?));
    }
    let next = Policy {
        version: current.version + 1,
        rules,
        ..current
    };
    app.store
        .save_policy(&next, app.owner_user, PolicySource::Dashboard)
        .await
        .map_err(internal)?;
    tracing::info!(version = next.version, "policy updated from the dashboard");
    Ok(Json(policy_view(app).await?))
}

/// `current` with each asset's limits replaced. Every allowed asset needs limits.
fn edited(current: &Policy, limits: &[Limits]) -> Result<Vec<Rule>, String> {
    let assets: Vec<Asset> = current
        .rules
        .iter()
        .find_map(|r| match r {
            Rule::AllowAssets { assets } => Some(assets.clone()),
            _ => None,
        })
        .unwrap_or_default();
    // Everything but the per-asset limits stays.
    let mut rules: Vec<Rule> = current
        .rules
        .iter()
        .filter(|r| {
            !matches!(
                r,
                Rule::MaxPerTx { .. } | Rule::ApprovalAbove { .. } | Rule::MaxPerWindow { .. }
            )
        })
        .cloned()
        .collect();
    for asset in &assets {
        let code = asset.code.as_str();
        let given = limits
            .iter()
            .find(|l| l.asset.eq_ignore_ascii_case(code))
            .ok_or_else(|| format!("limits for {code} are missing"))?;
        let parse = |field: &str, text: &str| {
            parse_major(text.trim(), asset.decimals)
                .map(|minor| Amount::new(minor, asset.clone()))
                .map_err(|e| format!("{code} {field}: {e}"))
        };
        let max_per_tx = parse("max per payment", &given.max_per_tx)?;
        let approval_above = parse("approval above", &given.approval_above)?;
        let max_per_day = parse("max per day", &given.max_per_day)?;
        if max_per_tx.minor > max_per_day.minor {
            return Err(format!("{code}: max per payment is above max per day"));
        }
        rules.extend([
            Rule::MaxPerTx { amount: max_per_tx },
            Rule::ApprovalAbove {
                amount: approval_above,
            },
            Rule::MaxPerWindow {
                amount: max_per_day,
                window: DAY,
            },
        ]);
    }
    if let Some(unknown) = limits.iter().find(|l| {
        !assets
            .iter()
            .any(|a| l.asset.eq_ignore_ascii_case(a.code.as_str()))
    }) {
        return Err(format!("{} isn't an allowed asset", unknown.asset));
    }
    Ok(rules)
}

#[derive(Deserialize)]
struct PauseRequest {
    paused: bool,
}

/// The kill switch, as /pause and /resume in the chat.
async fn pause(State(api): State<Api>, Json(req): Json<PauseRequest>) -> Reply<Overview> {
    if req.paused {
        api.app.pause.pause();
    } else {
        api.app.pause.resume();
    }
    tracing::info!(paused = req.paused, "pause switched from the dashboard");
    overview(State(api)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use warden_domain::{AgentId, PolicyId};

    fn sol() -> Asset {
        warden_rail_sol::sol()
    }

    fn policy() -> Policy {
        let amount = |minor| Amount::new(minor, sol());
        Policy {
            id: PolicyId::new(Uuid::nil()),
            version: 3,
            agent: AgentId::new(Uuid::nil()),
            rules: vec![
                Rule::AllowAssets {
                    assets: vec![sol()],
                },
                Rule::AllowRecipients { addresses: vec![] },
                Rule::MaxPerTx {
                    amount: amount(1_000_000_000),
                },
                Rule::ApprovalAbove {
                    amount: amount(50_000_000),
                },
                Rule::MaxPerWindow {
                    amount: amount(2_000_000_000),
                    window: DAY,
                },
            ],
        }
    }

    fn limits(tx: &str, above: &str, day: &str) -> Vec<Limits> {
        vec![Limits {
            asset: "SOL".into(),
            max_per_tx: tx.into(),
            approval_above: above.into(),
            max_per_day: day.into(),
        }]
    }

    #[test]
    fn limits_read_back_in_major_units() {
        assert_eq!(limits_of(&policy()), limits("1", "0.05", "2"));
    }

    #[test]
    fn an_edit_replaces_only_the_limits() {
        let rules = edited(&policy(), &limits("0.5", "0.1", "3")).unwrap_or_default();
        let next = Policy { rules, ..policy() };
        assert_eq!(limits_of(&next), limits("0.5", "0.1", "3"));
        assert!(matches!(next.rules[0], Rule::AllowAssets { .. }));
        assert!(matches!(next.rules[1], Rule::AllowRecipients { .. }));
        assert_eq!(next.rules.len(), 5);
    }

    #[test]
    fn bad_edits_are_refused() {
        assert!(edited(&policy(), &[]).is_err());
        assert!(edited(&policy(), &limits("0.1234567891", "0.1", "3")).is_err());
        assert!(edited(&policy(), &limits("abc", "0.1", "3")).is_err());
        assert!(edited(&policy(), &limits("5", "0.1", "3")).is_err());
        let mut extra = limits("1", "0.1", "3");
        extra.extend(limits("1", "0.1", "3").into_iter().map(|l| Limits {
            asset: "DOGE".into(),
            ..l
        }));
        assert!(edited(&policy(), &extra).is_err());
    }

    #[test]
    fn tokens_compare_whole() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"abcd"));
        assert!(!same(b"", b"a"));
    }
}
