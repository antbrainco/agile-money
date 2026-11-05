//! x402: the server half answers paid routes with HTTP 402 and a challenge; the client half
//! turns a challenge into a `PaymentIntent` that goes through policy like any other payment.

use agile_money_domain::{Address, Amount};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("challenge expired")]
    Expired,
    #[error("nonce already used")]
    NonceReused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    pub amount: Amount,
    pub pay_to: Address,
    /// Single-use.
    pub nonce: String,
    pub expires_at: Timestamp,
}
