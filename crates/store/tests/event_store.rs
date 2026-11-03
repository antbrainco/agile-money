//! `PgStore` as an `EventStore`, against a fresh database per test.

#![allow(clippy::unwrap_used)] // helpers outside #[test] fns, which clippy.toml does not cover

use jiff::{SignedDuration, Timestamp};
use sqlx::PgPool;
use uuid::Uuid;
use warden_domain::ports::{EventStore, Outbox, OutboxCommand, ReadModel, StoreError};
use warden_domain::{
    Address, AgentId, Amount, Asset, Decision, Event, Intent, IntentId, PolicyId, Proposed,
    Provenance, RailId, RailRef, Recipient, RunId, SolAddress, SolSignature, StreamId,
};
use warden_store::PgStore;

fn intent() -> IntentId {
    IntentId::new(Uuid::from_u128(1))
}

fn agent() -> AgentId {
    AgentId::new(Uuid::from_u128(2))
}

fn sol() -> Asset {
    Asset {
        rail: RailId::Sol,
        code: "SOL".parse().unwrap(),
        decimals: 9,
        contract: None,
    }
}

fn proposal_for(id: IntentId, minor: u128, key: &str) -> Intent<Proposed> {
    Intent::new(
        id,
        agent(),
        Amount::new(minor, sol()),
        Recipient {
            label: "Acme".into(),
            address: Address::SolPubkey(SolAddress::from_bytes([1; 32])),
        },
        None,
        key.parse().unwrap(),
        Provenance {
            run: RunId::new(Uuid::from_u128(3)),
            tool_call: "call-1".parse().unwrap(),
        },
        Timestamp::UNIX_EPOCH,
    )
}

fn proposed() -> Event {
    proposal_for(intent(), 100, "key-1")
        .proposed_event()
        .unwrap()
}

/// A stream holding just the proposal, so later appends start at version 1.
async fn seeded(store: &PgStore) -> StreamId {
    let stream = StreamId::Intent(intent());
    store
        .append(&stream, 0, vec![proposed()], vec![])
        .await
        .unwrap();
    stream
}

fn decided() -> Event {
    Event::IntentDecided {
        intent: intent(),
        decision: Decision::Allow,
        policy: PolicyId::new(Uuid::from_u128(9)),
        version: 1,
    }
}

fn submitted() -> Event {
    Event::PaymentSubmitted {
        intent: intent(),
        rail_ref: RailRef::Sol {
            reference: SolAddress::from_bytes([2; 32]),
            signature: None,
        },
    }
}

