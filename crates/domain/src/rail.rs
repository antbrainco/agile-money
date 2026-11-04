//! Rail-facing values: what counterparties see and the native dedupe references (rule 10).

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{
    BchTxid, ConcordiumTxHash, IdempotencyKey, IntentId, NearTxHash, PaypalOrderId, SolAddress,
    SolSignature,
};

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

/// Each rail's native dedupe handle. The types check encoding and length
/// without the core importing a chain SDK.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RailRef {
    Sol {
        /// The Solana Pay reference key added to the transfer.
        reference: SolAddress,
        signature: Option<SolSignature>,
    },
    Paypal {
        /// The checkout order for an invoice, or the payout batch for a payment sent.
        order_id: PaypalOrderId,
        /// Sent as `PayPal-Request-Id`, so PayPal dedupes retries too.
        request_id: IdempotencyKey,
    },
    Near {
        tx_hash: Option<NearTxHash>,
        nonce: u64,
    },
    Bch {
        txid: Option<BchTxid>,
    },
    Concordium {
        tx_hash: Option<ConcordiumTxHash>,
    },
}
