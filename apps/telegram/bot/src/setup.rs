//! Identities and the owner's policy, derived from config.

use jiff::SignedDuration;
use uuid::Uuid;
use warden_agent_core::Briefing;
use warden_domain::ports::ReadModel;
use warden_domain::{Address, AgentId, Policy, PolicyId, Recipient, Rule, UserId};
use warden_store::PgStore;

use crate::config::{Config, Contact, Network};

/// "tilly" in ASCII, so ids derived from a chat are recognisable.
const NAMESPACE: u64 = 0x7469_6c6c_7900_0000;

/// One agent per owner chat, stable across restarts.
pub fn agent_id(chat: i64) -> AgentId {
    AgentId::new(Uuid::from_u64_pair(NAMESPACE | 1, chat as u64))
}

pub fn owner_id(chat: i64) -> UserId {
    UserId::new(Uuid::from_u64_pair(NAMESPACE | 2, chat as u64))
}

fn policy_id(chat: i64) -> PolicyId {
    PolicyId::new(Uuid::from_u64_pair(NAMESPACE | 3, chat as u64))
}

pub fn recipients(contacts: &[Contact]) -> Vec<Recipient> {
    contacts
        .iter()
        .map(|c| Recipient {
            label: c.label.clone(),
            address: Address::SolPubkey(c.address),
        })
        .collect()
}

/// What the agent is told about this deployment. Mirrors `rules`; the policy enforces it.
pub fn briefing(config: &Config) -> Briefing {
    let limits = &config.policy;
    let network = match config.network {
        Network::Devnet => "Solana devnet",
        Network::Testnet => "Solana testnet",
        Network::Mainnet => "Solana mainnet",
    };
    Briefing {
        network: network.into(),
        assets: vec![warden_rail_sol::sol().code.to_string()],
        limits: vec![
            "Only saved contacts can be paid; any other address is denied.".into(),
            format!(
                "Payments above {} need the owner's approval.",
                limits.approval_above.0
            ),
            format!("No single payment may exceed {}.", limits.max_per_tx.0),
            format!(
                "No more than {} in total per rolling 24 hours.",
                limits.max_per_day.0
            ),
        ],
    }
}

fn rules(config: &Config) -> Vec<Rule> {
    let limits = &config.policy;
    vec![
        Rule::AllowAssets {
            assets: vec![warden_rail_sol::sol()],
        },
        Rule::AllowRecipients {
            addresses: recipients(&config.contacts)
                .into_iter()
                .map(|r| r.address)
                .collect(),
        },
        Rule::MaxPerTx {
            amount: limits.max_per_tx.0.clone(),
        },
        Rule::ApprovalAbove {
            amount: limits.approval_above.0.clone(),
        },
        Rule::MaxPerWindow {
            amount: limits.max_per_day.0.clone(),
            window: SignedDuration::from_hours(24),
        },
    ]
}

/// Stores the configured policy as a new version when it differs from the latest.
pub async fn seed_policy(store: &PgStore, config: &Config) -> anyhow::Result<Policy> {
    let chat = config.telegram.owner_chat_id;
    let agent = agent_id(chat);
    let rules = rules(config);
    let current = store.policy(agent).await?;
    if let Some(current) = current.filter(|p| p.rules == rules) {
        return Ok(current);
    }
    let latest = store.policy(agent).await?.map_or(0, |p| p.version);
    let policy = Policy {
        id: policy_id(chat),
        version: latest + 1,
        agent,
        rules,
    };
    store.save_policy(&policy, owner_id(chat)).await?;
    tracing::info!(version = policy.version, "policy updated from config");
    Ok(policy)
}
