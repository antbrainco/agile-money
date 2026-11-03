//! `EventStore` on Postgres: events and outbox rows go in one transaction (rule 11),
//! and a unique `(stream, version)` turns concurrent appends into a `Conflict`.

use async_trait::async_trait;
use warden_domain::ports::{EventStore, OutboxCommand, StoreError};
use warden_domain::{Event, EventEnvelope, StreamId};

use crate::PgStore;

const STREAM_VERSION_CONSTRAINT: &str = "events_stream_version";

#[async_trait]
impl EventStore for PgStore {
    #[tracing::instrument(skip_all, fields(stream = %stream, expected_len, events = events.len()))]
    async fn append(
        &self,
        stream: &StreamId,
        expected_len: u64,
        events: Vec<Event>,
        outbox: Vec<OutboxCommand>,
    ) -> Result<Vec<EventEnvelope>, StoreError> {
        let expected = i64::try_from(expected_len).map_err(backend)?;
        let mut tx = self.pool().begin().await.map_err(backend)?;

        // Catches a stale caller up front; the unique constraint catches the race.
        let current = sqlx::query_scalar!(
            r#"SELECT COALESCE(MAX(version), 0) AS "version!" FROM events WHERE stream = $1"#,
            stream.to_string(),
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(backend)?;
        if current != expected {
            return Err(StoreError::Conflict(*stream));
        }

        // Rule 10: one intent per idempotency key, enforced in the same transaction.
        for event in &events {
            if let Event::IntentProposed {
                intent, proposal, ..
            } = event
            {
                let inserted = sqlx::query!(
                    "INSERT INTO idempotency (key, intent_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                    proposal.idempotency_key.as_str(),
                    intent.as_uuid(),
                )
                .execute(&mut *tx)
                .await
                .map_err(backend)?;
                if inserted.rows_affected() == 0 {
                    return Err(StoreError::Duplicate(proposal.idempotency_key.clone()));
                }
            }
        }

        let mut envelopes = Vec::with_capacity(events.len());
        for (version, event) in (expected + 1..).zip(events) {
            let body = serde_json::to_value(&event).map_err(backend)?;
            let row = sqlx::query!(
                r#"INSERT INTO events (stream, version, type, body)
                   VALUES ($1, $2, $3, $4)
                   RETURNING seq, at AS "at: jiff_sqlx::Timestamp""#,
                stream.to_string(),
                version,
                event.kind(),
                body,
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| conflict_or_backend(e, stream))?;
            envelopes.push(EventEnvelope {
                seq: row.seq,
                stream: *stream,
                at: row.at.to_jiff(),
                event,
            });
        }

        for command in outbox {
            let body = serde_json::to_value(&command).map_err(backend)?;
            sqlx::query!(
                "INSERT INTO outbox (intent_id, kind, body) VALUES ($1, $2, $3)",
                command.intent().as_uuid(),
                command.kind(),
                body,
            )
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        }

        if let Some(intent) = stream.as_intent() {
            crate::projection::project_intent(&mut tx, stream, intent).await?;
        }

        tx.commit().await.map_err(backend)?;
        Ok(envelopes)
    }

    #[tracing::instrument(skip_all, fields(stream = %stream))]
    async fn load(&self, stream: &StreamId) -> Result<Vec<EventEnvelope>, StoreError> {
        let rows = sqlx::query!(
            r#"SELECT seq, at AS "at: jiff_sqlx::Timestamp", body
               FROM events WHERE stream = $1 ORDER BY version"#,
            stream.to_string(),
        )
        .fetch_all(self.pool())
        .await
        .map_err(backend)?;

        rows.into_iter()
            .map(|row| {
                Ok(EventEnvelope {
                    seq: row.seq,
                    stream: *stream,
                    at: row.at.to_jiff(),
                    event: serde_json::from_value(row.body).map_err(backend)?,
                })
            })
            .collect()
    }
}

fn conflict_or_backend(err: sqlx::Error, stream: &StreamId) -> StoreError {
    match &err {
        sqlx::Error::Database(db) if db.constraint() == Some(STREAM_VERSION_CONSTRAINT) => {
            StoreError::Conflict(*stream)
        }
        _ => backend(err),
    }
}

pub(crate) fn backend(err: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(err.to_string())
}
