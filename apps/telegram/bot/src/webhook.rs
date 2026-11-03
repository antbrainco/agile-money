//! Webhook mode: Telegram posts updates to a public HTTPS URL, which a tunnel or
//! reverse proxy forwards to the bot's local listener.
//!
//! The URL and secret come from config and env, so they are stable across
//! restarts: Telegram keeps queueing updates while the bot is down, and
//! registering again on startup is a no-op unless something changed.

use std::net::SocketAddr;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use teloxide::prelude::*;
use teloxide::types::AllowedUpdate;
use teloxide::update_listeners::{UpdateListener, webhooks};
use url::Url;

/// `[telegram.webhook]`. Env overrides: `TUNNEL_URL` or `WARDEN_TELEGRAM__WEBHOOK__URL`
/// for the URL, `TELEGRAM_WEBHOOK_SECRET` or `WARDEN_TELEGRAM__WEBHOOK__SECRET` for the
/// secret. Webhook mode is on when a URL is set; otherwise the bot long-polls.
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookConfig {
    /// The tunnel's public base URL, e.g. `https://bot.example.com`.
    pub url: Option<Url>,
    /// Appended to `url`, and the route the listener serves.
    #[serde(default = "default_path")]
    pub path: String,
    /// Sent by Telegram as `X-Telegram-Bot-Api-Secret-Token`; requests without it
    /// are refused. 1-256 of `A-Z a-z 0-9 _ -`. From env only.
    pub secret: Option<SecretString>,
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// Discard updates Telegram queued while the bot was down.
    #[serde(default)]
    pub drop_pending_updates: bool,
}

fn default_path() -> String {
    "/telegram".into()
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8443))
}

/// A webhook ready to register: endpoint and secret resolved and checked.
#[derive(Debug)]
pub struct Webhook {
    pub endpoint: Url,
    pub path: String,
    pub listen: SocketAddr,
    secret: SecretString,
    /// The secret was generated for this run, so it changes on every start.
    pub ephemeral_secret: bool,
    drop_pending_updates: bool,
}

/// The updates the bot handles; everything else Telegram need not send.
const ALLOWED: [AllowedUpdate; 2] = [AllowedUpdate::Message, AllowedUpdate::CallbackQuery];

impl Webhook {
    /// `None` when no URL is configured, i.e. polling mode.
    pub fn resolve(config: Option<&WebhookConfig>) -> anyhow::Result<Option<Self>> {
        let Some(config) = config else {
            return Ok(None);
        };
        let Some(base) = &config.url else {
            if config.secret.is_some() {
                tracing::warn!("webhook secret set without a URL; long polling instead");
            }
            return Ok(None);
        };
        let endpoint = endpoint(base, &config.path)?;
        // An empty `TELEGRAM_WEBHOOK_SECRET=` line in .env counts as unset.
        let secret = config
            .secret
            .as_ref()
            .filter(|s| !s.expose_secret().is_empty());
        let (secret, ephemeral_secret) = match secret {
            Some(secret) => {
                check_secret(secret.expose_secret())?;
                (secret.clone(), false)
            }
            None => (generate_secret(), true),
        };
        Ok(Some(Self {
            path: endpoint.path().to_owned(),
            endpoint,
            listen: config.listen,
            secret,
            ephemeral_secret,
            drop_pending_updates: config.drop_pending_updates,
        }))
    }

    /// Points Telegram at this webhook, then reads back what Telegram has stored.
    pub async fn register(&self, bot: &Bot) -> anyhow::Result<()> {
        if self.ephemeral_secret {
            tracing::warn!(
                "no webhook secret configured; using a random one for this run. \
                 Set TELEGRAM_WEBHOOK_SECRET to keep it stable"
            );
        }
        bot.set_webhook(self.endpoint.clone())
            .secret_token(self.secret.expose_secret())
            .allowed_updates(ALLOWED)
            .drop_pending_updates(self.drop_pending_updates)
            .await?;
        let info = bot.get_webhook_info().await?;
        if info.url.as_ref() != Some(&self.endpoint) {
            anyhow::bail!(
                "Telegram stored webhook {:?}, expected {}",
                info.url.as_ref().map(Url::as_str),
                self.endpoint
            );
        }
        tracing::info!(
            url = %self.endpoint,
            pending = info.pending_update_count,
            last_error = info.last_error_message.as_deref(),
            "webhook registered"
        );
        Ok(())
    }

