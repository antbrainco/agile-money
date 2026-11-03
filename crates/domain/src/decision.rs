//! The outcome of running an intent through policy.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Decision {
    Allow,
    RequireApproval { reasons: Vec<String> },
    Deny { reasons: Vec<String> },
}
