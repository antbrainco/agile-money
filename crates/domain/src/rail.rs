//! Rail-facing values: what counterparties see and the native dedupe references (rule 10).

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::IntentId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentRequest {
    pub url: String,
    pub qr_png: Option<Vec<u8>>,
    pub rail_ref: RailRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    pub intent: IntentId,
    pub rail_ref: RailRef,
    pub at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Settlement {
    Pending,
    Confirmed { at: Timestamp },
    Failed { reason: String },
}

/// Chain values stay base58/hex strings so the core never imports a chain SDK.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RailRef {
    Sol {
        reference: String,
        signature: Option<String>,
    },
    Paypal {
        order_id: String,
        request_id: String,
    },
    Near {
        tx_hash: Option<String>,
        nonce: u64,
    },
    Bch {
        txid: Option<String>,
    },
    Concordium {
        tx_hash: Option<String>,
    },
}
