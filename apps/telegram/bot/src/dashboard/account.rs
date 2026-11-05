//! The owner's profile: wallets that sign in in place of the password, and the
//! Telegram account confirmed as the owner chat. A wallet never skips the
//! authenticator code; linking one needs a code too, and Telegram hears of both.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use teloxide::prelude::*;

use super::{Api, Failure, Reply, check_code, failure, internal};
use crate::profile::{Purpose, short};
use crate::setup;

pub(super) fn routes() -> Router<Api> {
    Router::new()
        .route("/api/wallet/challenge", post(challenge))
        .route("/api/sign-in/wallet", post(sign_in))
        .route("/api/profile", get(profile))
        .route("/api/wallets", post(link_wallet))
        .route("/api/wallets/{address}/remove", post(unlink_wallet))
        .route("/api/telegram/link", post(telegram_link))
}

/// The dashboard's host, set by the Worker, so the wallet shows where it signs.
fn host(headers: &HeaderMap) -> String {
    headers
        .get("x-dashboard-host")
        .and_then(|v| v.to_str().ok())
        .filter(|h| !h.is_empty() && h.len() <= 253)
        .unwrap_or("Agile Money dashboard")
        .to_owned()
}

fn refused(e: impl std::fmt::Display) -> Failure {
    failure(StatusCode::FORBIDDEN, e.to_string())
}

#[derive(Deserialize)]
struct ChallengeRequest {
    address: String,
    purpose: Purpose,
}

#[derive(Serialize)]
struct ChallengeReply {
    message: String,
}

async fn challenge(
    State(api): State<Api>,
    headers: HeaderMap,
    Json(req): Json<ChallengeRequest>,
) -> Reply<ChallengeReply> {
    let app = &api.app;
    let address = req.address.trim();
    if req.purpose == Purpose::SignIn && !linked(&api, address).await? {
        return Err(refused(
            "This wallet isn't linked. Sign in with your password, then link it under Profile.",
        ));
    }
    let message = app
        .profile
        .challenge(req.purpose, address, &host(&headers), Timestamp::now())
        .map_err(|e| failure(StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(ChallengeReply { message }))
}

async fn linked(api: &Api, address: &str) -> Result<bool, Failure> {
    let agent = setup::agent_id(api.app.owner.0);
    let wallets = api.app.store.owner_wallets(agent).await.map_err(internal)?;
    Ok(wallets.iter().any(|w| w.address == address))
}

#[derive(Deserialize)]
struct WalletSignIn {
    address: String,
    signature: String,
    #[serde(default)]
    code: Option<String>,
}

/// In place of the password: a linked wallet's signature over its challenge,
/// then the same code check and Telegram notice as a password sign-in.
async fn sign_in(
    State(api): State<Api>,
    Json(req): Json<WalletSignIn>,
) -> Reply<serde_json::Value> {
    let app = &api.app;
    let address = req.address.trim();
    app.profile
        .verify(
            Purpose::SignIn,
            address,
            req.signature.trim(),
            Timestamp::now(),
        )
        .map_err(|e| failure(StatusCode::UNAUTHORIZED, e.to_string()))?;
    // Checked again: it may have been removed since the challenge.
    if !linked(&api, address).await? {
        return Err(failure(
            StatusCode::UNAUTHORIZED,
            "This wallet isn't linked.",
        ));
    }
    check_code(app, req.code.as_deref())?;
    tracing::info!(wallet = %address, "dashboard signed in with a wallet");
    app.contacts
        .notify(
            format!(
                "Someone signed in to the dashboard with wallet {}. If that wasn't you, send \
                 /pause and remove the wallet under Profile.",
                short(address)
            ),
            None,
        )
        .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Serialize)]
struct Profile {
    telegram: TelegramView,
    wallets: Vec<WalletView>,
    codes: bool,
}

#[derive(Serialize)]
struct TelegramView {
    chat_id: i64,
    /// The bot's username, for t.me links.
    bot: Option<String>,
    name: Option<String>,
    username: Option<String>,
    /// When the owner chat last opened the dashboard's link.
    linked_at: Option<Timestamp>,
}

