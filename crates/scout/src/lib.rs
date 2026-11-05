//! Web research for the agent: Tavily search via Bifrost. Every result is `Untrusted` (rule 14).

use agile_money_domain::Untrusted;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("search failed: {0}")]
    Search(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub url: String,
    pub title: Untrusted<String>,
    pub snippet: Untrusted<String>,
}
