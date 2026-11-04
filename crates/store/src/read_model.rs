//! `ReadModel` over `intents_view` and `policies`.

use async_trait::async_trait;
use jiff::Timestamp;
use warden_domain::ports::{EventStore, ReadModel, StoreError};
use warden_domain::{
    Address, AgentId, Amount, ApprovalId, Asset, Event, EventEnvelope, IdempotencyKey, IntentId,
    Invoice, PaymentRequest, Policy, SpendSnapshot, StreamId,
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
                 COALESCE(SUM(amount_minor) FILTER (WHERE state = ANY($4)), 0)::text AS "reserved!",
                 (SELECT COUNT(*) FROM intents_view
                  WHERE agent_id = $1 AND created_at >= $3
                    AND (state = 'confirmed' OR state = ANY($4)))::int4 AS "payments!"
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
            recipient_paid: false,
            payments: u32::try_from(row.payments).unwrap_or(u32::MAX),
        })
    }

    async fn paid_before(&self, agent: AgentId, to: &Address) -> Result<bool, StoreError> {
        let to = serde_json::to_value(to).map_err(backend)?;
        let paid = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                 SELECT 1 FROM intents_view v
                 JOIN events e ON e.type = 'intent_proposed'
                   AND e.body->>'intent' = v.intent_id::text
                 WHERE v.agent_id = $1 AND v.state = 'confirmed'
                   AND e.body->'proposal'->'to'->'address' = $2
               ) AS "paid!""#,
            agent.as_uuid(),
            to,
        )
        .fetch_one(self.pool())
        .await
        .map_err(backend)?;
        Ok(paid)
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

    /// Stores a new policy version. Policies are human-edited (rule 9): the bot
    /// seeds one from config at startup, and the dashboard saves the owner's edits.
    pub async fn save_policy(
        &self,
        policy: &Policy,
        by: warden_domain::UserId,
        source: PolicySource,
    ) -> Result<(), StoreError> {
        let body = serde_json::to_value(policy).map_err(backend)?;
        sqlx::query!(
            r#"INSERT INTO policies (policy_id, version, agent_id, body, created_by, source)
               VALUES ($1, $2, $3, $4, $5, $6)
               ON CONFLICT (policy_id, version) DO NOTHING"#,
            policy.id.as_uuid(),
            i32::try_from(policy.version).map_err(backend)?,
            policy.agent.as_uuid(),
            body,
            by.as_uuid(),
            source.as_str(),
        )
        .execute(self.pool())
        .await
        .map_err(backend)?;
        Ok(())
    }

    /// The latest version seeded from config, to tell whether the file changed.
    pub async fn config_policy(&self, agent: AgentId) -> Result<Option<Policy>, StoreError> {
        let body = sqlx::query_scalar!(
            r#"SELECT body FROM policies WHERE agent_id = $1 AND source = 'config'
               ORDER BY version DESC LIMIT 1"#,
            agent.as_uuid(),
        )
        .fetch_optional(self.pool())
        .await
        .map_err(backend)?;
        body.map(serde_json::from_value)
            .transpose()
            .map_err(backend)
    }

    /// Every policy version, newest first.
    pub async fn policy_versions(&self, agent: AgentId) -> Result<Vec<PolicyVersion>, StoreError> {
        let rows = sqlx::query!(
            r#"SELECT version, source, created_at AS "created_at: jiff_sqlx::Timestamp"
               FROM policies WHERE agent_id = $1 ORDER BY version DESC"#,
            agent.as_uuid(),
        )
        .fetch_all(self.pool())
        .await
        .map_err(backend)?;
        rows.into_iter()
            .map(|r| {
                Ok(PolicyVersion {
                    version: u32::try_from(r.version).map_err(backend)?,
                    source: r.source.parse().map_err(backend)?,
                    created_at: r.created_at.to_jiff(),
                })
            })
            .collect()
    }

    /// The newest `limit` intent streams, newest first.
    pub async fn recent_intents(
        &self,
        agent: AgentId,
        limit: i64,
    ) -> Result<Vec<(IntentId, Vec<EventEnvelope>)>, StoreError> {
        let ids = sqlx::query_scalar!(
            "SELECT intent_id FROM intents_view WHERE agent_id = $1 ORDER BY created_at DESC LIMIT $2",
            agent.as_uuid(),
            limit,
        )
        .fetch_all(self.pool())
        .await
        .map_err(backend)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids.into_iter().map(IntentId::new) {
            out.push((id, self.load(&StreamId::Intent(id)).await?));
        }
        Ok(out)
    }

    /// The newest `limit` invoice streams, newest first.
    pub async fn recent_invoices(&self, limit: i64) -> Result<Vec<Vec<EventEnvelope>>, StoreError> {
        let streams = sqlx::query_scalar!(
            "SELECT stream FROM events WHERE type = 'invoice_issued' ORDER BY seq DESC LIMIT $1",
            limit,
        )
        .fetch_all(self.pool())
        .await
        .map_err(backend)?;
        let mut out = Vec::with_capacity(streams.len());
        for stream in streams {
            let stream: StreamId = stream.parse().map_err(backend)?;
            out.push(self.load(&stream).await?);
        }
        Ok(out)
    }
}

/// Who wrote a policy version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicySource {
    /// `warden.toml`, at startup.
    Config,
    /// The owner, on the dashboard.
    Dashboard,
    /// The recipient allowlist, rewritten when the active contacts change.
    Contacts,
}

impl PolicySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Dashboard => "dashboard",
            Self::Contacts => "contacts",
        }
    }
}

impl std::str::FromStr for PolicySource {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "config" => Ok(Self::Config),
            "dashboard" => Ok(Self::Dashboard),
            "contacts" => Ok(Self::Contacts),
            other => Err(format!("unknown policy source {other:?}")),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PolicyVersion {
    pub version: u32,
    pub source: PolicySource,
    pub created_at: Timestamp,
}
