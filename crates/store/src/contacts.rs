//! Saved payees and the pause switch.

use jiff::Timestamp;
use uuid::Uuid;
use warden_domain::ports::StoreError;
use warden_domain::{Address, AgentId};

use crate::PgStore;
use crate::event_store::backend;

/// Where a contact is in its life. Only `Active` contacts can be paid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContactState {
    /// Waiting for the owner's code.
    Proposed,
    /// Code given; goes live at `active_at`.
    Confirmed,
    Active,
    Cancelled,
    Removed,
    /// Never confirmed in time.
    Expired,
}

impl ContactState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Confirmed => "confirmed",
            Self::Active => "active",
            Self::Cancelled => "cancelled",
            Self::Removed => "removed",
            Self::Expired => "expired",
        }
    }

    fn closed(self) -> bool {
        matches!(self, Self::Cancelled | Self::Removed | Self::Expired)
    }
}

impl std::str::FromStr for ContactState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "proposed" => Self::Proposed,
            "confirmed" => Self::Confirmed,
            "active" => Self::Active,
            "cancelled" => Self::Cancelled,
            "removed" => Self::Removed,
            "expired" => Self::Expired,
            other => return Err(format!("unknown contact state {other:?}")),
        })
    }
}

/// Who asked for a contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContactSource {
    /// `[[contacts]]` in `warden.toml`: whoever edits it already holds the keys.
    Config,
    Telegram,
    Dashboard,
    /// Suggested by the agent in chat; the owner still confirms it.
    Agent,
}

impl ContactSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Telegram => "telegram",
            Self::Dashboard => "dashboard",
            Self::Agent => "agent",
        }
    }
}

impl std::str::FromStr for ContactSource {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "config" => Self::Config,
            "telegram" => Self::Telegram,
            "dashboard" => Self::Dashboard,
            "agent" => Self::Agent,
            other => return Err(format!("unknown contact source {other:?}")),
        })
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Contact {
    pub id: Uuid,
    pub label: String,
    pub address: Address,
    pub state: ContactState,
    pub source: ContactSource,
    pub replaces: Option<Uuid>,
    pub proposed_at: Timestamp,
    pub confirmed_at: Option<Timestamp>,
    pub active_at: Option<Timestamp>,
    pub closed_at: Option<Timestamp>,
}

struct Row {
    contact_id: Uuid,
    label: String,
    address: serde_json::Value,
    state: String,
    source: String,
    replaces: Option<Uuid>,
    proposed_at: jiff_sqlx::Timestamp,
    confirmed_at: Option<jiff_sqlx::Timestamp>,
    active_at: Option<jiff_sqlx::Timestamp>,
    closed_at: Option<jiff_sqlx::Timestamp>,
}

impl TryFrom<Row> for Contact {
    type Error = StoreError;
    fn try_from(r: Row) -> Result<Self, StoreError> {
        Ok(Self {
            id: r.contact_id,
            label: r.label,
            address: serde_json::from_value(r.address).map_err(backend)?,
            state: r.state.parse().map_err(backend)?,
            source: r.source.parse().map_err(backend)?,
            replaces: r.replaces,
            proposed_at: r.proposed_at.to_jiff(),
            confirmed_at: r.confirmed_at.map(|t| t.to_jiff()),
            active_at: r.active_at.map(|t| t.to_jiff()),
            closed_at: r.closed_at.map(|t| t.to_jiff()),
        })
    }
}

impl PgStore {
    /// Stores a new contact as given.
    pub async fn insert_contact(&self, agent: AgentId, c: &Contact) -> Result<(), StoreError> {
        sqlx::query!(
            r#"INSERT INTO contacts (contact_id, agent_id, label, address, state, source,
                                     replaces, proposed_at, confirmed_at, active_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
            c.id,
            agent.as_uuid(),
            c.label,
            serde_json::to_value(&c.address).map_err(backend)?,
            c.state.as_str(),
            c.source.as_str(),
            c.replaces,
            jiff_sqlx::Timestamp::from(c.proposed_at) as _,
            c.confirmed_at.map(jiff_sqlx::Timestamp::from) as _,
            c.active_at.map(jiff_sqlx::Timestamp::from) as _,
        )
        .execute(self.pool())
        .await
        .map_err(backend)?;
        Ok(())
    }

