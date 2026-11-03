//! Who gets paid, addressed in the rail's native format.

use serde::{Deserialize, Serialize};

use crate::{CashAddr, ConcordiumAccount, EmailAddress, NearAccount, SolAddress};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Address {
    SolPubkey(SolAddress),
    /// A PayPal account's email.
    Email(EmailAddress),
    NearAccount(NearAccount),
    CashAddr(CashAddr),
    ConcordiumAccount(ConcordiumAccount),
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SolPubkey(a) => a.fmt(f),
            Self::Email(a) => a.fmt(f),
            Self::NearAccount(a) => a.fmt(f),
            Self::CashAddr(a) => a.fmt(f),
            Self::ConcordiumAccount(a) => a.fmt(f),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    /// Display name, e.g. "Acme Ltd".
    pub label: String,
    pub address: Address,
}
