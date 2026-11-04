//! I/O boundaries as traits. Adapters live in their own crates; fakes and
//! `mockall` mocks (feature `mock`) keep domain, policy and agent tests offline.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Address, AgentId, ApprovalId, Approved, Asset, Event, EventEnvelope, IdempotencyKey, Intent,
    IntentId, Invoice, PaymentRequest, Policy, RailId, Settlement, SpendSnapshot, StreamId,
    Submission,
};

#[derive(Debug, thiserror::Error)]
pub enum RailError {
    #[error("asset not supported by this rail")]
    Unsupported,
    #[error("rail call timed out")]
    Timeout,
    #[error("rail rejected the payment: {0}")]
    Rejected(String),
    #[error("transport error: {0}")]
    Transport(String),
}

impl RailError {
    /// Rule 13: only idempotent failures are retried.
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Timeout | Self::Transport(_))
    }
}

/// One payment network. Adding a chain is one more impl of this trait.
#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait Rail: Send + Sync {
    fn id(&self) -> RailId;
    fn supports(&self, asset: &Asset) -> bool;

    /// Build what the counterparty sees: a Solana Pay URL, a PayPal checkout link, an x402 challenge.
    async fn request(&self, inv: &Invoice) -> Result<PaymentRequest, RailError>;

    /// Only callable with an approved intent.
    async fn submit(&self, intent: &Intent<Approved>) -> Result<Submission, RailError>;

    /// Poll or verify settlement; must be safe to call any number of times.
    async fn status(&self, sub: &Submission) -> Result<Settlement, RailError>;
}

/// A command the executor picks up from the outbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutboxCommand {
    SubmitPayment { intent: IntentId },
    WatchPayment { intent: IntentId },
}

impl OutboxCommand {
    pub fn intent(&self) -> IntentId {
        match self {
            Self::SubmitPayment { intent } | Self::WatchPayment { intent } => *intent,
        }
    }

    /// The `type` tag, as stored in the `outbox.kind` column.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SubmitPayment { .. } => "submit_payment",
            Self::WatchPayment { .. } => "watch_payment",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("stream {0} changed concurrently")]
    Conflict(StreamId),
    #[error("duplicate idempotency key {0}")]
    Duplicate(IdempotencyKey),
    #[error("storage backend: {0}")]
    Backend(String),
}

/// Append-only event log plus outbox, written in one transaction (rules 11, 12).
#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Appends events and outbox commands atomically, failing if the stream has
    /// moved past `expected_len` events.
    async fn append(
        &self,
        stream: &StreamId,
        expected_len: u64,
        events: Vec<Event>,
        outbox: Vec<OutboxCommand>,
    ) -> Result<Vec<EventEnvelope>, StoreError>;

    async fn load(&self, stream: &StreamId) -> Result<Vec<EventEnvelope>, StoreError>;
}

/// An outbox row leased to one executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxJob {
    pub id: i64,
    pub command: OutboxCommand,
    /// Including this claim.
    pub attempts: u32,
}

/// The executor's side of the outbox. Claims are leases, so a crashed worker's
/// job comes back once `lease` runs out.
#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait Outbox: Send + Sync {
    async fn claim(&self, lease: SignedDuration) -> Result<Option<OutboxJob>, StoreError>;
    async fn complete(&self, id: i64) -> Result<(), StoreError>;
    async fn retry_later(&self, id: i64, delay: SignedDuration) -> Result<(), StoreError>;
}

/// Projections the policy pipeline reads instead of replaying every stream.
#[cfg_attr(feature = "mock", mockall::automock)]
#[async_trait]
pub trait ReadModel: Send + Sync {
    /// Latest policy version for an agent.
    async fn policy(&self, agent: AgentId) -> Result<Option<Policy>, StoreError>;

    /// Spend in `asset` for intents created at or after `since`.
    async fn spend(
        &self,
        agent: AgentId,
        asset: &Asset,
        since: Timestamp,
    ) -> Result<SpendSnapshot, StoreError>;

    /// Whether a payment from `agent` to `to` has been confirmed.
    async fn paid_before(&self, agent: AgentId, to: &Address) -> Result<bool, StoreError>;

    /// The intent an idempotency key already belongs to.
    async fn intent_by_key(&self, key: &IdempotencyKey) -> Result<Option<IntentId>, StoreError>;

    /// Intents waiting on a human, for the expiry sweeper.
    async fn awaiting_approval(&self) -> Result<Vec<IntentId>, StoreError>;

    /// The intent an approval was requested for. Lets approval buttons carry
    /// only the approval id (Telegram allows 64 bytes of callback data).
    async fn approval_intent(&self, approval: ApprovalId) -> Result<Option<IntentId>, StoreError>;
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

#[cfg_attr(feature = "mock", mockall::automock)]
pub trait IdGen: Send + Sync {
    fn next_uuid(&self) -> Uuid;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// Test clock that only moves when told to.
#[derive(Debug)]
pub struct FixedClock(Mutex<Timestamp>);

impl FixedClock {
    pub fn new(at: Timestamp) -> Self {
        Self(Mutex::new(at))
    }

    pub fn set(&self, at: Timestamp) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = at;
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct UuidV7;

impl IdGen for UuidV7 {
    fn next_uuid(&self) -> Uuid {
        Uuid::now_v7()
    }
}

/// Deterministic ids for tests: 1, 2, 3, ...
#[derive(Debug, Default)]
pub struct SequentialIds(AtomicU64);

impl IdGen for SequentialIds {
    fn next_uuid(&self) -> Uuid {
        Uuid::from_u128(u128::from(self.0.fetch_add(1, Ordering::Relaxed) + 1))
    }
}
