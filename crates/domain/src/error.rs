//! Errors raised by domain validation and state transitions.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("illegal {machine} transition from {from} on {trigger}")]
    IllegalTransition {
        machine: &'static str,
        from: String,
        trigger: String,
    },
    #[error("asset mismatch: expected {expected}, got {actual}")]
    AssetMismatch { expected: String, actual: String },
    #[error("amount overflow")]
    Overflow,
    #[error("approval does not bind to this intent")]
    HashMismatch,
    #[error("approval expired")]
    ApprovalExpired,
    #[error("decision is not allow")]
    NotAllowed,
    #[error("hashing intent failed: {0}")]
    Hash(#[from] serde_json::Error),
}

impl Error {
    pub(crate) fn illegal(
        machine: &'static str,
        from: impl std::fmt::Debug,
        trigger: impl std::fmt::Debug,
    ) -> Self {
        Self::IllegalTransition {
            machine,
            from: format!("{from:?}"),
            trigger: format!("{trigger:?}"),
        }
    }
}