    /// Active and pending contacts, by label.
    pub async fn contacts(&self, agent: AgentId) -> Result<Vec<Contact>, StoreError> {
        let rows = sqlx::query_as!(
            Row,
            r#"SELECT contact_id, label, address, state, source, replaces,
                      proposed_at AS "proposed_at: jiff_sqlx::Timestamp",
                      confirmed_at AS "confirmed_at: jiff_sqlx::Timestamp",
                      active_at AS "active_at: jiff_sqlx::Timestamp",
                      closed_at AS "closed_at: jiff_sqlx::Timestamp"
               FROM contacts
               WHERE agent_id = $1 AND state IN ('proposed', 'confirmed', 'active')
               ORDER BY lower(label), proposed_at"#,
            agent.as_uuid(),
        )
        .fetch_all(self.pool())
        .await
        .map_err(backend)?;
        rows.into_iter().map(Contact::try_from).collect()
    }

    pub async fn contact(&self, id: Uuid) -> Result<Option<Contact>, StoreError> {
        let row = sqlx::query_as!(
            Row,
            r#"SELECT contact_id, label, address, state, source, replaces,
                      proposed_at AS "proposed_at: jiff_sqlx::Timestamp",
                      confirmed_at AS "confirmed_at: jiff_sqlx::Timestamp",
                      active_at AS "active_at: jiff_sqlx::Timestamp",
                      closed_at AS "closed_at: jiff_sqlx::Timestamp"
               FROM contacts WHERE contact_id = $1"#,
            id,
        )
        .fetch_optional(self.pool())
        .await
        .map_err(backend)?;
        row.map(Contact::try_from).transpose()
    }

    /// Moves a contact to `to` if it is still in one of `from`, and says whether
    /// it moved: two answers racing for the same contact can't both win.
    /// Entering `Confirmed` sets `confirmed_at` and `active_at`; a closed state
    /// sets `closed_at`.
    pub async fn move_contact(
        &self,
        id: Uuid,
        from: &[ContactState],
        to: ContactState,
        active_at: Option<Timestamp>,
        now: Timestamp,
    ) -> Result<bool, StoreError> {
        let from: Vec<&str> = from.iter().map(|s| s.as_str()).collect();
        let done = sqlx::query!(
            r#"UPDATE contacts SET
                 state = $3,
                 confirmed_at = CASE WHEN $3 = 'confirmed' THEN $5 ELSE confirmed_at END,
                 active_at = COALESCE($4, active_at),
                 closed_at = CASE WHEN $6 THEN $5 ELSE closed_at END
               WHERE contact_id = $1 AND state = ANY($2)"#,
            id,
            &from as &[&str],
            to.as_str(),
            active_at.map(jiff_sqlx::Timestamp::from) as _,
            jiff_sqlx::Timestamp::from(now) as _,
            to.closed(),
        )
        .execute(self.pool())
        .await
        .map_err(backend)?;
        Ok(done.rows_affected() == 1)
    }

    /// Whether payments are paused. Nothing stored means running.
    pub async fn paused(&self, agent: AgentId) -> Result<bool, StoreError> {
        let paused = sqlx::query_scalar!(
            "SELECT paused FROM switches WHERE agent_id = $1",
            agent.as_uuid()
        )
        .fetch_optional(self.pool())
        .await
        .map_err(backend)?;
        Ok(paused.unwrap_or(false))
    }

    pub async fn set_paused(&self, agent: AgentId, paused: bool) -> Result<(), StoreError> {
        sqlx::query!(
            r#"INSERT INTO switches (agent_id, paused) VALUES ($1, $2)
               ON CONFLICT (agent_id) DO UPDATE SET paused = $2, changed_at = now()"#,
            agent.as_uuid(),
            paused,
        )
        .execute(self.pool())
        .await
        .map_err(backend)?;
        Ok(())
    }
}
