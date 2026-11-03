//! Who gets paid, addressed in the rail's native format.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Address {
    SolPubkey(String),
    Email(String),
    NearAccount(String),
    CashAddr(String),
    ConcordiumAccount(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    /// Display name, e.g. "Acme Ltd".
    pub label: String,
    pub address: Address,
}
