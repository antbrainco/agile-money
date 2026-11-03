//! Tilly on Telegram: chat with the agent and answer approval requests.

mod config;

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
    store.ping().await?;
    tracing::info!("Tilly bot starting");
    store.close().await;
    Ok(())
}
