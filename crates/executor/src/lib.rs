//! The only code that signs. Claims outbox jobs, checks the pause flag, rebuilds
//! `Intent<Approved>` from the log, submits it to its rail, and watches it settle.
//! Every outcome is appended as an event before the job is marked done, so a
//! crash at any point replays safely: rails dedupe by native reference (rule 10).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;
use warden_domain::ports::{
    EventStore, Outbox, OutboxCommand, OutboxJob, Rail, RailError, StoreError,
};
use warden_domain::{
    Approved, Event, EventEnvelope, Intent, IntentId, IntentState, MAX_SUBMIT_ATTEMPTS, RailRef,
    Settlement, StreamId, Submission,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("executor is paused")]
    Paused,
    #[error(transparent)]
    Rail(#[from] RailError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Domain(#[from] warden_domain::Error),
}

/// Global kill switch (rule 6), checked before every submit.
#[derive(Debug, Clone, Default)]
pub struct PauseSwitch(Arc<AtomicBool>);

impl PauseSwitch {
    pub fn pause(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.0.store(false, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn ensure_running(&self) -> Result<(), Error> {
        if self.is_paused() {
            Err(Error::Paused)
        } else {
            Ok(())
        }
    }
}

/// What the owner hears about, e.g. on Telegram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Sent {
        intent: IntentId,
        summary: String,
        rail_ref: RailRef,
    },
    Confirmed {
        intent: IntentId,
        summary: String,
        rail_ref: RailRef,
    },
    Failed {
        intent: IntentId,
        summary: String,
        reason: String,
        /// No more retries.
        abandoned: bool,
    },
}

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn notify(&self, notice: Notice);
}

/// `[executor]` in `warden.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ExecConfig {
    pub poll_interval_ms: u64,
    /// Longer than the slowest submit, so a live job is never claimed twice.
    pub lease_ms: u64,
    /// Before resubmitting after a transport error. On Solana this outlives a
    /// blockhash (~60-90 s), so a send that timed out has landed or expired by
    /// the time the reference is checked again.
    pub retry_delay_ms: u64,
    /// Between settlement checks.
    pub watch_delay_ms: u64,
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 500,
            lease_ms: 180_000,
            retry_delay_ms: 90_000,
            watch_delay_ms: 2_000,
        }
    }
}

pub struct Executor {
    store: Arc<dyn EventStore>,
    outbox: Arc<dyn Outbox>,
    rails: Vec<Arc<dyn Rail>>,
    pause: PauseSwitch,
    notifier: Arc<dyn Notifier>,
    config: ExecConfig,
}

impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Executor")
            .field(
                "rails",
                &self.rails.iter().map(|r| r.id()).collect::<Vec<_>>(),
            )
            .field("paused", &self.pause.is_paused())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// What one job's handling decided about the job itself.
enum Next {
    Done,
    RetryIn(u64),
}

impl Executor {
    pub fn new(
        store: Arc<dyn EventStore>,
        outbox: Arc<dyn Outbox>,
        rails: Vec<Arc<dyn Rail>>,
        pause: PauseSwitch,
        notifier: Arc<dyn Notifier>,
        config: ExecConfig,
    ) -> Self {
        Self {
            store,
            outbox,
            rails,
            pause,
            notifier,
            config,
        }
    }

