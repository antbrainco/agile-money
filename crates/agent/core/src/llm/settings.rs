//! `[agent]` model settings: which provider, where to reach it, and the few
//! knobs each API has of its own. Keys and gateway tokens come from env.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;

use super::anthropic::ANTHROPIC_VERSION;
use super::{AnthropicMessages, ChatCompletions, Flavor, LlmClient, Wire, http, secret_header};
use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Deepseek,
    Openai,
    Claude,
    /// Any Chat Completions endpoint; needs `base_url` (or `gateway`) and `model`.
    Compatible,
}

impl Provider {
    /// Detection order when `provider` is unset and several keys are present.
    pub const DETECT_ORDER: [Self; 3] = [Self::Deepseek, Self::Claude, Self::Openai];

    pub fn default_model(self) -> Option<&'static str> {
        match self {
            Self::Deepseek => Some("deepseek-flash"),
            Self::Openai => Some("gpt-5.5"),
            Self::Claude => Some("claude-sonnet-5-5"),
            Self::Compatible => None,
        }
    }

    /// With a trailing slash, so wire paths join under it.
    pub fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::Deepseek => Some("https://api.deepseek.com/"),
            Self::Openai => Some("https://api.openai.com/v1/"),
            Self::Claude => Some("https://api.anthropic.com/"),
            Self::Compatible => None,
        }
    }

    /// The provider's usual key variable, read when `api_key` is unset.
    pub fn key_var(self) -> Option<&'static str> {
        match self {
            Self::Deepseek => Some("DEEPSEEK_API_KEY"),
            Self::Openai => Some("OPENAI_API_KEY"),
            Self::Claude => Some("ANTHROPIC_API_KEY"),
            Self::Compatible => None,
        }
    }

    /// Cloudflare AI Gateway's path segment for this provider.
    fn gateway_path(self) -> &'static str {
        match self {
            Self::Deepseek => "deepseek",
            Self::Openai => "openai",
            Self::Claude => "anthropic",
            Self::Compatible => "compat",
        }
    }
}

/// `[agent.gateway]`: route through Cloudflare AI Gateway.
#[derive(Debug, Clone, Deserialize)]
pub struct GatewaySettings {
    pub account_id: String,
    #[serde(default = "default_gateway")]
    pub gateway_id: String,
    /// For an authenticated gateway, sent as `cf-aig-authorization`. From env:
    /// `WARDEN_AGENT__GATEWAY__TOKEN` or `CF_AIG_TOKEN`.
    pub token: Option<SecretString>,
}

fn default_gateway() -> String {
    "default".into()
}

impl GatewaySettings {
    fn base_url(&self, provider: Provider) -> Result<Url, Error> {
        for (field, value) in [
            ("account_id", &self.account_id),
            ("gateway_id", &self.gateway_id),
        ] {
            if value.is_empty()
                || !value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(Error::Invalid(format!(
                    "agent.gateway.{field} is not an id"
                )));
            }
        }
        url(&format!(
            "https://gateway.ai.cloudflare.com/v1/{}/{}/{}/",
            self.account_id,
            self.gateway_id,
            provider.gateway_path()
        ))
    }
}

/// `[agent.openai]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OpenAiSettings {
    pub organization: Option<String>,
    pub project: Option<String>,
    /// Reasoning models only: `minimal`, `low`, `medium` or `high`.
    pub reasoning_effort: Option<String>,
}

/// `[agent.anthropic]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AnthropicSettings {
    /// The `anthropic-version` header.
    pub version: String,
    /// `anthropic-beta` features, e.g. `token-efficient-tools-2025-02-19`.
    pub betas: Vec<String>,
    /// Extended thinking. Must be below `max_tokens`.
    pub thinking_budget: Option<u32>,
}

impl Default for AnthropicSettings {
    fn default() -> Self {
        Self {
            version: ANTHROPIC_VERSION.into(),
            betas: Vec::new(),
            thinking_budget: None,
        }
    }
}

/// Why the active provider was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `agent.provider` names it.
    Configured,
    /// Unset, and this provider's key variable was the first one found.
    KeyVar(&'static str),
    /// Unset, but `agent.base_url` points at an OpenAI-compatible endpoint.
    BaseUrl,
}

