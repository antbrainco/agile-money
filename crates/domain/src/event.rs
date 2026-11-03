//! The append-only event log's vocabulary. Past tense, one variant per fact.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{
    Amount, ApprovalId, Decision, IntentHash, IntentId, InvoiceId, PaymentRequest, PolicyId,
    RailRef, Recipient, RunId, UserId,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    IntentProposed {
        intent: IntentId,
        hash: IntentHash,
        amount: Amount,
        to: Recipient,
        run: RunId,
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

/// An aggregate's stream name, e.g. `intent:<uuid>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StreamId(String);

impl StreamId {
    pub fn intent(id: IntentId) -> Self {
        Self(format!("intent:{id}"))
    }

    pub fn invoice(id: InvoiceId) -> Self {
        Self(format!("invoice:{id}"))
    }

    pub fn policy(id: PolicyId) -> Self {
        Self(format!("policy:{id}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
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
