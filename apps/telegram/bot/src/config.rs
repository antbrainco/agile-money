//! Layered config: `warden.toml`, then `WARDEN_*` env vars (`__` nests), then
//! the conventional `DATABASE_URL` and `TELOXIDE_TOKEN`.

use figment::Figment;
use figment::providers::{Env, Format, Toml};
use secrecy::SecretString;
use serde::Deserialize;
use warden_agent_core::LlmSettings;
use warden_agent_core::tools::parse_major;
use warden_domain::{Amount, SolAddress};
use warden_executor::ExecConfig;
use warden_rail_sol::Cluster;
use warden_store::DbConfig;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub network: Network,
    pub database: DbConfig,
    pub telegram: TelegramConfig,
    pub agent: AgentConfig,
    #[serde(default)]
    pub executor: ExecConfig,
    pub rail: RailConfig,
    #[serde(default)]
    pub contacts: Vec<Contact>,
    pub policy: PolicyConfig,
}

/// The network the owner means to use. Checked against the RPC's genesis hash at startup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    #[default]
    Devnet,
    Testnet,
    Mainnet,
}

#[derive(Debug, Deserialize)]
pub struct TelegramConfig {
    pub token: SecretString,
    /// The only chat Tilly answers. Send /start once to learn yours.
    pub owner_chat_id: i64,
}

/// `[agent]`: the loop's step limit plus the model client's settings
/// (provider, model, endpoint or gateway, and per-provider sections).
#[derive(Debug, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "step_limit")]
    pub step_limit: u32,
    #[serde(flatten)]
    pub llm: LlmSettings,
}

fn step_limit() -> u32 {
    warden_domain::STEP_LIMIT
}

#[derive(Debug, Deserialize)]
pub struct RailConfig {
    pub sol: SolSection,
}

#[derive(Debug, Deserialize)]
pub struct SolSection {
    #[serde(default)]
    pub mainnet: bool,
    #[serde(flatten)]
    pub conn: warden_rail_sol::SolConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Contact {
    pub label: String,
    pub address: SolAddress,
}

/// The owner's spending rules, in SOL. Edited by a human, never by the agent (rule 9).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PolicyConfig {
    pub max_per_tx: SolAmount,
    pub approval_above: SolAmount,
    pub max_per_day: SolAmount,
}

/// A SOL amount written in major units, `"0.05"` or `2`. Floats are refused:
/// `0.1` in TOML is already a rounded binary number (rule 4).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "RawAmount")]
pub struct SolAmount(pub Amount);

#[derive(Deserialize)]
#[serde(untagged)]
enum RawAmount {
    Text(String),
    Whole(u64),
}

impl TryFrom<RawAmount> for SolAmount {
    type Error = String;
    fn try_from(raw: RawAmount) -> Result<Self, String> {
        let sol = warden_rail_sol::sol();
        let text = match raw {
            RawAmount::Text(text) => text,
            RawAmount::Whole(n) => n.to_string(),
        };
        let minor = parse_major(&text, sol.decimals).map_err(|e| e.to_string())?;
        Ok(Self(Amount::new(minor, sol)))
    }
}

fn serves(network: Network, cluster: Cluster) -> bool {
    matches!(
        (network, cluster),
        (Network::Devnet, Cluster::Devnet)
            | (Network::Testnet, Cluster::Testnet)
            | (Network::Mainnet, Cluster::Mainnet)
            | (Network::Devnet | Network::Testnet, Cluster::Other)
    )
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        // A missing .env is fine; real env vars take precedence over it.
        let _ = dotenvy::dotenv();
        let config: Self = Figment::new()
            .merge(Toml::file("warden.toml"))
            .merge(Env::prefixed("WARDEN_").split("__"))
            .merge(
                Env::raw()
                    .only(&["DATABASE_URL"])
                    .map(|_| "database.url".into()),
            )
            .merge(
                Env::raw()
                    .only(&["TELOXIDE_TOKEN"])
                    .map(|_| "telegram.token".into()),
            )
            .extract()?;
        Self::finish(config)
    }

    fn finish(mut config: Self) -> anyhow::Result<Self> {
        // Picks the provider when `agent.provider` is unset, and fills its key
        // from the provider's own variable, e.g. DEEPSEEK_API_KEY.
        let found = config.agent.llm.detect(|var| std::env::var(var).ok())?;
        tracing::info!(
            active = ?found.active,
            source = ?found.source,
            available = ?found.available,
            "model provider"
        );
        // Rule 5: real money needs both the global network and the rail's own opt-in.
        if config.network == Network::Mainnet && !config.rail.sol.mainnet {
            anyhow::bail!("network = \"mainnet\" also needs rail.sol.mainnet = true");
        }
        Ok(config)
    }

    /// The RPC must serve the configured network. A local validator passes on
    /// devnet or testnet, never on mainnet.
    pub fn check_cluster(&self, cluster: Cluster) -> anyhow::Result<()> {
        if !serves(self.network, cluster) {
            anyhow::bail!(
                "rail.sol.rpc_url serves {cluster:?}, but network is {:?}",
                self.network
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amount(value: &str) -> Option<SolAmount> {
        #[derive(Deserialize)]
        struct One {
            a: SolAmount,
        }
        Figment::from(Toml::string(&format!("a = {value}")))
            .extract::<One>()
            .ok()
            .map(|o| o.a)
    }

    #[test]
    fn amounts_parse_exactly_and_refuse_floats() {
        assert_eq!(amount("\"0.05\"").unwrap().0.minor, 50_000_000);
        assert_eq!(amount("2").unwrap().0.minor, 2_000_000_000);
        assert!(amount("0.05").is_none(), "float");
        assert!(amount("\"0.0000000001\"").is_none(), "below a lamport");
    }

    #[test]
    fn agent_section_carries_provider_settings() {
        let toml = r#"
            step_limit = 5
            provider = "claude"
            model = "claude-sonnet-5-5"
            max_tokens = 2048
            [gateway]
            account_id = "acct"
            [anthropic]
            thinking_budget = 1024
            betas = ["x"]
            [http]
            max_retries = 4
            [extra_body]
            metadata = { user_id = "tilly" }
        "#;
        let agent: AgentConfig = Figment::from(Toml::string(toml)).extract().unwrap();
        assert_eq!(agent.step_limit, 5);
        let llm = agent.llm;
        assert_eq!(llm.anthropic.thinking_budget, Some(1024));
        assert_eq!(llm.anthropic.version, "2023-06-01");
        assert_eq!(llm.http.max_retries, 4);
        assert_eq!(llm.http.retry_base_ms, 500, "defaults fill the rest");
        assert!(llm.stream);
        assert_eq!(llm.extra_body["metadata"]["user_id"], "tilly");
        assert_eq!(
            llm.base_url().unwrap().as_str(),
            "https://gateway.ai.cloudflare.com/v1/acct/default/anthropic/"
        );
    }

    #[test]
    fn rpc_must_serve_the_configured_network() {
        assert!(serves(Network::Devnet, Cluster::Devnet));
        assert!(serves(Network::Testnet, Cluster::Other));
        assert!(!serves(Network::Devnet, Cluster::Mainnet));
        assert!(!serves(Network::Mainnet, Cluster::Devnet));
        assert!(!serves(Network::Mainnet, Cluster::Other));
    }
}
