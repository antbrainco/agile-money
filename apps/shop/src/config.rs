//! Layered config: `warden.toml`, then `WARDEN_*` env vars (`__` nests), then `DATABASE_URL`.

use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::Deserialize;
use warden_store::DbConfig;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub database: DbConfig,
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        // A missing .env is fine; real env vars take precedence over it.
        let _ = dotenvy::dotenv();
        let config = Figment::new()
            .merge(Toml::file("warden.toml"))
            .merge(Env::prefixed("WARDEN_").split("__"))
            .merge(
                Env::raw()
                    .only(&["DATABASE_URL"])
                    .map(|_| "database.url".into()),
            )
            .extract()?;
        Ok(config)
    }
}
