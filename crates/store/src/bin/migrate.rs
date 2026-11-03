//! Applies `migrations/` to `DATABASE_URL` (read from `.env` if present):
//! `cargo run -p warden-store --bin warden-migrate`.

use secrecy::SecretString;
use warden_store::{DbConfig, PgStore};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A missing .env is fine; the real environment wins over it either way.
    let _ = dotenvy::dotenv();
    let url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is not set")?;
    let config = DbConfig {
        url: SecretString::from(url),
        max_connections: 1,
        acquire_timeout_ms: 5_000,
        statement_timeout_ms: 60_000,
        migrate_on_start: true,
    };
    let store = PgStore::connect(&config).await?;
    store.migrate().await?;
    println!("migrations up to date:");
    for m in warden_store::MIGRATOR.iter() {
        println!("{} {}", m.version, m.description);
    }
    store.close().await;
    Ok(())
}
