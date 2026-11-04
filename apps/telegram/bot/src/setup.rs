//! Identities and the owner's policy, derived from config.

use jiff::SignedDuration;
use uuid::Uuid;
use warden_agent_core::Briefing;
use warden_domain::ports::ReadModel;
use warden_domain::{Address, AgentId, Policy, PolicyId, Recipient, Rule, UserId};
use warden_store::PgStore;

use crate::config::{AssetPolicy, Config, Contact, Network};

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
pub fn briefing(config: &Config, assets: &[AssetPolicy]) -> Briefing {
    let network = match config.network {
        Network::Devnet => "Solana devnet",
        Network::Testnet => "Solana testnet",
        Network::Mainnet => "Solana mainnet",
    };
    let mut limits = vec!["Only saved contacts can be paid; any other address is denied.".into()];
    for a in assets {
        limits.push(format!(
            "{}: payments above {} need the owner's approval; no single payment may exceed {}; \
             no more than {} in total per rolling 24 hours.",
            a.asset.code, a.approval_above, a.max_per_tx, a.max_per_day
        ));
    }
    Briefing {
        network: network.into(),
        assets: assets.iter().map(|a| a.asset.code.to_string()).collect(),
        requests: config
            .rail
            .paypal
            .as_ref()
            .map(|_| vec![warden_rail_paypal::usd().code.to_string()])
            .unwrap_or_default(),
        limits,
    }
}

fn rules(config: &Config, assets: &[AssetPolicy]) -> Vec<Rule> {
    let mut rules = vec![
        Rule::AllowAssets {
            assets: assets.iter().map(|a| a.asset.clone()).collect(),
        },
        Rule::AllowRecipients {
            addresses: recipients(&config.contacts)
                .into_iter()
                .map(|r| r.address)
                .collect(),
        },
    ];
    // Each limit only applies to its own asset; AllowAssets keeps out the rest.
    for a in assets {
        rules.extend([
            Rule::MaxPerTx {
                amount: a.max_per_tx.clone(),
            },
            Rule::ApprovalAbove {
                amount: a.approval_above.clone(),
            },
            Rule::MaxPerWindow {
                amount: a.max_per_day.clone(),
                window: SignedDuration::from_hours(24),
            },
        ]);
    }
    rules
}

/// Stores the configured policy as a new version when it differs from the latest.
pub async fn seed_policy(
    store: &PgStore,
    config: &Config,
    assets: &[AssetPolicy],
) -> anyhow::Result<Policy> {
    let chat = config.telegram.owner_chat_id;
    let agent = agent_id(chat);
    let rules = rules(config, assets);
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
