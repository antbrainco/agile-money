//! Keeps `intents_view` in step with the log, inside the append transaction, so
//! spend checks never read a projection that lags its events.

use sqlx::{Postgres, Transaction};
use warden_domain::ports::StoreError;
use warden_domain::{Event, Intent, IntentId, IntentState, Proposed, StreamId};

use crate::event_store::backend;

pub(crate) async fn project_intent(
    tx: &mut Transaction<'_, Postgres>,
    stream: &StreamId,
    id: IntentId,
) -> Result<(), StoreError> {
    let events = sqlx::query_scalar!(
        "SELECT body FROM events WHERE stream = $1 ORDER BY version",
        stream.to_string(),
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(backend)?
    .into_iter()
    .map(serde_json::from_value::<Event>)
    .collect::<Result<Vec<_>, _>>()
    .map_err(backend)?;

    let state = IntentState::replay(&events).map_err(backend)?;
    let intent = Intent::<Proposed>::from_history(id, &events).map_err(backend)?;
    let asset = serde_json::to_string(&intent.amount.asset).map_err(backend)?;
    let rail = serde_json::to_value(intent.amount.asset.rail).map_err(backend)?;

    sqlx::query!(
        r#"INSERT INTO intents_view
               (intent_id, agent_id, state, amount_minor, asset, rail, updated_at, created_at, idempotency_key)
           VALUES ($1, $2, $3, $4::text::numeric, $5, $6, now(), $7, $8)
           ON CONFLICT (intent_id) DO UPDATE SET state = EXCLUDED.state, updated_at = now()"#,
        id.as_uuid(),
        intent.agent.as_uuid(),
        state.name(),
        intent.amount.minor.to_string(),
        asset,
        rail.as_str().unwrap_or_default(),
        jiff_sqlx::Timestamp::from(intent.created_at) as _,
        intent.idempotency_key.as_str(),
    )
    .execute(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(())
}
