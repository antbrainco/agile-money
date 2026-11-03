//! The append-only event log's vocabulary. Past tense, one variant per fact.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{
    ApprovalId, Decision, IntentHash, IntentId, InvalidValue, InvoiceId, PaymentRequest, PolicyId,
    Proposal, RailRef, UserId,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Carries the whole intent so the executor can rebuild it from the log alone.
    IntentProposed {
        intent: IntentId,
        hash: IntentHash,
        proposal: Proposal,
    },
    IntentDecided {
        intent: IntentId,
        decision: Decision,
        policy: PolicyId,
        version: u32,
    },
    ApprovalRequested {
        approval: ApprovalId,
        intent: IntentId,
        expires_at: Timestamp,
    },
    ApprovalGranted {
        approval: ApprovalId,
        by: UserId,
    },
    ApprovalRejected {
        approval: ApprovalId,
        by: Option<UserId>,
        expired: bool,
    },
    PaymentSubmitted {
        intent: IntentId,
        rail_ref: RailRef,
    },
    PaymentConfirmed {
        intent: IntentId,
        rail_ref: RailRef,
    },
    PaymentFailed {
        intent: IntentId,
        reason: String,
        retryable: bool,
    },
    /// Written by the executor once retries are exhausted or the failure is not retryable.
    PaymentAbandoned {
        intent: IntentId,
        attempts: u8,
    },
    InvoiceIssued {
        invoice: InvoiceId,
        request: PaymentRequest,
    },
    InvoicePaid {
        invoice: InvoiceId,
        rail_ref: RailRef,
    },
    PolicyChanged {
        policy: PolicyId,
        version: u32,
        by: UserId,
    },
}

impl Event {
    /// The `type` tag, as stored in the `events.type` column.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::IntentProposed { .. } => "intent_proposed",
            Self::IntentDecided { .. } => "intent_decided",
            Self::ApprovalRequested { .. } => "approval_requested",
            Self::ApprovalGranted { .. } => "approval_granted",
            Self::ApprovalRejected { .. } => "approval_rejected",
            Self::PaymentSubmitted { .. } => "payment_submitted",
            Self::PaymentConfirmed { .. } => "payment_confirmed",
            Self::PaymentFailed { .. } => "payment_failed",
            Self::PaymentAbandoned { .. } => "payment_abandoned",
            Self::InvoiceIssued { .. } => "invoice_issued",
            Self::InvoicePaid { .. } => "invoice_paid",
            Self::PolicyChanged { .. } => "policy_changed",
        }
    }
}

/// An aggregate's stream, stored as `intent:<uuid>` and so on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum StreamId {
    Intent(IntentId),
    Invoice(InvoiceId),
    Policy(PolicyId),
}

impl StreamId {
    /// The intent id, if this is an intent stream.
    pub fn as_intent(&self) -> Option<IntentId> {
        match self {
            Self::Intent(id) => Some(*id),
            _ => None,
        }
    }
}

impl std::fmt::Display for StreamId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Intent(id) => write!(f, "intent:{id}"),
            Self::Invoice(id) => write!(f, "invoice:{id}"),
            Self::Policy(id) => write!(f, "policy:{id}"),
        }
    }
}

impl std::str::FromStr for StreamId {
    type Err = InvalidValue;
    fn from_str(name: &str) -> Result<Self, InvalidValue> {
        let bad = |reason| InvalidValue {
            kind: "StreamId",
            reason,
        };
        let (kind, id) = name.split_once(':').ok_or(bad("missing kind"))?;
        let id = uuid::Uuid::parse_str(id).map_err(|_| bad("not a uuid"))?;
        match kind {
            "intent" => Ok(Self::Intent(IntentId::new(id))),
            "invoice" => Ok(Self::Invoice(InvoiceId::new(id))),
            "policy" => Ok(Self::Policy(PolicyId::new(id))),
            _ => Err(bad("unknown kind")),
        }
    }
}

impl TryFrom<String> for StreamId {
    type Error = InvalidValue;
    fn try_from(name: String) -> Result<Self, InvalidValue> {
        name.parse()
    }
}

impl From<StreamId> for String {
    fn from(stream: StreamId) -> String {
        stream.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Global and monotonically increasing.
    pub seq: i64,
    pub stream: StreamId,
    pub at: Timestamp,
    pub event: Event,
}
