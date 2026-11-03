//! Human approvals: bound to an intent hash (rule 7), expiring after 15 minutes (rule 8).

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::{ApprovalId, Approved, Error, Intent, IntentHash, IntentId, Proposed};

pub const DEFAULT_APPROVAL_TTL: SignedDuration = SignedDuration::from_mins(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Telegram,
    Dashboard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Granted,
    Rejected,
    Expired,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalTrigger {
    /// A human granted it and the intent hash still matches.
    Granted,
    Rejected,
    Expired,
    /// The intent changed, so its hash no longer matches.
    Superseded,
}

impl ApprovalState {
    pub fn transition(self, trigger: ApprovalTrigger) -> Result<ApprovalState, Error> {
        match (self, trigger) {
            (Self::Pending, ApprovalTrigger::Granted) => Ok(Self::Granted),
            (Self::Pending, ApprovalTrigger::Rejected) => Ok(Self::Rejected),
            (Self::Pending, ApprovalTrigger::Expired) => Ok(Self::Expired),
            (Self::Pending, ApprovalTrigger::Superseded) => Ok(Self::Superseded),
            (from, trigger) => Err(Error::illegal("approval", from, trigger)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub id: ApprovalId,
    pub intent: IntentId,
    pub intent_hash: IntentHash,
    pub state: ApprovalState,
    pub expires_at: Timestamp,
    pub channel: Channel,
}

impl Approval {
    pub fn new(
        id: ApprovalId,
        intent: &Intent<Proposed>,
        channel: Channel,
        now: Timestamp,
    ) -> Result<Self, Error> {
        Ok(Self {
            id,
            intent: intent.id,
            intent_hash: intent.hash()?,
            state: ApprovalState::Pending,
            expires_at: now
                .checked_add(DEFAULT_APPROVAL_TTL)
                .map_err(|_| Error::Overflow)?,
            channel,
        })
    }

    /// The only path from a human "yes" to an `Intent<Approved>`. Expired or
    /// mismatched approvals move to their terminal state and return an error.
    pub fn grant(
        &mut self,
        intent: Intent<Proposed>,
        now: Timestamp,
    ) -> Result<Intent<Approved>, Error> {
        if now >= self.expires_at {
            self.state = self.state.transition(ApprovalTrigger::Expired)?;
            return Err(Error::ApprovalExpired);
        }
        if intent.id != self.intent || intent.hash()? != self.intent_hash {
            self.state = self.state.transition(ApprovalTrigger::Superseded)?;
            return Err(Error::HashMismatch);
        }
        self.state = self.state.transition(ApprovalTrigger::Granted)?;
        Ok(intent.approve())
    }
}
