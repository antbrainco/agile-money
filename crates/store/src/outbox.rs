//! `Outbox` on Postgres. `FOR UPDATE SKIP LOCKED` lets several executors claim
//! jobs without blocking each other; the lease returns a crashed worker's job.

use agile_money_domain::ports::{Outbox, OutboxJob, StoreError};
use async_trait::async_trait;
use jiff::SignedDuration;

use crate::PgStore;
use crate::event_store::backend;

#[async_trait]
impl Outbox for PgStore {
    async fn claim(&self, lease: SignedDuration) -> Result<Option<OutboxJob>, StoreError> {
        let row = sqlx::query!(
            r#"UPDATE outbox
               SET locked_until = now() + make_interval(secs => $1), attempts = attempts + 1
               WHERE id = (
                   SELECT id FROM outbox
                   WHERE done_at IS NULL AND next_at <= now()
                     AND (locked_until IS NULL OR locked_until < now())
                   ORDER BY id
                   FOR UPDATE SKIP LOCKED
                   LIMIT 1
               )
               RETURNING id, body, attempts"#,
            lease.as_secs_f64(),
        )
        .fetch_optional(self.pool())
        .await
        .map_err(backend)?;
        row.map(|row| {
            Ok(OutboxJob {
                id: row.id,
                command: serde_json::from_value(row.body).map_err(backend)?,
                attempts: u32::try_from(row.attempts).map_err(backend)?,
            })
        })
        .transpose()
    }

    async fn complete(&self, id: i64) -> Result<(), StoreError> {
        sqlx::query!(
            "UPDATE outbox SET done_at = now(), locked_until = NULL WHERE id = $1",
            id
        )
        .execute(self.pool())
        .await
        .map_err(backend)?;
        Ok(())
    }

    async fn retry_later(&self, id: i64, delay: SignedDuration) -> Result<(), StoreError> {
        sqlx::query!(
            r#"UPDATE outbox SET next_at = now() + make_interval(secs => $2), locked_until = NULL
               WHERE id = $1"#,
            id,
            delay.as_secs_f64(),
        )
        .execute(self.pool())
        .await
        .map_err(backend)?;
        Ok(())
    }
}