    /// Runs until the task is dropped or aborted. Aborting mid-job is safe.
    pub async fn run(self) {
        let idle = Duration::from_millis(self.config.poll_interval_ms);
        loop {
            match self.tick().await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => tracing::error!(error = %e, "executor tick failed"),
            }
            tokio::time::sleep(idle).await;
        }
    }

    /// Claims and handles at most one job. `Ok(false)` when there was nothing to do.
    pub async fn tick(&self) -> Result<bool, Error> {
        if self.pause.is_paused() {
            return Ok(false);
        }
        let lease = millis(self.config.lease_ms);
        let Some(job) = self.outbox.claim(lease).await? else {
            return Ok(false);
        };
        let next = match self.handle(&job).await {
            Ok(next) => next,
            // Someone appended meanwhile (an approval sweep, another worker): look again soon.
            Err(Error::Store(StoreError::Conflict(_))) => {
                Next::RetryIn(self.config.poll_interval_ms)
            }
            Err(e) => {
                tracing::error!(job = job.id, error = %e, "job failed; will retry");
                Next::RetryIn(self.config.retry_delay_ms)
            }
        };
        match next {
            Next::Done => self.outbox.complete(job.id).await?,
            Next::RetryIn(ms) => self.outbox.retry_later(job.id, millis(ms)).await?,
        }
        Ok(true)
    }

    #[tracing::instrument(skip_all, fields(job = job.id, kind = job.command.kind(), intent = %job.command.intent()))]
    async fn handle(&self, job: &OutboxJob) -> Result<Next, Error> {
        let id = job.command.intent();
        let stream = StreamId::Intent(id);
        let history = self.store.load(&stream).await?;
        match job.command {
            OutboxCommand::SubmitPayment { .. } => self.submit(id, &stream, &history).await,
            OutboxCommand::WatchPayment { .. } => self.watch(id, &stream, &history).await,
        }
    }

    async fn submit(
        &self,
        id: IntentId,
        stream: &StreamId,
        history: &[EventEnvelope],
    ) -> Result<Next, Error> {
        if self.pause.is_paused() {
            return Ok(Next::RetryIn(self.config.poll_interval_ms));
        }
        let events = history.iter().map(|e| &e.event);
        let (intent, state) = match Intent::<Approved>::from_history(id, events) {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(error = %e, "not submittable; dropping job");
                return Ok(Next::Done);
            }
        };
        let tries = match state {
            // Submitted before a crash; its WatchPayment was queued with it.
            IntentState::Submitted { .. } => return Ok(Next::Done),
            IntentState::Failed { attempts } => attempts + 1,
            _ => 1,
        };
        let summary = summary(&intent);
        let len = history.len() as u64;

        let result = match self.rails.iter().find(|r| r.supports(&intent.amount.asset)) {
            Some(rail) => rail.submit(&intent).await,
            None => Err(RailError::Unsupported),
        };
        match result {
            Ok(sub) => {
                self.store
                    .append(
                        stream,
                        len,
                        vec![Event::PaymentSubmitted {
                            intent: id,
                            rail_ref: sub.rail_ref.clone(),
                        }],
                        vec![OutboxCommand::WatchPayment { intent: id }],
                    )
                    .await?;
                self.notifier
                    .notify(Notice::Sent {
                        intent: id,
                        summary,
                        rail_ref: sub.rail_ref,
                    })
                    .await;
                Ok(Next::Done)
            }
            Err(e) => {
                let retry = e.retryable() && tries < MAX_SUBMIT_ATTEMPTS;
                let mut events = vec![Event::PaymentFailed {
                    intent: id,
                    reason: e.to_string(),
                    retryable: e.retryable(),
                }];
                if !retry {
                    events.push(Event::PaymentAbandoned {
                        intent: id,
                        attempts: tries,
                    });
                }
                self.store.append(stream, len, events, vec![]).await?;
                self.notifier
                    .notify(Notice::Failed {
                        intent: id,
                        summary,
                        reason: e.to_string(),
                        abandoned: !retry,
                    })
                    .await;
                Ok(if retry {
                    Next::RetryIn(self.config.retry_delay_ms)
                } else {
                    Next::Done
                })
            }
        }
    }

    async fn watch(
        &self,
        id: IntentId,
        stream: &StreamId,
        history: &[EventEnvelope],
    ) -> Result<Next, Error> {
        let events = history.iter().map(|e| &e.event);
        let (intent, state) = match Intent::<Approved>::from_history(id, events) {
            Ok(found) => found,
            Err(_) => return Ok(Next::Done),
        };
        let IntentState::Submitted { attempts } = state else {
            return Ok(Next::Done);
        };
        let Some(rail_ref) = history.iter().rev().find_map(|e| match &e.event {
            Event::PaymentSubmitted { rail_ref, .. } => Some(rail_ref.clone()),
            _ => None,
        }) else {
            return Ok(Next::Done);
        };
        let Some(rail) = self.rails.iter().find(|r| r.supports(&intent.amount.asset)) else {
            return Ok(Next::RetryIn(self.config.retry_delay_ms));
        };
        let sub = Submission {
            intent: id,
            rail_ref: rail_ref.clone(),
            at: Timestamp::now(),
        };
        let len = history.len() as u64;
        match rail.status(&sub).await {
            Ok(Settlement::Pending) => Ok(Next::RetryIn(self.config.watch_delay_ms)),
            Ok(Settlement::Confirmed { .. }) => {
                self.store
                    .append(
                        stream,
                        len,
                        vec![Event::PaymentConfirmed {
                            intent: id,
                            rail_ref: rail_ref.clone(),
                        }],
                        vec![],
                    )
                    .await?;
                self.notifier
                    .notify(Notice::Confirmed {
                        intent: id,
                        summary: summary(&intent),
                        rail_ref,
                    })
                    .await;
                Ok(Next::Done)
            }
            // Landed and failed on chain: resending would fail the same way.
            Ok(Settlement::Failed { reason }) => {
                self.store
                    .append(
                        stream,
                        len,
                        vec![
                            Event::PaymentFailed {
                                intent: id,
                                reason: reason.clone(),
                                retryable: false,
                            },
                            Event::PaymentAbandoned {
                                intent: id,
                                attempts,
                            },
                        ],
                        vec![],
                    )
                    .await?;
                self.notifier
                    .notify(Notice::Failed {
                        intent: id,
                        summary: summary(&intent),
                        reason,
                        abandoned: true,
                    })
                    .await;
                Ok(Next::Done)
            }
            Err(e) => {
                tracing::warn!(error = %e, "status check failed");
                Ok(Next::RetryIn(self.config.watch_delay_ms))
            }
        }
    }
}

fn summary(intent: &Intent<Approved>) -> String {
    format!("{} to {}", intent.amount, intent.to.label)
}

fn millis(ms: u64) -> SignedDuration {
    SignedDuration::from_millis(i64::try_from(ms).unwrap_or(i64::MAX))
}
