//! `PgStore` as an `EventStore`, against a fresh database per test.

#![allow(clippy::unwrap_used)] // helpers outside #[test] fns, which clippy.toml does not cover

use sqlx::PgPool;
use uuid::Uuid;
use warden_domain::ports::{EventStore, OutboxCommand, StoreError};
use warden_domain::{Decision, Event, IntentId, PolicyId, RailRef, StreamId};
use warden_store::PgStore;

fn intent() -> IntentId {
    IntentId::new(Uuid::from_u128(1))
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
            reference: "ref".into(),
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
    let stream = StreamId::intent(intent());
    let cmd = OutboxCommand::SubmitPayment { intent: intent() };

    let written = store
        .append(&stream, 0, vec![decided()], vec![cmd.clone()])
        .await
        .unwrap();
    store
        .append(&stream, 1, vec![submitted()], vec![])
        .await
        .unwrap();
    let loaded = store.load(&stream).await.unwrap();

    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0], written[0]);
    assert_eq!(loaded[1].event, submitted());
    assert!(loaded[0].seq < loaded[1].seq);

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
    let stream = StreamId::intent(intent());
    store
        .append(&stream, 0, vec![decided()], vec![])
        .await
        .unwrap();

    let cmd = OutboxCommand::SubmitPayment { intent: intent() };
    let err = store
        .append(&stream, 0, vec![submitted()], vec![cmd])
        .await
        .unwrap_err();

    assert!(matches!(err, StoreError::Conflict(_)));
    assert_eq!(store.load(&stream).await.unwrap().len(), 1);
    assert_eq!(outbox_len(&pool).await, 0);
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn concurrent_appends_at_same_position_conflict(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    let stream = StreamId::intent(intent());

    let (a, b) = tokio::join!(
        store.append(&stream, 0, vec![decided()], vec![]),
        store.append(&stream, 0, vec![decided()], vec![]),
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
    assert_eq!(store.load(&stream).await.unwrap().len(), 1);
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn unknown_stream_loads_empty(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    assert!(
        store
            .load(&StreamId::intent(intent()))
            .await
            .unwrap()
            .is_empty()
    );
}
