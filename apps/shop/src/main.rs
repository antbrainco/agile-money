//! Tilly's shop agent: Instagram and WhatsApp webhooks on axum.

mod config;

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Router, routing::get};
use tracing_subscriber::EnvFilter;
use warden_store::PgStore;

use crate::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let config = Config::load()?;
    let store = PgStore::connect(&config.database).await?;
    if config.database.migrate_on_start {
        store.migrate().await?;
    }
    let app = Router::new()
        .route("/health", get(health))
        .with_state(store);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    tracing::info!(addr = %listener.local_addr()?, "Tilly shop listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health(State(store): State<PgStore>) -> StatusCode {
    match store.ping().await {
        Ok(()) => StatusCode::OK,
        Err(e) => {
            tracing::warn!(error = %e, "database ping failed");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}
