//! NEAR rail; caps are re-checked onchain by the guardrails contract.
//! Will implement [`warden_domain::ports::Rail`]; live network tests go behind the `live` feature.

use warden_domain::RailId;

pub const RAIL_ID: RailId = RailId::Near;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("rail misconfigured: {0}")]
    Config(String),
}

/// Placeholder until the rail lands; see the module table in the spec (../docs/warden-spec.md beside this repo).
#[derive(Debug, Default)]
pub struct NearRail;