/// What [`LlmSettings::detect`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub active: Provider,
    pub source: Source,
    /// Every provider that could run with the env as it is, in detection order.
    pub available: Vec<Provider>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmSettings {
    /// Unset: detected from which key variable is present; see [`LlmSettings::detect`].
    pub provider: Option<Provider>,
    /// The provider's model id; defaults per provider. Through the gateway's
    /// `compat` endpoint, prefix it with the provider, e.g. `deepseek/deepseek-flash`.
    pub model: Option<String>,
    /// Overrides the provider's endpoint. Exclusive with `gateway`.
    pub base_url: Option<Url>,
    /// From env only. Optional through a gateway that stores provider keys.
    pub api_key: Option<SecretString>,
    /// Stream replies over SSE. Off for endpoints that cannot.
    #[serde(default = "yes")]
    pub stream: bool,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    #[serde(default)]
    pub http: http::HttpSettings,
    pub gateway: Option<GatewaySettings>,
    /// Extra, non-secret headers on every call.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Merged into every request body, for parameters not modelled here.
    #[serde(default)]
    pub extra_body: Map<String, Value>,
    #[serde(default)]
    pub openai: OpenAiSettings,
    #[serde(default)]
    pub anthropic: AnthropicSettings,
}

fn yes() -> bool {
    true
}

fn url(text: &str) -> Result<Url, Error> {
    Url::parse(text).map_err(|e| Error::Invalid(format!("bad url {text}: {e}")))
}

fn header(name: &str, value: &str) -> Result<(HeaderName, HeaderValue), Error> {
    let bad = || Error::Invalid(format!("bad header {name}"));
    Ok((
        HeaderName::from_bytes(name.as_bytes()).map_err(|_| bad())?,
        HeaderValue::from_str(value).map_err(|_| bad())?,
    ))
}

impl LlmSettings {
    pub fn new(provider: Provider) -> Self {
        Self {
            provider: Some(provider),
            model: None,
            base_url: None,
            api_key: None,
            stream: true,
            max_tokens: None,
            temperature: None,
            http: http::HttpSettings::default(),
            gateway: None,
            headers: BTreeMap::new(),
            extra_body: Map::new(),
            openai: OpenAiSettings::default(),
            anthropic: AnthropicSettings::default(),
        }
    }

    /// Picks the active provider and fills `api_key` from its variable. An
    /// explicit `provider` always wins. Otherwise a `base_url` means
    /// `compatible`, else the first of [`Provider::DETECT_ORDER`] whose key
    /// variable is set. `env` reads a variable, e.g. `|k| std::env::var(k).ok()`.
    pub fn detect(&mut self, env: impl Fn(&str) -> Option<String>) -> Result<Detection, Error> {
        let env = |var: &str| env(var).filter(|v| !v.trim().is_empty());
        let mut available: Vec<Provider> = Provider::DETECT_ORDER
            .into_iter()
            .filter(|p| p.key_var().and_then(env).is_some())
            .collect();
        if self.base_url.is_some() {
            available.push(Provider::Compatible);
        }
        let (active, source) = match (self.provider, self.base_url.is_some(), available.first()) {
            (Some(provider), _, _) => (provider, Source::Configured),
            (None, true, _) => (Provider::Compatible, Source::BaseUrl),
            (None, false, Some(&provider)) => (
                provider,
                Source::KeyVar(provider.key_var().unwrap_or_default()),
            ),
            (None, false, None) => {
                let hint = if self.api_key.is_some() || self.gateway.is_some() {
                    "a key or gateway alone does not say which API; set agent.provider"
                } else {
                    "set DEEPSEEK_API_KEY, ANTHROPIC_API_KEY or OPENAI_API_KEY, or agent.provider"
                };
                return Err(Error::Invalid(format!("no model provider found: {hint}")));
            }
        };
        if self.api_key.is_none() {
            self.api_key = active.key_var().and_then(env).map(SecretString::from);
        }
        if self.api_key.is_some() && !available.contains(&active) {
            available.insert(0, active);
        }
        self.provider = Some(active);
        Ok(Detection {
            active,
            source,
            available,
        })
    }

    /// The active provider. Unset until [`Self::detect`] runs or `provider` is configured.
    pub fn provider(&self) -> Result<Provider, Error> {
        self.provider
            .ok_or_else(|| Error::Invalid("no model provider: set agent.provider".into()))
    }

    pub fn model(&self) -> Result<&str, Error> {
        self.model
            .as_deref()
            .or(self.provider()?.default_model())
            .ok_or_else(|| Error::Invalid("agent.model is required for this provider".into()))
    }

