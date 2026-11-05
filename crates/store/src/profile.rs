//! The owner's dashboard profile: wallets that may sign in, and the confirmed
//! Telegram account.

use jiff::Timestamp;
use warden_domain::AgentId;
use warden_domain::ports::StoreError;

use crate::PgStore;
use crate::event_store::backend;

/// Most wallets one owner may link.
pub const MAX_WALLETS: i64 = 5;

/// A wallet that may sign in to the dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerWallet {
    /// Base58 public key.
    pub address: String,
    pub label: String,
    pub linked_at: Timestamp,
}

/// The Telegram account that confirmed the owner chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerTelegram {
    pub name: String,
    pub username: Option<String>,
    pub linked_at: Timestamp,
}

impl PgStore {
    pub async fn owner_wallets(&self, agent: AgentId) -> Result<Vec<OwnerWallet>, StoreError> {
        let rows = sqlx::query!(
            r#"SELECT address, label, linked_at AS "linked_at: jiff_sqlx::Timestamp"
               FROM owner_wallets WHERE agent_id = $1 ORDER BY linked_at"#,
            agent.as_uuid()
        )
        .fetch_all(self.pool())
        .await
        .map_err(backend)?;
        Ok(rows
            .into_iter()
            .map(|r| OwnerWallet {
                address: r.address,
                label: r.label,
                linked_at: r.linked_at.to_jiff(),
            })
            .collect())
    }

    /// Links a wallet, or renames one already linked. False when the owner
    /// already has [`MAX_WALLETS`] others.
    pub async fn link_wallet(
        &self,
        agent: AgentId,
        address: &str,
        label: &str,
    ) -> Result<bool, StoreError> {
        let done = sqlx::query!(
            "INSERT INTO owner_wallets (agent_id, address, label)
             SELECT $1, $2, $3
             WHERE (SELECT count(*) FROM owner_wallets WHERE agent_id = $1 AND address <> $2) < $4
             ON CONFLICT (agent_id, address) DO UPDATE SET label = $3",
            agent.as_uuid(),
            address,
            label,
            MAX_WALLETS,
        )
        .execute(self.pool())
        .await
        .map_err(backend)?;
        Ok(done.rows_affected() == 1)
    }

    /// False when the wallet wasn't linked.
    pub async fn unlink_wallet(&self, agent: AgentId, address: &str) -> Result<bool, StoreError> {
        let done = sqlx::query!(
            "DELETE FROM owner_wallets WHERE agent_id = $1 AND address = $2",
            agent.as_uuid(),
            address,
        )
        .execute(self.pool())
        .await
        .map_err(backend)?;
        Ok(done.rows_affected() == 1)
    }

    pub async fn owner_telegram(
        &self,
        agent: AgentId,
    ) -> Result<Option<OwnerTelegram>, StoreError> {
        let row = sqlx::query!(
            r#"SELECT name, username, linked_at AS "linked_at: jiff_sqlx::Timestamp"
               FROM owner_telegram WHERE agent_id = $1"#,
            agent.as_uuid()
        )
        .fetch_optional(self.pool())
        .await
        .map_err(backend)?;
        Ok(row.map(|r| OwnerTelegram {
            name: r.name,
            username: r.username,
            linked_at: r.linked_at.to_jiff(),
        }))
    }

    /// Records (again) who confirmed the owner chat, and when.
    pub async fn link_telegram(
        &self,
        agent: AgentId,
        name: &str,
        username: Option<&str>,
    ) -> Result<(), StoreError> {
        sqlx::query!(
            "INSERT INTO owner_telegram (agent_id, name, username) VALUES ($1, $2, $3)
             ON CONFLICT (agent_id) DO UPDATE SET name = $2, username = $3, linked_at = now()",
            agent.as_uuid(),
            name,
            username,
        )
        .execute(self.pool())
        .await
        .map_err(backend)?;
        Ok(())
    }
}
