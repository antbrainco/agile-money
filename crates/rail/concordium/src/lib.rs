//! Concordium rail with identity proofs and x402 payments.
//! Will implement [`agile_money_domain::ports::Rail`]; live network tests go behind the `live` feature.

use agile_money_domain::RailId;

pub const RAIL_ID: RailId = RailId::Concordium;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("rail misconfigured: {0}")]
    Config(String),
}

/// Placeholder until the rail lands; see the module table in the spec (../docs/agile-money-spec.md beside this repo).
#[derive(Debug, Default)]
pub struct ConcordiumRail;
