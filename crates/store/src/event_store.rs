//! `EventStore` on Postgres: events and outbox rows go in one transaction (rule 11),
//! and a unique `(stream, version)` turns concurrent appends into a `Conflict`.

use async_trait::async_trait;
use warden_domain::ports::{EventStore, OutboxCommand, StoreError};
use warden_domain::{Event, EventEnvelope, StreamId};

use crate::PgStore;

const STREAM_VERSION_CONSTRAINT: &str = "events_stream_version";

#[async_trait]
impl EventStore for PgStore {
    #[tracing::instrument(skip_all, fields(stream = stream.as_str(), expected_len, events = events.len()))]
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
            stream.as_str(),
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(backend)?;
        if current != expected {
            return Err(StoreError::Conflict(stream.as_str().to_owned()));
        }

        let mut envelopes = Vec::with_capacity(events.len());
        for (version, event) in (expected + 1..).zip(events) {
            let body = serde_json::to_value(&event).map_err(backend)?;
            let row = sqlx::query!(
                r#"INSERT INTO events (stream, version, type, body)
                   VALUES ($1, $2, $3, $4)
                   RETURNING seq, at AS "at: jiff_sqlx::Timestamp""#,
                stream.as_str(),
                version,
                event.kind(),
                body,
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| conflict_or_backend(e, stream))?;
            envelopes.push(EventEnvelope {
                seq: row.seq,
                stream: stream.clone(),
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

        tx.commit().await.map_err(backend)?;
        Ok(envelopes)
    }

    #[tracing::instrument(skip_all, fields(stream = stream.as_str()))]
    async fn load(&self, stream: &StreamId) -> Result<Vec<EventEnvelope>, StoreError> {
        let rows = sqlx::query!(
            r#"SELECT seq, at AS "at: jiff_sqlx::Timestamp", body
               FROM events WHERE stream = $1 ORDER BY version"#,
            stream.as_str(),
        )
        .fetch_all(self.pool())
        .await
        .map_err(backend)?;

        rows.into_iter()
            .map(|row| {
                Ok(EventEnvelope {
                    seq: row.seq,
                    stream: stream.clone(),
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
            StoreError::Conflict(stream.as_str().to_owned())
        }
        _ => backend(err),
    }
}

fn backend(err: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(err.to_string())
}
