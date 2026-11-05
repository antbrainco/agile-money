//! Policy data: versioned, human-edited rule lists (rule 9). Evaluation lives in `agile-money-policy`.

use jiff::{SignedDuration, Timestamp, civil::Time};
use serde::{Deserialize, Serialize};

use crate::{Address, AgentId, Amount, Asset, PolicyId, TimeZoneName};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    pub id: PolicyId,
    pub version: u32,
    pub agent: AgentId,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Rule {
    MaxPerTx {
        amount: Amount,
    },
    /// Rolling window, e.g. 24h.
    MaxPerWindow {
        amount: Amount,
        window: SignedDuration,
    },
    AllowRecipients {
        addresses: Vec<Address>,
    },
    AllowAssets {
        assets: Vec<Asset>,
    },
    ApprovalAbove {
        amount: Amount,
    },
    QuietHours {
        from: Time,
        to: Time,
        tz: TimeZoneName,
    },
    /// The first payment to an address waits for the owner, whatever the
    /// amount: a newly saved or mistyped contact can't be paid unseen.
    ApproveFirstPayment,
    /// After `count` payments in the window, any asset, each further one
    /// waits for the owner however small: injected text can't drain saved
    /// contacts a little at a time.
    ApprovalAfterCount {
        count: u32,
    },
}

/// What rules read instead of the database, for the intent's asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendSnapshot {
    pub settled: Amount,
    /// Approved or submitted, not yet confirmed (rule 3).
    pub reserved: Amount,
    pub window_start: Timestamp,
    /// A payment to this intent's recipient has been confirmed before.
    #[serde(default)]
    pub recipient_paid: bool,
    /// Payments sent or on their way in the window, any asset.
    #[serde(default)]
    pub payments: u32,
}