#[derive(Serialize)]
struct WalletView {
    address: String,
    label: String,
    linked_at: Timestamp,
}

async fn profile(State(api): State<Api>) -> Reply<Profile> {
    let app = &api.app;
    let agent = setup::agent_id(app.owner.0);
    let (wallets, telegram) = tokio::try_join!(
        app.store.owner_wallets(agent),
        app.store.owner_telegram(agent)
    )
    .map_err(internal)?;
    let bot = match api.bot.get_me().await {
        Ok(me) => me.user.username.clone(),
        Err(e) => {
            tracing::debug!(error = %e, "bot name unavailable");
            None
        }
    };
    Ok(Json(Profile {
        telegram: TelegramView {
            chat_id: app.owner.0,
            bot,
            name: telegram.as_ref().map(|t| t.name.clone()),
            username: telegram.as_ref().and_then(|t| t.username.clone()),
            linked_at: telegram.map(|t| t.linked_at),
        },
        wallets: wallets
            .into_iter()
            .map(|w| WalletView {
                address: w.address,
                label: w.label,
                linked_at: w.linked_at,
            })
            .collect(),
        codes: app.contacts.guard().enabled(),
    }))
}

#[derive(Deserialize)]
struct LinkWallet {
    address: String,
    signature: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    code: Option<String>,
}

/// Adds a sign-in wallet: it must sign its challenge, and a new way in needs a
/// code once codes are on.
async fn link_wallet(State(api): State<Api>, Json(req): Json<LinkWallet>) -> Reply<Profile> {
    let app = &api.app;
    let address = req.address.trim();
    let label = match req.label.trim() {
        "" => "Wallet",
        l => l,
    };
    if label.chars().count() > 40 {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Keep the name under 40 characters.",
        ));
    }
    app.profile
        .verify(
            Purpose::Link,
            address,
            req.signature.trim(),
            Timestamp::now(),
        )
        .map_err(refused)?;
    check_code(app, req.code.as_deref())?;
    let agent = setup::agent_id(app.owner.0);
    if !app
        .store
        .link_wallet(agent, address, label)
        .await
        .map_err(internal)?
    {
        return Err(failure(
            StatusCode::CONFLICT,
            format!(
                "You can link up to {} wallets. Remove one first.",
                agile_money_store::MAX_WALLETS
            ),
        ));
    }
    tracing::info!(wallet = %address, "sign-in wallet linked");
    app.contacts
        .notify(
            format!(
                "Wallet {} ({label}) can now sign in to the dashboard. If that wasn't you, send \
                 /pause and remove it under Profile.",
                short(address)
            ),
            None,
        )
        .await;
    profile(State(api)).await
}

/// Removing a way in only tightens, so it needs no code.
async fn unlink_wallet(State(api): State<Api>, Path(address): Path<String>) -> Reply<Profile> {
    let agent = setup::agent_id(api.app.owner.0);
    if !api
        .app
        .store
        .unlink_wallet(agent, &address)
        .await
        .map_err(internal)?
    {
        return Err(failure(StatusCode::NOT_FOUND, "That wallet isn't linked."));
    }
    tracing::info!(wallet = %address, "sign-in wallet removed");
    profile(State(api)).await
}

#[derive(Serialize)]
struct TelegramLink {
    url: String,
}

/// A one-time t.me link. Opened in the owner chat, it records that account on
/// the profile; any other chat gets nothing.
async fn telegram_link(State(api): State<Api>) -> Reply<TelegramLink> {
    let me = api.bot.get_me().await.map_err(internal)?;
    let bot = me
        .user
        .username
        .clone()
        .ok_or_else(|| internal("the bot has no username"))?;
    let token = api.app.profile.telegram_token(Timestamp::now());
    Ok(Json(TelegramLink {
        url: format!("https://t.me/{bot}?start={token}"),
    }))
}
