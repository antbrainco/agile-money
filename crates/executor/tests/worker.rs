//! `Executor` against the in-memory store and a scripted rail.

#![allow(clippy::unwrap_used)] // helpers outside #[test] fns, which clippy.toml does not cover

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use agile_money_domain::memory::MemStore;
use agile_money_domain::ports::{EventStore, OutboxCommand, Rail, RailError};
use agile_money_domain::*;
use agile_money_executor::{ExecConfig, Executor, Notice, Notifier, PauseSwitch};
use async_trait::async_trait;
use jiff::Timestamp;
use uuid::Uuid;

fn sol() -> Asset {
    Asset {
        rail: RailId::Sol,
        code: "SOL".parse().unwrap(),
        decimals: 9,
        contract: None,
    }
}

fn rail_ref() -> RailRef {
    RailRef::Sol {
        reference: SolAddress::from_bytes([2; 32]),
        signature: Some(SolSignature::from_bytes([3; 64])),
    }
}

fn id() -> IntentId {
    IntentId::new(Uuid::from_u128(1))
}

/// Replies to `submit` and `status` in order; counts submits.
#[derive(Default)]
struct Script {
    submits: Mutex<VecDeque<Result<(), RailError>>>,
    statuses: Mutex<VecDeque<Settlement>>,
    sent: Mutex<u32>,
}

#[async_trait]
impl Rail for Script {
    fn id(&self) -> RailId {
        RailId::Sol
    }
    fn supports(&self, asset: &Asset) -> bool {
        *asset == sol()
    }
    async fn request(&self, _: &Invoice) -> Result<PaymentRequest, RailError> {
        Err(RailError::Unsupported)
    }
    async fn submit(&self, intent: &Intent<Approved>) -> Result<Submission, RailError> {
        *self.sent.lock().unwrap() += 1;
        self.submits.lock().unwrap().pop_front().unwrap_or(Ok(()))?;
        Ok(Submission {
            intent: intent.id,
            rail_ref: rail_ref(),
            at: Timestamp::UNIX_EPOCH,
        })
    }
    async fn status(&self, _: &Submission) -> Result<Settlement, RailError> {
        Ok(self
            .statuses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Settlement::Confirmed {
                at: Timestamp::UNIX_EPOCH,
            }))
    }
}

#[derive(Default)]
struct Log(Mutex<Vec<Notice>>);

#[async_trait]
impl Notifier for Log {
    async fn notify(&self, n: Notice) {
        self.0.lock().unwrap().push(n);
    }
}

struct Rig {
    store: Arc<MemStore>,
    rail: Arc<Script>,
    log: Arc<Log>,
    pause: PauseSwitch,
    exec: Executor,
}

/// An intent that policy allowed, with its `SubmitPayment` queued.
async fn rig() -> Rig {
    let store = Arc::new(MemStore::new());
    let intent = Intent::new(
        id(),
        AgentId::new(Uuid::from_u128(2)),
        Amount::new(100_000_000, sol()),
        Recipient {
            label: "Acme".into(),
            address: Address::SolPubkey(SolAddress::from_bytes([1; 32])),
        },
        None,
        "key-1".parse().unwrap(),
        Provenance {
            run: RunId::new(Uuid::from_u128(3)),
            tool_call: "c".parse().unwrap(),
        },
        Timestamp::UNIX_EPOCH,
    );
    store
        .append(
            &StreamId::Intent(id()),
            0,
            vec![
                intent.proposed_event().unwrap(),
                Event::IntentDecided {
                    intent: id(),
                    decision: Decision::Allow,
                    policy: PolicyId::new(Uuid::from_u128(9)),
                    version: 1,
                },
            ],
            vec![OutboxCommand::SubmitPayment { intent: id() }],
        )
        .await
        .unwrap();
    let rail = Arc::new(Script::default());
    let log = Arc::new(Log::default());
    let pause = PauseSwitch::default();
    let exec = Executor::new(
        store.clone(),
        store.clone(),
        vec![rail.clone()],
        pause.clone(),
        log.clone(),
        ExecConfig::default(),
    );
    Rig {
        store,
        rail,
        log,
        pause,
        exec,
    }
}

async fn state(store: &MemStore) -> IntentState {
    let h = store.load(&StreamId::Intent(id())).await.unwrap();
    IntentState::replay(h.iter().map(|e| &e.event)).unwrap()
}

async fn drain(exec: &Executor) {
    for _ in 0..20 {
        if !exec.tick().await.unwrap() {
            return;
        }
    }
}

#[tokio::test]
async fn submits_then_confirms() {
    let r = rig().await;
    r.rail
        .statuses
        .lock()
        .unwrap()
        .push_back(Settlement::Pending);
    drain(&r.exec).await;

    assert_eq!(state(&r.store).await, IntentState::Confirmed);
    assert_eq!(*r.rail.sent.lock().unwrap(), 1);
    assert!(r.store.pending_commands().is_empty());
    let log = r.log.0.lock().unwrap();
    assert!(matches!(log[0], Notice::Sent { .. }));
    assert!(matches!(&log[1], Notice::Confirmed { summary, .. } if summary == "0.1 SOL to Acme"));
}

#[tokio::test]
async fn transport_errors_retry_until_abandoned() {
    let r = rig().await;
    for _ in 0..MAX_SUBMIT_ATTEMPTS {
        r.rail
            .submits
            .lock()
            .unwrap()
            .push_back(Err(RailError::Timeout));
    }
    drain(&r.exec).await;

    assert_eq!(state(&r.store).await, IntentState::Abandoned);
    assert_eq!(*r.rail.sent.lock().unwrap(), u32::from(MAX_SUBMIT_ATTEMPTS));
    assert!(r.store.pending_commands().is_empty());
}

#[tokio::test]
async fn rejection_is_not_retried() {
    let r = rig().await;
    r.rail
        .submits
        .lock()
        .unwrap()
        .push_back(Err(RailError::Rejected("insufficient funds".into())));
    drain(&r.exec).await;

    assert_eq!(state(&r.store).await, IntentState::Abandoned);
    assert_eq!(*r.rail.sent.lock().unwrap(), 1);
}

#[tokio::test]
async fn paused_executor_sends_nothing() {
    let r = rig().await;
    r.pause.pause();
    drain(&r.exec).await;
    assert_eq!(*r.rail.sent.lock().unwrap(), 0);
    assert_eq!(state(&r.store).await, IntentState::Approved);

    r.pause.resume();
    drain(&r.exec).await;
    assert_eq!(state(&r.store).await, IntentState::Confirmed);
}

#[tokio::test]
async fn replayed_submit_job_does_not_send_twice() {
    let r = rig().await;
    drain(&r.exec).await;
    // A duplicate command, as after a crash between append and complete.
    let len = r.store.load(&StreamId::Intent(id())).await.unwrap().len() as u64;
    r.store
        .append(
            &StreamId::Intent(id()),
            len,
            vec![],
            vec![OutboxCommand::SubmitPayment { intent: id() }],
        )
        .await
        .unwrap();
    drain(&r.exec).await;
    assert_eq!(*r.rail.sent.lock().unwrap(), 1);
}
