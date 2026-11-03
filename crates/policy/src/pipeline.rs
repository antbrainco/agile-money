//! The money path up to the outbox: propose, decide, record; then a human's
//! grant or reject. Every outcome is an event; `SubmitPayment` is only queued
//! next to the event that made the intent `Approved` (rules 1, 11).

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use warden_domain::ports::{Clock, EventStore, IdGen, OutboxCommand, ReadModel, StoreError};
use warden_domain::{
    Approval, ApprovalId, ApprovalState, Channel, Event, EventEnvelope, Intent, IntentId,
    IntentState, Policy, PolicyId, Proposed as ProposedState, RailRef, Rule, StreamId, UserId,
};

use crate::{Approver, Error, Outcome, PolicyEngine};

/// Spend window when a policy has no `MaxPerWindow`; only matters for display.
const DEFAULT_WINDOW: SignedDuration = SignedDuration::from_hours(24);

/// What happened to a new intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proposed {
    /// Allowed by policy and queued for the executor.
    Queued { intent: IntentId },
    AwaitingApproval {
        intent: IntentId,
        approval: ApprovalId,
        reasons: Vec<String>,
    },
    Denied {
        intent: IntentId,
        reasons: Vec<String>,
    },
    /// The idempotency key already belongs to this intent; nothing was written.
    Duplicate { intent: IntentId },
}

/// What happened to a pending approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Granted,
    Rejected,
    Expired,
    /// The intent no longer matches what was approved.
    Superseded,
}

/// Wires the policy engine to the event log. Cheap to clone.
#[derive(Clone)]
pub struct Payments {
    store: Arc<dyn EventStore>,
    read: Arc<dyn ReadModel>,
    approver: Arc<dyn Approver>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    engine: PolicyEngine,
}

impl std::fmt::Debug for Payments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Payments").finish_non_exhaustive()
    }
}

impl Payments {
    pub fn new(
        store: Arc<dyn EventStore>,
        read: Arc<dyn ReadModel>,
        approver: Arc<dyn Approver>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGen>,
    ) -> Self {
        Self {
            store,
            read,
            approver,
            clock,
            ids,
            engine: PolicyEngine::new(),
        }
    }

    #[tracing::instrument(skip_all, fields(intent = %intent.id))]
    pub async fn propose(&self, intent: Intent<ProposedState>) -> Result<Proposed, Error> {
        if let Some(existing) = self.read.intent_by_key(&intent.idempotency_key).await? {
            return Ok(Proposed::Duplicate { intent: existing });
        }
        let now = self.clock.now();
        let policy = self.read.policy(intent.agent).await?;
        let since = window_start(policy.as_ref(), now)?;
        let spend = self
            .read
            .spend(intent.agent, &intent.amount.asset, since)
            .await?;

        let id = intent.id;
        let key = intent.idempotency_key.clone();
        let mut events = vec![intent.proposed_event()?];
        let (decision, outcome) = self.engine.decide(policy.as_ref(), intent, &spend, now)?;
        events.push(Event::IntentDecided {
            intent: id,
            decision,
            policy: policy
                .as_ref()
                .map_or(PolicyId::new(uuid::Uuid::nil()), |p| p.id),
            version: policy.as_ref().map_or(0, |p| p.version),
        });

        let (result, outbox, pending) = match outcome {
            Outcome::Allowed(_approved) => (
                Proposed::Queued { intent: id },
                vec![OutboxCommand::SubmitPayment { intent: id }],
                None,
            ),
            Outcome::NeedsApproval(intent, reasons) => {
                let approval = Approval::new(
                    ApprovalId::new(self.ids.next_uuid()),
                    &intent,
                    Channel::Telegram,
                    now,
                )?;
                events.push(Event::ApprovalRequested {
                    approval: approval.id,
                    intent: id,
                    expires_at: approval.expires_at,
                });
                let result = Proposed::AwaitingApproval {
                    intent: id,
                    approval: approval.id,
                    reasons,
                };
                (result, vec![], Some((approval, intent)))
            }
            Outcome::Denied(reasons) => (
                Proposed::Denied {
                    intent: id,
                    reasons,
                },
                vec![],
                None,
            ),
        };

        match self
            .store
            .append(&StreamId::Intent(id), 0, events, outbox)
            .await
        {
            Ok(_) => {}
            // Lost a race with the same key: report the winner.
            Err(StoreError::Duplicate(_)) => {
                let winner = self.read.intent_by_key(&key).await?.unwrap_or(id);
                return Ok(Proposed::Duplicate { intent: winner });
            }
            Err(e) => return Err(e.into()),
        }

        // After the append: a failed send leaves a pending approval that the sweeper expires.
        if let Some((approval, intent)) = pending
            && let Err(e) = self.approver.request(&approval, &intent).await
        {
            tracing::warn!(error = %e, "approval request not delivered");
        }
        Ok(result)
    }

