//! `ReadModel` over `intents_view` and `policies`.

use async_trait::async_trait;
use jiff::Timestamp;
use warden_domain::ports::{ReadModel, StoreError};
use warden_domain::{
    AgentId, Amount, ApprovalId, Asset, Event, IdempotencyKey, IntentId, Invoice, PaymentRequest,
    Policy, SpendSnapshot, StreamId,
};

use crate::PgStore;
use crate::event_store::backend;

/// Intent states whose amount is committed but not yet settled (rule 3).
const RESERVED: &[&str] = &["approved", "submitted", "failed"];

#[async_trait]
impl ReadModel for PgStore {
    async fn policy(&self, agent: AgentId) -> Result<Option<Policy>, StoreError> {
        let body = sqlx::query_scalar!(
            "SELECT body FROM policies WHERE agent_id = $1 ORDER BY version DESC LIMIT 1",
            agent.as_uuid(),
        )
        .fetch_optional(self.pool())
        .await
        .map_err(backend)?;
        body.map(serde_json::from_value)
            .transpose()
            .map_err(backend)
    }

    async fn spend(
        &self,
        agent: AgentId,
        asset: &Asset,
        since: Timestamp,
    ) -> Result<SpendSnapshot, StoreError> {
        let key = serde_json::to_string(asset).map_err(backend)?;
        let row = sqlx::query!(
            r#"SELECT
                 COALESCE(SUM(amount_minor) FILTER (WHERE state = 'confirmed'), 0)::text AS "settled!",
                 COALESCE(SUM(amount_minor) FILTER (WHERE state = ANY($4)), 0)::text AS "reserved!"
               FROM intents_view
               WHERE agent_id = $1 AND asset = $2 AND created_at >= $3"#,
            agent.as_uuid(),
            key,
            jiff_sqlx::Timestamp::from(since) as _,
            RESERVED as &[&str],
        )
        .fetch_one(self.pool())
        .await
        .map_err(backend)?;
        Ok(SpendSnapshot {
            settled: Amount::new(row.settled.parse().map_err(backend)?, asset.clone()),
            reserved: Amount::new(row.reserved.parse().map_err(backend)?, asset.clone()),
            window_start: since,
        })
    }

    async fn intent_by_key(&self, key: &IdempotencyKey) -> Result<Option<IntentId>, StoreError> {
        let id = sqlx::query_scalar!(
            "SELECT intent_id FROM idempotency WHERE key = $1",
            key.as_str()
        )
        .fetch_optional(self.pool())
        .await
        .map_err(backend)?;
        Ok(id.map(IntentId::new))
    }

    async fn awaiting_approval(&self) -> Result<Vec<IntentId>, StoreError> {
        let ids = sqlx::query_scalar!(
            "SELECT intent_id FROM intents_view WHERE state = 'awaiting_approval' ORDER BY created_at"
        )
        .fetch_all(self.pool())
        .await
        .map_err(backend)?;
        Ok(ids.into_iter().map(IntentId::new).collect())
    }

    async fn approval_intent(&self, approval: ApprovalId) -> Result<Option<IntentId>, StoreError> {
        let stream = sqlx::query_scalar!(
            "SELECT stream FROM events WHERE type = 'approval_requested' AND body->>'approval' = $1 LIMIT 1",
            approval.to_string(),
        )
        .fetch_optional(self.pool())
        .await
        .map_err(backend)?;
        Ok(stream
            .and_then(|s| s.parse::<StreamId>().ok())
            .and_then(|s| s.as_intent()))
    }
}

impl PgStore {
    /// Invoices issued at or after `since` with no payment recorded yet, oldest
    /// first, for the payment watcher.
    pub async fn open_invoices(
        &self,
        since: Timestamp,
    ) -> Result<Vec<(Invoice, PaymentRequest)>, StoreError> {
        let bodies = sqlx::query_scalar!(
            r#"SELECT body FROM events e
               WHERE e.type = 'invoice_issued' AND e.at >= $1
                 AND NOT EXISTS (
                   SELECT 1 FROM events p WHERE p.stream = e.stream
                     AND p.type IN ('invoice_paid', 'invoice_cancelled')
                 )
               ORDER BY e.seq"#,
            jiff_sqlx::Timestamp::from(since) as _,
        )
        .fetch_all(self.pool())
        .await
        .map_err(backend)?;
        bodies
            .into_iter()
            .filter_map(|body| match serde_json::from_value(body) {
                Ok(Event::InvoiceIssued {
                    details, request, ..
                }) => Some(Ok((details, request))),
                Ok(_) => None,
                Err(e) => Some(Err(backend(e))),
            })
            .collect()
    }

    /// Stores a new policy version. Policies are human-edited (rule 9); the bot
    /// seeds one from config at startup.
    pub async fn save_policy(
        &self,
        policy: &Policy,
        by: warden_domain::UserId,
    ) -> Result<(), StoreError> {
        let body = serde_json::to_value(policy).map_err(backend)?;
        sqlx::query!(
            r#"INSERT INTO policies (policy_id, version, agent_id, body, created_by)
               VALUES ($1, $2, $3, $4, $5)
               ON CONFLICT (policy_id, version) DO NOTHING"#,
            policy.id.as_uuid(),
            i32::try_from(policy.version).map_err(backend)?,
            policy.agent.as_uuid(),
            body,
            by.as_uuid(),
        )
        .execute(self.pool())
        .await
        .map_err(backend)?;
        Ok(())
    }
}
