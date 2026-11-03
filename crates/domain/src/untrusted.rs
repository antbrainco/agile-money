//! Marker for outside input (rule 14): DMs, emails, web pages, search results, onchain memos.

use serde::{Deserialize, Serialize};

/// Data from outside Warden. It can be read, never trusted to raise limits,
/// extend allowlists or skip approvals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Untrusted<T>(T);

impl<T> Untrusted<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    pub fn as_inner(&self) -> &T {
        &self.0
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}