    pub fn base_url(&self) -> Result<Url, Error> {
        let mut base = match (&self.base_url, &self.gateway) {
            (Some(_), Some(_)) => {
                return Err(Error::Invalid(
                    "set agent.base_url or agent.gateway, not both".into(),
                ));
            }
            (Some(base), None) => base.clone(),
            (None, Some(gateway)) => gateway.base_url(self.provider()?)?,
            (None, None) => url(self.provider()?.default_base_url().ok_or_else(|| {
                Error::Invalid("agent.base_url is required for this provider".into())
            })?)?,
        };
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        Ok(base)
    }

    pub fn wire(&self) -> Result<Arc<dyn Wire>, Error> {
        Ok(match self.provider()? {
            Provider::Claude => {
                let wire = AnthropicMessages::new(&self.anthropic.version, &self.anthropic.betas)?;
                Arc::new(match self.anthropic.thinking_budget {
                    Some(budget) => wire.with_thinking(budget),
                    None => wire,
                })
            }
            Provider::Openai => {
                let mut wire = ChatCompletions::new(Flavor::OpenAi);
                if let Some(effort) = &self.openai.reasoning_effort {
                    wire = wire.with_reasoning_effort(effort);
                }
                for (name, value) in [
                    ("openai-organization", &self.openai.organization),
                    ("openai-project", &self.openai.project),
                ] {
                    if let Some(value) = value {
                        let (name, value) = header(name, value)?;
                        wire = wire.with_header(name, value);
                    }
                }
                Arc::new(wire)
            }
            Provider::Deepseek => Arc::new(ChatCompletions::new(Flavor::DeepSeek)),
            Provider::Compatible => Arc::new(ChatCompletions::new(Flavor::Compatible)),
        })
    }

    /// Every header the stack adds: the wire's own, the key, the gateway token, then `headers`.
    fn stack_headers(&self, wire: &dyn Wire) -> Result<HeaderMap, Error> {
        let mut headers = wire.headers();
        match &self.api_key {
            Some(key) => {
                let (name, value) = wire.auth(key)?;
                headers.insert(name, value);
            }
            None if self.gateway.is_some() || self.provider()? == Provider::Compatible => {}
            None => {
                return Err(Error::Invalid(format!(
                    "no API key: set {} or WARDEN_AGENT__API_KEY",
                    self.provider()?.key_var().unwrap_or("agent.api_key")
                )));
            }
        }
        if let Some(token) = self.gateway.as_ref().and_then(|g| g.token.as_ref()) {
            let (name, value) = secret_header(
                HeaderName::from_static("cf-aig-authorization"),
                &format!("Bearer {}", token.expose_secret()),
            )?;
            headers.insert(name, value);
        }
        for (name, value) in &self.headers {
            let (name, value) = header(name, value)?;
            headers.insert(name, value);
        }
        Ok(headers)
    }

    /// The client over the default round-tripper stack.
    pub fn client(&self) -> Result<LlmClient, Error> {
        let wire = self.wire()?;
        let transport = http::stack(&self.http, self.stack_headers(wire.as_ref())?)?;
        self.client_with(transport, wire)
    }

