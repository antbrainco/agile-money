//! The only code that signs. Polls the outbox, checks the pause flag and idempotency key,
//! submits `Intent<Approved>` to its rail, and a watcher records settlement.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("executor is paused")]
    Paused,
    #[error(transparent)]
    Rail(#[from] warden_domain::ports::RailError),
    #[error(transparent)]
    Store(#[from] warden_domain::ports::StoreError),
}

/// Global kill switch (rule 6), checked before every submit.
#[derive(Debug, Clone, Default)]
pub struct PauseSwitch(Arc<AtomicBool>);

impl PauseSwitch {
    pub fn pause(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.0.store(false, Ordering::SeqCst);
    }

    pub fn ensure_running(&self) -> Result<(), Error> {
        if self.0.load(Ordering::SeqCst) {
            Err(Error::Paused)
        } else {
            Ok(())
        }
    }
}
