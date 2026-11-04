//! Postgres adapter for the event log, outbox and projections.
//! `PgStore` implements [`warden_domain::ports::EventStore`] with compile-time checked queries.

mod contacts;
mod event_store;
mod outbox;
mod projection;
mod read_model;

pub use contacts::{Contact, ContactSource, ContactState};
pub use read_model::{PolicySource, PolicyVersion};

use std::str::FromStr;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sqlx::PgPool;
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// Migrations from `migrations/` at the workspace root, embedded at compile time.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid database url")]
    Url(#[source] sqlx::Error),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

/// `[database]` in `warden.toml`; the url comes from `DATABASE_URL`, never the file.
#[derive(Debug, Clone, Deserialize)]
pub struct DbConfig {
    pub url: SecretString,
    #[serde(default = "defaults::max_connections")]
    pub max_connections: u32,
    #[serde(default = "defaults::acquire_timeout_ms")]
    pub acquire_timeout_ms: u64,
    #[serde(default = "defaults::statement_timeout_ms")]
    pub statement_timeout_ms: u64,
    #[serde(default = "defaults::migrate_on_start")]
    pub migrate_on_start: bool,
}

mod defaults {
    pub fn max_connections() -> u32 {
        10
    }
    pub fn acquire_timeout_ms() -> u64 {
        5_000
    }
    pub fn statement_timeout_ms() -> u64 {
        10_000
    }
    pub fn migrate_on_start() -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    /// Connects eagerly so a bad url or unreachable server fails at startup.
    #[tracing::instrument(skip_all, fields(max_connections = config.max_connections))]
    pub async fn connect(config: &DbConfig) -> Result<Self, Error> {
        let options = PgConnectOptions::from_str(config.url.expose_secret())
            .map_err(Error::Url)?
            .application_name("warden")
            // Server-side cap on every statement, so a stuck query cannot hold a money path.
            .options([(
                "statement_timeout",
                format!("{}ms", config.statement_timeout_ms),
            )]);
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .acquire_timeout(Duration::from_millis(config.acquire_timeout_ms))
            .connect_with(options)
            .await?;
        Ok(Self::from_pool(pool))
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Applies pending migrations. Safe to run from several processes: sqlx takes an advisory lock.
    #[tracing::instrument(skip_all)]
    pub async fn migrate(&self) -> Result<(), Error> {
        MIGRATOR.run(&self.pool).await?;
        tracing::info!("migrations up to date");
        Ok(())
    }

    /// Round-trips to the server; for health checks.
    pub async fn ping(&self) -> Result<(), Error> {
        use sqlx::Connection;
        self.pool.acquire().await?.ping().await?;
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}