    /// The client over a round tripper of your own, e.g. one with extra layers.
    /// Auth headers are not added: put them in the stack.
    pub fn client_with(
        &self,
        transport: Arc<dyn http::RoundTripper>,
        wire: Arc<dyn Wire>,
    ) -> Result<LlmClient, Error> {
        if self.provider()? == Provider::Compatible
            && self.gateway.is_some()
            && !self.model()?.contains('/')
        {
            tracing::warn!("the gateway's compat endpoint wants models as provider/model");
        }
        Ok(
            LlmClient::new(transport, wire, self.base_url()?, self.model()?)
                .with_streaming(self.stream)
                .with_max_tokens(self.max_tokens)
                .with_temperature(self.temperature)
                .with_extra_body(self.extra_body.clone())
                .with_request_timeout(Duration::from_millis(self.http.request_timeout_ms)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gateway(token: Option<&str>) -> GatewaySettings {
        GatewaySettings {
            account_id: "acct".into(),
            gateway_id: "tilly".into(),
            token: token.map(|t| SecretString::from(t.to_owned())),
        }
    }

    #[test]
    fn gateway_urls_follow_the_provider() {
        let mut settings = LlmSettings::new(Provider::Deepseek);
        settings.gateway = Some(gateway(None));
        assert_eq!(
            settings.base_url().unwrap().as_str(),
            "https://gateway.ai.cloudflare.com/v1/acct/tilly/deepseek/"
        );
        settings.provider = Some(Provider::Claude);
        let url = settings.base_url().unwrap().join("v1/messages").unwrap();
        assert_eq!(
            url.as_str(),
            "https://gateway.ai.cloudflare.com/v1/acct/tilly/anthropic/v1/messages"
        );
        settings.provider = Some(Provider::Compatible);
        assert!(settings.base_url().unwrap().path().ends_with("/compat/"));
    }

    #[test]
    fn gateway_ids_cannot_escape_the_path() {
        let mut settings = LlmSettings::new(Provider::Openai);
        let mut bad = gateway(None);
        bad.gateway_id = "../x".into();
        settings.gateway = Some(bad);
        assert!(settings.base_url().is_err());
    }

    #[test]
    fn base_url_and_gateway_are_exclusive() {
        let mut settings = LlmSettings::new(Provider::Openai);
        settings.base_url = Some(url("https://example.test/v1").unwrap());
        assert_eq!(
            settings.base_url().unwrap().as_str(),
            "https://example.test/v1/"
        );
        settings.gateway = Some(gateway(None));
        assert!(settings.base_url().is_err());
    }

    #[test]
    fn a_key_is_needed_unless_a_gateway_holds_it() {
        let mut settings = LlmSettings::new(Provider::Claude);
        let wire = settings.wire().unwrap();
        let err = settings.stack_headers(wire.as_ref()).unwrap_err();
        assert!(err.to_string().contains("ANTHROPIC_API_KEY"));

        settings.gateway = Some(gateway(Some("cf")));
        let headers = settings.stack_headers(wire.as_ref()).unwrap();
        assert!(headers["cf-aig-authorization"].is_sensitive());
        assert_eq!(headers["anthropic-version"], ANTHROPIC_VERSION);
        assert!(!headers.contains_key("x-api-key"));

        settings.api_key = Some(SecretString::from("sk".to_owned()));
        let headers = settings.stack_headers(wire.as_ref()).unwrap();
        assert!(headers["x-api-key"].is_sensitive());
    }

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    fn unset() -> LlmSettings {
        let mut settings = LlmSettings::new(Provider::Openai);
        settings.provider = None;
        settings
    }

    #[test]
    fn detects_the_first_provider_with_a_key() {
        let mut settings = unset();
        let found = settings
            .detect(env(&[
                ("OPENAI_API_KEY", "sk-o"),
                ("ANTHROPIC_API_KEY", "sk-a"),
                ("DEEPSEEK_API_KEY", " "),
            ]))
            .unwrap();
        assert_eq!(found.active, Provider::Claude);
        assert_eq!(found.source, Source::KeyVar("ANTHROPIC_API_KEY"));
        assert_eq!(found.available, [Provider::Claude, Provider::Openai]);
        assert_eq!(settings.api_key.unwrap().expose_secret(), "sk-a");
    }

    #[test]
    fn configured_provider_wins_over_keys() {
        let mut settings = unset();
        settings.provider = Some(Provider::Openai);
        let found = settings
            .detect(env(&[
                ("DEEPSEEK_API_KEY", "sk-d"),
                ("OPENAI_API_KEY", "sk-o"),
            ]))
            .unwrap();
        assert_eq!(
            (found.active, found.source),
            (Provider::Openai, Source::Configured)
        );
        assert_eq!(settings.api_key.unwrap().expose_secret(), "sk-o");
    }

    #[test]
    fn base_url_alone_means_compatible() {
        let mut settings = unset();
        settings.base_url = Some(url("http://localhost:11434/v1").unwrap());
        let found = settings
            .detect(env(&[("DEEPSEEK_API_KEY", "sk-d")]))
            .unwrap();
        assert_eq!(
            (found.active, found.source),
            (Provider::Compatible, Source::BaseUrl)
        );
        assert!(
            settings.api_key.is_none(),
            "DeepSeek's key is not sent elsewhere"
        );
    }

    #[test]
    fn nothing_to_detect_says_what_to_set() {
        let err = unset().detect(env(&[])).unwrap_err();
        assert!(err.to_string().contains("DEEPSEEK_API_KEY"));

        let mut keyed = unset();
        keyed.api_key = Some(SecretString::from("sk".to_owned()));
        let err = keyed.detect(env(&[])).unwrap_err();
        assert!(err.to_string().contains("set agent.provider"));
    }
}