    /// Serves the webhook route on `listen`. The webhook stays registered after
    /// the bot stops, so Telegram queues updates until it is back; polling mode
    /// deletes it on start.
    pub async fn serve(
        &self,
    ) -> anyhow::Result<impl UpdateListener<Err = std::convert::Infallible>> {
        let options = webhooks::Options::new(self.listen, self.endpoint.clone())
            .path(self.path.clone())
            .secret_token(self.secret.expose_secret().to_owned());
        let (mut listener, stop_flag, router) = webhooks::axum_no_setup(options);
        let stop = listener.stop_token();
        let tcp = tokio::net::TcpListener::bind(self.listen)
            .await
            .map_err(|e| anyhow::anyhow!("webhook listener on {}: {e}", self.listen))?;
        tokio::spawn(async move {
            if let Err(e) = axum::serve(tcp, router)
                .with_graceful_shutdown(stop_flag)
                .await
            {
                tracing::error!(error = %e, "webhook server stopped");
                stop.stop();
            }
        });
        Ok(listener)
    }
}

/// `base` joined with `path`. Must be HTTPS, and must not carry a query: Telegram
/// would send it back on every update, and logs would show it.
fn endpoint(base: &Url, path: &str) -> anyhow::Result<Url> {
    if base.scheme() != "https" {
        anyhow::bail!("telegram.webhook.url must be https, got {}", base.scheme());
    }
    if base.query().is_some() || base.fragment().is_some() {
        anyhow::bail!("telegram.webhook.url must not have a query or fragment");
    }
    let mut endpoint = base.clone();
    let joined = format!(
        "{}/{}",
        base.path().trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    endpoint.set_path(&joined);
    Ok(endpoint)
}

fn check_secret(secret: &str) -> anyhow::Result<()> {
    let valid = (1..=256).contains(&secret.len())
        && secret
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid {
        anyhow::bail!("webhook secret must be 1-256 characters of A-Z a-z 0-9 _ -");
    }
    Ok(())
}

fn generate_secret() -> SecretString {
    let hex: String = (0..2)
        .map(|_| uuid::Uuid::new_v4().simple().to_string())
        .collect();
    SecretString::from(hex)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn config(url: Option<&str>, secret: Option<&str>) -> WebhookConfig {
        WebhookConfig {
            url: url.map(|u| u.parse().unwrap()),
            path: default_path(),
            secret: secret.map(SecretString::from),
            listen: default_listen(),
            drop_pending_updates: false,
        }
    }

    #[test]
    fn joins_the_path_onto_the_tunnel_url() {
        let base: Url = "https://bot.example.com".parse().unwrap();
        assert_eq!(
            endpoint(&base, "/telegram").unwrap().as_str(),
            "https://bot.example.com/telegram"
        );
        let nested: Url = "https://example.test/bots/".parse().unwrap();
        assert_eq!(
            endpoint(&nested, "telegram").unwrap().as_str(),
            "https://example.test/bots/telegram"
        );
    }

    #[test]
    fn refuses_plain_http_and_queries() {
        assert!(endpoint(&"http://a.test".parse().unwrap(), "/t").is_err());
        assert!(endpoint(&"https://a.test/?k=1".parse().unwrap(), "/t").is_err());
    }

    #[test]
    fn no_url_means_polling() {
        assert!(Webhook::resolve(None).unwrap().is_none());
        assert!(
            Webhook::resolve(Some(&config(None, Some("s"))))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn configured_secret_is_kept_and_checked() {
        let hook = Webhook::resolve(Some(&config(Some("https://a.test"), Some("abc_DEF-1"))))
            .unwrap()
            .unwrap();
        assert!(!hook.ephemeral_secret);
        assert_eq!(hook.secret.expose_secret(), "abc_DEF-1");
        assert_eq!(hook.path, "/telegram");
        assert!(
            Webhook::resolve(Some(&config(Some("https://a.test"), Some("no spaces")))).is_err()
        );
    }

    #[test]
    fn missing_secret_is_generated() {
        let hook = Webhook::resolve(Some(&config(Some("https://a.test"), None)))
            .unwrap()
            .unwrap();
        assert!(hook.ephemeral_secret);
        check_secret(hook.secret.expose_secret()).unwrap();
    }
}