    /// A human said yes. Re-checks expiry and the intent hash before queueing.
    #[tracing::instrument(skip(self))]
    pub async fn grant(
        &self,
        id: IntentId,
        approval: ApprovalId,
        by: UserId,
    ) -> Result<Resolution, Error> {
        let (history, mut pending) = self.pending(id, approval).await?;
        let events: Vec<&Event> = history.iter().map(|e| &e.event).collect();
        let intent = Intent::<ProposedState>::from_history(id, events.iter().copied())?;
        let now = self.clock.now();

        let (event, outbox, resolution) = match pending.grant(intent, now) {
            Ok(_approved) => (
                Event::ApprovalGranted { approval, by },
                vec![OutboxCommand::SubmitPayment { intent: id }],
                Resolution::Granted,
            ),
            Err(warden_domain::Error::ApprovalExpired) => {
                (rejected(approval, None, true), vec![], Resolution::Expired)
            }
            Err(warden_domain::Error::HashMismatch) => (
                rejected(approval, None, false),
                vec![],
                Resolution::Superseded,
            ),
            Err(e) => return Err(e.into()),
        };
        self.store
            .append(
                &StreamId::Intent(id),
                history.len() as u64,
                vec![event],
                outbox,
            )
            .await?;
        Ok(resolution)
    }

    #[tracing::instrument(skip(self))]
    pub async fn reject(
        &self,
        id: IntentId,
        approval: ApprovalId,
        by: UserId,
    ) -> Result<Resolution, Error> {
        let (history, _) = self.pending(id, approval).await?;
        self.store
            .append(
                &StreamId::Intent(id),
                history.len() as u64,
                vec![rejected(approval, Some(by), false)],
                vec![],
            )
            .await?;
        Ok(Resolution::Rejected)
    }

    /// Rejects approvals past their expiry (rule 8). Returns how many expired.
    pub async fn expire_stale(&self) -> Result<usize, Error> {
        let now = self.clock.now();
        let mut expired = 0;
        for id in self.read.awaiting_approval().await? {
            let history = self.store.load(&StreamId::Intent(id)).await?;
            let Some((approval, expires_at)) = requested(&history) else {
                continue;
            };
            if now < expires_at {
                continue;
            }
            match self
                .store
                .append(
                    &StreamId::Intent(id),
                    history.len() as u64,
                    vec![rejected(approval, None, true)],
                    vec![],
                )
                .await
            {
                Ok(_) => expired += 1,
                // Someone answered it meanwhile.
                Err(StoreError::Conflict(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(expired)
    }

    /// Current state and latest rail reference, for the agent and the user.
    pub async fn status(
        &self,
        id: IntentId,
    ) -> Result<Option<(IntentState, Option<RailRef>)>, Error> {
        let history = self.store.load(&StreamId::Intent(id)).await?;
        if history.is_empty() {
            return Ok(None);
        }
        let state = IntentState::replay(history.iter().map(|e| &e.event))?;
        let rail_ref = history.iter().rev().find_map(|e| match &e.event {
            Event::PaymentSubmitted { rail_ref, .. } | Event::PaymentConfirmed { rail_ref, .. } => {
                Some(rail_ref.clone())
            }
            _ => None,
        });
        Ok(Some((state, rail_ref)))
    }

    /// Loads the stream and rebuilds the pending approval, failing unless the
    /// intent is still waiting on exactly this approval.
    async fn pending(
        &self,
        id: IntentId,
        approval: ApprovalId,
    ) -> Result<(Vec<EventEnvelope>, Approval), Error> {
        let history = self.store.load(&StreamId::Intent(id)).await?;
        let state = IntentState::replay(history.iter().map(|e| &e.event))?;
        if state != IntentState::AwaitingApproval {
            return Err(Error::NotAwaiting(state.name()));
        }
        let (requested_id, expires_at) =
            requested(&history).ok_or_else(|| Error::UnknownApproval(approval.to_string()))?;
        if requested_id != approval {
            return Err(Error::UnknownApproval(approval.to_string()));
        }
        let intent_hash = history
            .iter()
            .find_map(|e| match &e.event {
                Event::IntentProposed { hash, .. } => Some(hash.clone()),
                _ => None,
            })
            .ok_or(warden_domain::Error::MissingProposal(id))?;
        let pending = Approval {
            id: approval,
            intent: id,
            intent_hash,
            state: ApprovalState::Pending,
            expires_at,
            channel: Channel::Telegram,
        };
        Ok((history, pending))
    }
}

fn requested(history: &[EventEnvelope]) -> Option<(ApprovalId, Timestamp)> {
    history.iter().rev().find_map(|e| match &e.event {
        Event::ApprovalRequested {
            approval,
            expires_at,
            ..
        } => Some((*approval, *expires_at)),
        _ => None,
    })
}

fn rejected(approval: ApprovalId, by: Option<UserId>, expired: bool) -> Event {
    Event::ApprovalRejected {
        approval,
        by,
        expired,
    }
}

fn window_start(policy: Option<&Policy>, now: Timestamp) -> Result<Timestamp, Error> {
    let window = policy
        .into_iter()
        .flat_map(|p| &p.rules)
        .find_map(|r| match r {
            Rule::MaxPerWindow { window, .. } => Some(*window),
            _ => None,
        })
        .unwrap_or(DEFAULT_WINDOW);
    now.checked_sub(window)
        .map_err(|_| warden_domain::Error::Overflow.into())
}
