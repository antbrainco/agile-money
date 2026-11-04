//! Contacts and the pause switch; needs `DATABASE_URL`.

#![allow(clippy::unwrap_used)] // helpers outside #[test] fns, which clippy.toml does not cover

use jiff::{SignedDuration, Timestamp};
use sqlx::PgPool;
use uuid::Uuid;
use warden_domain::{Address, AgentId, SolAddress};
use warden_store::{Contact, ContactSource, ContactState, PgStore};

fn contact(label: &str, byte: u8, state: ContactState) -> Contact {
    Contact {
        id: Uuid::from_u128(u128::from(byte) << 8 | label.len() as u128),
        label: label.into(),
        address: Address::SolPubkey(SolAddress::from_bytes([byte; 32])),
        state,
        source: ContactSource::Telegram,
        replaces: None,
        proposed_at: Timestamp::UNIX_EPOCH,
        confirmed_at: None,
        active_at: None,
        closed_at: None,
    }
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn a_contact_moves_once_and_closed_ones_drop_out(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    let agent = AgentId::new(Uuid::from_u128(1));
    let c = contact("Acme", 1, ContactState::Proposed);
    store.insert_contact(agent, &c).await.unwrap();

    let now = Timestamp::UNIX_EPOCH + SignedDuration::from_mins(1);
    let live = now + SignedDuration::from_hours(24);
    let from = [ContactState::Proposed];
    assert!(
        store
            .move_contact(c.id, &from, ContactState::Confirmed, Some(live), now)
            .await
            .unwrap()
    );
    // A second answer to the same proposal loses.
    assert!(
        !store
            .move_contact(c.id, &from, ContactState::Confirmed, Some(live), now)
            .await
            .unwrap()
    );

    let saved = store.contact(c.id).await.unwrap().unwrap();
    assert_eq!(saved.state, ContactState::Confirmed);
    assert_eq!(saved.confirmed_at, Some(now));
    assert_eq!(saved.active_at, Some(live));

    let to = ContactState::Cancelled;
    assert!(
        store
            .move_contact(c.id, &[ContactState::Confirmed], to, None, now)
            .await
            .unwrap()
    );
    assert!(store.contacts(agent).await.unwrap().is_empty());
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn two_live_contacts_cannot_share_an_address(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    let agent = AgentId::new(Uuid::from_u128(1));
    store
        .insert_contact(agent, &contact("Acme", 1, ContactState::Active))
        .await
        .unwrap();
    let twin = store
        .insert_contact(agent, &contact("Acme Ltd", 1, ContactState::Active))
        .await;
    assert!(twin.is_err());
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn pause_survives_and_defaults_to_running(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    let agent = AgentId::new(Uuid::from_u128(1));
    assert!(!store.paused(agent).await.unwrap());
    store.set_paused(agent, true).await.unwrap();
    assert!(store.paused(agent).await.unwrap());
    store.set_paused(agent, false).await.unwrap();
    assert!(!store.paused(agent).await.unwrap());
}