async fn outbox_len(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM outbox")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn append_then_load_round_trips(pool: PgPool) {
    let store = PgStore::from_pool(pool.clone());
    let stream = seeded(&store).await;
    let cmd = OutboxCommand::SubmitPayment { intent: intent() };

    let written = store
        .append(&stream, 1, vec![decided()], vec![cmd.clone()])
        .await
        .unwrap();
    store
        .append(&stream, 2, vec![submitted()], vec![])
        .await
        .unwrap();
    let loaded = store.load(&stream).await.unwrap();

    assert_eq!(loaded.len(), 3);
    assert_eq!(loaded[1], written[0]);
    assert_eq!(loaded[2].event, submitted());
    assert!(loaded[1].seq < loaded[2].seq);

    let (kind, body): (String, serde_json::Value) = sqlx::query_as("SELECT kind, body FROM outbox")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kind, "submit_payment");
    assert_eq!(serde_json::from_value::<OutboxCommand>(body).unwrap(), cmd);
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn stale_expected_len_conflicts_and_writes_nothing(pool: PgPool) {
    let store = PgStore::from_pool(pool.clone());
    let stream = seeded(&store).await;
    store
        .append(&stream, 1, vec![decided()], vec![])
        .await
        .unwrap();

    let cmd = OutboxCommand::SubmitPayment { intent: intent() };
    let err = store
        .append(&stream, 1, vec![submitted()], vec![cmd])
        .await
        .unwrap_err();

    assert!(matches!(err, StoreError::Conflict(_)));
    assert_eq!(store.load(&stream).await.unwrap().len(), 2);
    assert_eq!(outbox_len(&pool).await, 0);
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn concurrent_appends_at_same_position_conflict(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    let stream = seeded(&store).await;

    let (a, b) = tokio::join!(
        store.append(&stream, 1, vec![decided()], vec![]),
        store.append(&stream, 1, vec![decided()], vec![]),
    );

    assert_eq!(
        a.is_ok() as u8 + b.is_ok() as u8,
        1,
        "exactly one append wins"
    );
    for r in [a, b] {
        if let Err(e) = r {
            assert!(matches!(e, StoreError::Conflict(_)), "got {e:?}");
        }
    }
    assert_eq!(store.load(&stream).await.unwrap().len(), 2);
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn unknown_stream_loads_empty(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    assert!(
        store
            .load(&StreamId::Intent(intent()))
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn reused_idempotency_key_is_a_duplicate(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    seeded(&store).await;

    let other = IntentId::new(Uuid::from_u128(77));
    let event = proposal_for(other, 100, "key-1").proposed_event().unwrap();
    let err = store
        .append(&StreamId::Intent(other), 0, vec![event], vec![])
        .await
        .unwrap_err();

    assert!(matches!(err, StoreError::Duplicate(k) if k.as_str() == "key-1"));
    assert_eq!(
        store
            .intent_by_key(&"key-1".parse().unwrap())
            .await
            .unwrap(),
        Some(intent())
    );
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn spend_counts_reserved_and_settled(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    let stream = seeded(&store).await;
    let since = Timestamp::UNIX_EPOCH;

    let spend = store.spend(agent(), &sol(), since).await.unwrap();
    assert_eq!((spend.settled.minor, spend.reserved.minor), (0, 0));

    store
        .append(&stream, 1, vec![decided()], vec![])
        .await
        .unwrap();
    let spend = store.spend(agent(), &sol(), since).await.unwrap();
    assert_eq!((spend.settled.minor, spend.reserved.minor), (0, 100));

    let confirmed = Event::PaymentConfirmed {
        intent: intent(),
        rail_ref: RailRef::Sol {
            reference: SolAddress::from_bytes([2; 32]),
            signature: Some(SolSignature::from_bytes([3; 64])),
        },
    };
    store
        .append(&stream, 2, vec![submitted(), confirmed], vec![])
        .await
        .unwrap();
    let spend = store.spend(agent(), &sol(), since).await.unwrap();
    assert_eq!((spend.settled.minor, spend.reserved.minor), (100, 0));
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn outbox_claims_are_exclusive_leases(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    let stream = seeded(&store).await;
    let cmd = OutboxCommand::SubmitPayment { intent: intent() };
    store
        .append(&stream, 1, vec![decided()], vec![cmd.clone()])
        .await
        .unwrap();
    let lease = SignedDuration::from_secs(30);

    let job = store.claim(lease).await.unwrap().unwrap();
    assert_eq!((job.command, job.attempts), (cmd, 1));
    assert!(store.claim(lease).await.unwrap().is_none(), "leased");

    store
        .retry_later(job.id, SignedDuration::ZERO)
        .await
        .unwrap();
    let again = store.claim(lease).await.unwrap().unwrap();
    assert_eq!((again.id, again.attempts), (job.id, 2));

    store.complete(again.id).await.unwrap();
    store
        .retry_later(job.id, SignedDuration::ZERO)
        .await
        .unwrap();
    assert!(
        store.claim(lease).await.unwrap().is_none(),
        "done stays done"
    );
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn approval_maps_back_to_its_intent(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    let stream = seeded(&store).await;
    let approval = warden_domain::ApprovalId::new(Uuid::from_u128(55));
    store
        .append(
            &stream,
            1,
            vec![
                Event::IntentDecided {
                    intent: intent(),
                    decision: Decision::RequireApproval { reasons: vec![] },
                    policy: PolicyId::new(Uuid::from_u128(9)),
                    version: 1,
                },
                Event::ApprovalRequested {
                    approval,
                    intent: intent(),
                    expires_at: Timestamp::UNIX_EPOCH,
                },
            ],
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(
        store.approval_intent(approval).await.unwrap(),
        Some(intent())
    );
    assert_eq!(store.awaiting_approval().await.unwrap(), [intent()]);
}
