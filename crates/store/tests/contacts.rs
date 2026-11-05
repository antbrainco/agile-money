//! Contacts and the pause switch; needs `DATABASE_URL`.

#![allow(clippy::unwrap_used)] // helpers outside #[test] fns, which clippy.toml does not cover

use agile_money_domain::{Address, AgentId, SolAddress};
use agile_money_store::{Contact, ContactSource, ContactState, PgStore};
use jiff::{SignedDuration, Timestamp};
use sqlx::PgPool;
use uuid::Uuid;

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

#[sqlx::test(migrator = "agile_money_store::MIGRATOR")]
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

#[sqlx::test(migrator = "agile_money_store::MIGRATOR")]
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

#[sqlx::test(migrator = "agile_money_store::MIGRATOR")]
async fn pause_survives_and_defaults_to_running(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    let agent = AgentId::new(Uuid::from_u128(1));
    assert!(!store.paused(agent).await.unwrap());
    store.set_paused(agent, true).await.unwrap();
    assert!(store.paused(agent).await.unwrap());
    store.set_paused(agent, false).await.unwrap();
    assert!(!store.paused(agent).await.unwrap());
}

#[sqlx::test(migrator = "agile_money_store::MIGRATOR")]
async fn a_card_closes_once(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    let approval = Uuid::from_u128(7);
    store.open_card(approval, 42).await.unwrap();
    assert_eq!(store.close_card(approval).await.unwrap(), Some(42));
    assert_eq!(store.close_card(approval).await.unwrap(), None);
}

#[sqlx::test(migrator = "agile_money_store::MIGRATOR")]
async fn an_enrolled_code_is_stored_once(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    let agent = AgentId::new(Uuid::from_u128(1));
    assert_eq!(store.owner_code(agent).await.unwrap(), None);
    assert!(store.save_owner_code(agent, "FIRST").await.unwrap());
    assert!(!store.save_owner_code(agent, "SECOND").await.unwrap());
    assert_eq!(
        store.owner_code(agent).await.unwrap().as_deref(),
        Some("FIRST")
    );
}

#[sqlx::test(migrator = "agile_money_store::MIGRATOR")]
async fn wallets_link_up_to_the_limit_and_unlink(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    let agent = AgentId::new(Uuid::from_u128(1));
    for n in 0..agile_money_store::MAX_WALLETS {
        assert!(
            store
                .link_wallet(agent, &format!("W{n}"), "phone")
                .await
                .unwrap()
        );
    }
    assert!(
        !store
            .link_wallet(agent, "WX", "one too many")
            .await
            .unwrap()
    );
    // Renaming a linked wallet isn't a new one.
    assert!(store.link_wallet(agent, "W0", "laptop").await.unwrap());
    let wallets = store.owner_wallets(agent).await.unwrap();
    assert_eq!(wallets.len() as i64, agile_money_store::MAX_WALLETS);
    assert_eq!(wallets[0].label, "laptop");
    assert!(store.unlink_wallet(agent, "W0").await.unwrap());
    assert!(!store.unlink_wallet(agent, "W0").await.unwrap());
}

#[sqlx::test(migrator = "agile_money_store::MIGRATOR")]
async fn telegram_link_is_replaced(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    let agent = AgentId::new(Uuid::from_u128(1));
    assert_eq!(store.owner_telegram(agent).await.unwrap(), None);
    store.link_telegram(agent, "Ada", None).await.unwrap();
    store
        .link_telegram(agent, "Ada L", Some("ada"))
        .await
        .unwrap();
    let linked = store.owner_telegram(agent).await.unwrap().unwrap();
    assert_eq!(
        (linked.name.as_str(), linked.username.as_deref()),
        ("Ada L", Some("ada"))
    );
}
