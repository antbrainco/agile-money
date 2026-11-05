//! Each `#[sqlx::test]` gets a fresh database with `migrations/` applied; needs `DATABASE_URL`.

use sqlx::PgPool;
use warden_store::PgStore;

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn creates_all_tables(pool: PgPool) {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name <> '_sqlx_migrations'
         ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        tables,
        [
            "approval_cards",
            "contacts",
            "events",
            "idempotency",
            "intents_view",
            "outbox",
            "owner_codes",
            "owner_telegram",
            "owner_wallets",
            "policies",
            "switches"
        ]
    );
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn events_are_append_only(pool: PgPool) {
    sqlx::query(
        "INSERT INTO events (stream, version, type, body) VALUES ('intent:x', 1, 'intent_proposed', '{}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let update = sqlx::query("UPDATE events SET type = 'tampered'")
        .execute(&pool)
        .await;
    let delete = sqlx::query("DELETE FROM events").execute(&pool).await;
    assert!(update.unwrap_err().to_string().contains("append-only"));
    assert!(delete.unwrap_err().to_string().contains("append-only"));
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn idempotency_key_is_unique(pool: PgPool) {
    let insert = "INSERT INTO idempotency (key, intent_id) VALUES ('k1', gen_random_uuid())";
    sqlx::query(insert).execute(&pool).await.unwrap();
    assert!(sqlx::query(insert).execute(&pool).await.is_err());
}

#[sqlx::test(migrator = "warden_store::MIGRATOR")]
async fn store_pings_and_migrates_idempotently(pool: PgPool) {
    let store = PgStore::from_pool(pool);
    store.ping().await.unwrap();
    store.migrate().await.unwrap();
}
