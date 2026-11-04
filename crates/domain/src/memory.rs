//! In-memory `EventStore`, `ReadModel` and `Outbox` for tests and offline demos.
//! Same contract as Postgres: optimistic appends, one intent per idempotency key.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use jiff::{SignedDuration, Timestamp};

use crate::ports::{EventStore, Outbox, OutboxCommand, OutboxJob, ReadModel, StoreError};
use crate::{
    Address, AgentId, Amount, ApprovalId, Asset, Event, EventEnvelope, IdempotencyKey, Intent,
    IntentId, IntentState, Policy, Proposed, SpendSnapshot, StreamId,
};

#[derive(Debug, Default)]
struct Inner {
    seq: i64,
    streams: HashMap<StreamId, Vec<EventEnvelope>>,
    keys: HashMap<IdempotencyKey, IntentId>,
    policies: Vec<Policy>,
    outbox: Vec<(OutboxJob, Slot)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Ready,
    Leased,
    Done,
}

#[derive(Debug, Default)]
pub struct MemStore(Mutex<Inner>);

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_policy(policy: Policy) -> Self {
        let store = Self::default();
        store.lock().policies.push(policy);
        store
    }

    /// Outbox commands not yet completed, oldest first.
    pub fn pending_commands(&self) -> Vec<OutboxCommand> {
        self.lock()
            .outbox
            .iter()
            .filter(|(_, slot)| *slot != Slot::Done)
            .map(|(job, _)| job.command.clone())
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl EventStore for MemStore {
    async fn append(
        &self,
        stream: &StreamId,
        expected_len: u64,
        events: Vec<Event>,
        outbox: Vec<OutboxCommand>,
    ) -> Result<Vec<EventEnvelope>, StoreError> {
        let mut inner = self.lock();
        let len = inner.streams.get(stream).map_or(0, Vec::len) as u64;
        if len != expected_len {
            return Err(StoreError::Conflict(*stream));
        }
        for event in &events {
            if let Event::IntentProposed { proposal, .. } = event
                && inner.keys.contains_key(&proposal.idempotency_key)
            {
                return Err(StoreError::Duplicate(proposal.idempotency_key.clone()));
            }
        }
        for event in &events {
            if let Event::IntentProposed {
                intent, proposal, ..
            } = event
            {
                inner.keys.insert(proposal.idempotency_key.clone(), *intent);
            }
        }
        let mut written = Vec::with_capacity(events.len());
        for event in events {
            inner.seq += 1;
            written.push(EventEnvelope {
                seq: inner.seq,
                stream: *stream,
                at: Timestamp::now(),
                event,
            });
        }
        inner
            .streams
            .entry(*stream)
            .or_default()
            .extend(written.iter().cloned());
        for command in outbox {
            let id = inner.outbox.len() as i64 + 1;
            inner.outbox.push((
                OutboxJob {
                    id,
                    command,
                    attempts: 0,
                },
                Slot::Ready,
            ));
        }
        Ok(written)
    }

    async fn load(&self, stream: &StreamId) -> Result<Vec<EventEnvelope>, StoreError> {
        Ok(self.lock().streams.get(stream).cloned().unwrap_or_default())
    }
}

#[async_trait]
impl ReadModel for MemStore {
    async fn policy(&self, agent: AgentId) -> Result<Option<Policy>, StoreError> {
        Ok(self
            .lock()
            .policies
            .iter()
            .filter(|p| p.agent == agent)
            .max_by_key(|p| p.version)
            .cloned())
    }

    async fn spend(
        &self,
        agent: AgentId,
        asset: &Asset,
        since: Timestamp,
    ) -> Result<SpendSnapshot, StoreError> {
        let inner = self.lock();
        let (mut settled, mut reserved) = (0u128, 0u128);
        for (stream, history) in &inner.streams {
            let Some(id) = stream.as_intent() else {
                continue;
            };
            let events: Vec<&Event> = history.iter().map(|e| &e.event).collect();
            let backend = |e: crate::Error| StoreError::Backend(e.to_string());
            let intent =
                Intent::<Proposed>::from_history(id, events.iter().copied()).map_err(backend)?;
            if intent.agent != agent || &intent.amount.asset != asset || intent.created_at < since {
                continue;
            }
            let bucket = match IntentState::replay(events).map_err(backend)? {
                IntentState::Confirmed => &mut settled,
                IntentState::Approved
                | IntentState::Submitted { .. }
                | IntentState::Failed { .. } => &mut reserved,
                _ => continue,
            };
            *bucket = bucket.saturating_add(intent.amount.minor);
        }
        Ok(SpendSnapshot {
            settled: Amount::new(settled, asset.clone()),
            reserved: Amount::new(reserved, asset.clone()),
            window_start: since,
            recipient_paid: false,
        })
    }

    async fn paid_before(&self, agent: AgentId, to: &Address) -> Result<bool, StoreError> {
        let inner = self.lock();
        let backend = |e: crate::Error| StoreError::Backend(e.to_string());
        for (stream, history) in &inner.streams {
            let Some(id) = stream.as_intent() else {
                continue;
            };
            let events: Vec<&Event> = history.iter().map(|e| &e.event).collect();
            let intent =
                Intent::<Proposed>::from_history(id, events.iter().copied()).map_err(backend)?;
            if intent.agent == agent
                && &intent.to.address == to
                && IntentState::replay(events).map_err(backend)? == IntentState::Confirmed
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn intent_by_key(&self, key: &IdempotencyKey) -> Result<Option<IntentId>, StoreError> {
        Ok(self.lock().keys.get(key).copied())
    }

    async fn awaiting_approval(&self) -> Result<Vec<IntentId>, StoreError> {
        let inner = self.lock();
        let mut ids = Vec::new();
        for (stream, history) in &inner.streams {
            if let Some(id) = stream.as_intent()
                && IntentState::replay(history.iter().map(|e| &e.event))
                    .is_ok_and(|s| s == IntentState::AwaitingApproval)
            {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    async fn approval_intent(&self, approval: ApprovalId) -> Result<Option<IntentId>, StoreError> {
        let inner = self.lock();
        Ok(inner
            .streams
            .values()
            .flatten()
            .find_map(|e| match &e.event {
                Event::ApprovalRequested {
                    approval: a,
                    intent,
                    ..
                } if *a == approval => Some(*intent),
                _ => None,
            }))
    }
}

/// Leases are ignored: a claimed job stays claimed until completed or retried.
#[async_trait]
impl Outbox for MemStore {
    async fn claim(&self, _lease: SignedDuration) -> Result<Option<OutboxJob>, StoreError> {
        let mut inner = self.lock();
        Ok(inner
            .outbox
            .iter_mut()
            .find(|(_, slot)| *slot == Slot::Ready)
            .map(|(job, slot)| {
                *slot = Slot::Leased;
                job.attempts += 1;
                job.clone()
            }))
    }

    async fn complete(&self, id: i64) -> Result<(), StoreError> {
        if let Some((_, slot)) = self.lock().outbox.iter_mut().find(|(j, _)| j.id == id) {
            *slot = Slot::Done;
        }
        Ok(())
    }

    async fn retry_later(&self, id: i64, _delay: SignedDuration) -> Result<(), StoreError> {
        if let Some((_, slot)) = self.lock().outbox.iter_mut().find(|(j, _)| j.id == id)
            && *slot == Slot::Leased
        {
            *slot = Slot::Ready;
        }
        Ok(())
    }
}
