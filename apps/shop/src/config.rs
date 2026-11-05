//! Layered config: `agile-money.toml`, then `agile-money.local.toml`, then `AGILE_MONEY_*` env vars (`__` nests), then `DATABASE_URL`.

use agile_money_store::DbConfig;
use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub database: DbConfig,
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        // A missing .env is fine; real env vars take precedence over it.
        let _ = dotenvy::dotenv();
        let config = Figment::new()
            .merge(Toml::file("agile-money.toml"))
            // Your own deployment: git-ignored, optional, same shape.
            .merge(Toml::file("agile-money.local.toml"))
            .merge(Env::prefixed("AGILE_MONEY_").split("__"))
            .merge(
                Env::raw()
                    .only(&["DATABASE_URL"])
                    .map(|_| "database.url".into()),
            )
            .extract()?;
        Ok(config)
    }
}
