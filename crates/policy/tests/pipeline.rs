//! `Payments` against the in-memory store.

#![allow(clippy::unwrap_used)] // helpers outside #[test] fns, which clippy.toml does not cover

use std::sync::{Arc, Mutex};

use agile_money_domain::memory::MemStore;
use agile_money_domain::ports::{FixedClock, OutboxCommand, SequentialIds};
use agile_money_domain::*;
use agile_money_policy::{Approver, Error, Payments, ProposeOutcome, Resolution};
use async_trait::async_trait;
use jiff::{SignedDuration, Timestamp};
use uuid::Uuid;

fn sol() -> Asset {
    Asset {
        rail: RailId::Sol,
        code: "SOL".parse().unwrap(),
        decimals: 9,
        contract: None,
    }
}

fn agent() -> AgentId {
    AgentId::new(Uuid::from_u128(2))
}

fn acme() -> Address {
    Address::SolPubkey(SolAddress::from_bytes([1; 32]))
}

fn policy() -> Policy {
    Policy {
        id: PolicyId::new(Uuid::from_u128(9)),
        version: 1,
        agent: agent(),
        rules: vec![
            Rule::AllowAssets {
                assets: vec![sol()],
            },
            Rule::AllowRecipients {
                addresses: vec![acme()],
            },
            Rule::ApprovalAbove {
                amount: Amount::new(1_000, sol()),
            },
            Rule::MaxPerWindow {
                amount: Amount::new(10_000, sol()),
                window: SignedDuration::from_hours(24),
            },
        ],
    }
}

fn intent(n: u128, minor: u128) -> Intent<Proposed> {
    Intent::new(
        IntentId::new(Uuid::from_u128(100 + n)),
        agent(),
        Amount::new(minor, sol()),
        Recipient {
            label: "Acme".into(),
            address: acme(),
        },
        None,
        format!("key-{n}").parse().unwrap(),
        Provenance {
            run: RunId::new(Uuid::from_u128(3)),
            tool_call: format!("call-{n}").parse().unwrap(),
        },
        Timestamp::UNIX_EPOCH,
    )
}

#[derive(Default)]
struct Inbox(Mutex<Vec<ApprovalId>>);

#[async_trait]
impl Approver for Inbox {
    async fn request(&self, a: &Approval, _: &Intent<Proposed>) -> Result<(), Error> {
        self.0.lock().unwrap().push(a.id);
        Ok(())
    }
}

struct Rig {
    store: Arc<MemStore>,
    inbox: Arc<Inbox>,
    clock: Arc<FixedClock>,
    payments: Payments,
}

fn rig() -> Rig {
    let store = Arc::new(MemStore::with_policy(policy()));
    let inbox = Arc::new(Inbox::default());
    let clock = Arc::new(FixedClock::new(Timestamp::UNIX_EPOCH));
    let payments = Payments::new(
        store.clone(),
        store.clone(),
        inbox.clone(),
        clock.clone(),
        Arc::new(SequentialIds::default()),
    );
    Rig {
        store,
        inbox,
        clock,
        payments,
    }
}

#[tokio::test]
async fn small_payment_is_queued() {
    let r = rig();
    let out = r.payments.propose(intent(1, 500)).await.unwrap();
    let id = IntentId::new(Uuid::from_u128(101));
    assert_eq!(out, ProposeOutcome::Queued { intent: id });
    assert_eq!(
        r.store.pending_commands(),
        [OutboxCommand::SubmitPayment { intent: id }]
    );
}

#[tokio::test]
async fn large_payment_waits_then_queues_on_grant() {
    let r = rig();
    let ProposeOutcome::AwaitingApproval {
        intent: id,
        approval,
        ..
    } = r.payments.propose(intent(1, 5_000)).await.unwrap()
    else {
        panic!("expected approval");
    };
    assert_eq!(*r.inbox.0.lock().unwrap(), [approval]);
    assert!(r.store.pending_commands().is_empty());

    let by = UserId::new(Uuid::from_u128(7));
    assert_eq!(
        r.payments.grant(id, approval, by).await.unwrap(),
        Resolution::Granted
    );
    assert_eq!(
        r.store.pending_commands(),
        [OutboxCommand::SubmitPayment { intent: id }]
    );
    // A second tap does nothing.
    assert!(r.payments.grant(id, approval, by).await.is_err());
}

#[tokio::test]
async fn late_grant_expires_instead() {
    let r = rig();
    let ProposeOutcome::AwaitingApproval {
        intent: id,
        approval,
        ..
    } = r.payments.propose(intent(1, 5_000)).await.unwrap()
    else {
        panic!("expected approval");
    };
    r.clock.set(
        Timestamp::UNIX_EPOCH
            .checked_add(SignedDuration::from_mins(16))
            .unwrap(),
    );
    let by = UserId::new(Uuid::from_u128(7));
    assert_eq!(
        r.payments.grant(id, approval, by).await.unwrap(),
        Resolution::Expired
    );
    assert!(r.store.pending_commands().is_empty());
}

#[tokio::test]
async fn sweeper_expires_stale_approvals() {
    let r = rig();
    r.payments.propose(intent(1, 5_000)).await.unwrap();
    assert!(r.payments.expire_stale().await.unwrap().is_empty());
    r.clock.set(
        Timestamp::UNIX_EPOCH
            .checked_add(SignedDuration::from_mins(15))
            .unwrap(),
    );
    assert_eq!(r.payments.expire_stale().await.unwrap().len(), 1);
    let (state, _) = r
        .payments
        .status(IntentId::new(Uuid::from_u128(101)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state, IntentState::Rejected);
}

#[tokio::test]
async fn same_key_is_a_duplicate() {
    let r = rig();
    r.payments.propose(intent(1, 500)).await.unwrap();
    let mut again = intent(2, 500);
    again.idempotency_key = "key-1".parse().unwrap();
    assert_eq!(
        r.payments.propose(again).await.unwrap(),
        ProposeOutcome::Duplicate {
            intent: IntentId::new(Uuid::from_u128(101))
        }
    );
}

#[tokio::test]
async fn window_cap_counts_reserved_spend() {
    let r = rig();
    for n in 0..10 {
        r.payments.propose(intent(n, 1_000)).await.unwrap();
    }
    assert!(matches!(
        r.payments.propose(intent(10, 1)).await.unwrap(),
        ProposeOutcome::Denied { .. }
    ));
}

#[tokio::test]
async fn unknown_recipient_is_denied() {
    let r = rig();
    let mut i = intent(1, 10);
    i.to.address = Address::SolPubkey(SolAddress::from_bytes([6; 32]));
    assert!(matches!(
        r.payments.propose(i).await.unwrap(),
        ProposeOutcome::Denied { .. }
    ));
}
