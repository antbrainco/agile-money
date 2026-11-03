//! Payment intents with their approval state in the type (typestate), plus the
//! replayable `IntentState` machine from the spec's States section.

use std::marker::PhantomData;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    AgentId, Amount, Decision, Error, Event, IdempotencyKey, IntentId, Recipient, RunId, ToolCallId,
};

/// Submissions allowed before an intent is abandoned.
pub const MAX_SUBMIT_ATTEMPTS: u8 = 5;

/// Typestate marker: proposed by the agent, not yet cleared by policy or a human.
#[derive(Debug, Clone, Copy)]
pub struct Proposed;

/// Typestate marker: cleared to be signed. Only `Rail::submit` accepts it.
#[derive(Debug, Clone, Copy)]
pub struct Approved;

/// Which run and tool call proposed an intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub run: RunId,
    pub tool_call: ToolCallId,
}

/// sha256 over rail, asset, amount, recipient and memo, hex-encoded. Approvals bind to it (rule 7).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IntentHash(String);

impl IntentHash {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct Intent<S> {
    pub id: IntentId,
    pub agent: AgentId,
    pub amount: Amount,
    pub to: Recipient,
    pub memo: Option<String>,
    pub idempotency_key: IdempotencyKey,
    pub source: Provenance,
    pub created_at: Timestamp,
    state: PhantomData<S>,
}

impl<S> Intent<S> {
    pub fn hash(&self) -> Result<IntentHash, Error> {
        #[derive(Serialize)]
        struct Bound<'a> {
            amount: &'a Amount,
            to: &'a Recipient,
            memo: &'a Option<String>,
        }
        let bytes = serde_json::to_vec(&Bound {
            amount: &self.amount,
            to: &self.to,
            memo: &self.memo,
        })?;
        Ok(IntentHash(hex::encode(Sha256::digest(&bytes))))
    }

    fn cast<T>(self) -> Intent<T> {
        Intent {
            id: self.id,
            agent: self.agent,
            amount: self.amount,
            to: self.to,
            memo: self.memo,
            idempotency_key: self.idempotency_key,
            source: self.source,
            created_at: self.created_at,
            state: PhantomData,
        }
    }
}

/// An intent's data without its typestate, as stored in `IntentProposed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub agent: AgentId,
    pub amount: Amount,
    pub to: Recipient,
    pub memo: Option<String>,
    pub idempotency_key: IdempotencyKey,
    pub source: Provenance,
    pub created_at: Timestamp,
}

impl<S> Intent<S> {
    /// The `IntentProposed` event for this intent.
    pub fn proposed_event(&self) -> Result<Event, Error> {
        Ok(Event::IntentProposed {
            intent: self.id,
            hash: self.hash()?,
            proposal: Proposal {
                agent: self.agent,
                amount: self.amount.clone(),
                to: self.to.clone(),
                memo: self.memo.clone(),
                idempotency_key: self.idempotency_key.clone(),
                source: self.source.clone(),
                created_at: self.created_at,
            },
        })
    }
}

/// Finds the proposal in a stream and checks it still matches its recorded hash.
fn proposal<'a>(
    id: IntentId,
    events: impl IntoIterator<Item = &'a Event>,
) -> Result<Intent<Proposed>, Error> {
    let (hash, p) = events
        .into_iter()
        .find_map(|e| match e {
            Event::IntentProposed {
                intent,
                hash,
                proposal,
            } if *intent == id => Some((hash, proposal)),
            _ => None,
        })
        .ok_or(Error::MissingProposal(id))?;
    let intent = Intent::new(
        id,
        p.agent,
        p.amount.clone(),
        p.to.clone(),
        p.memo.clone(),
        p.idempotency_key.clone(),
        p.source.clone(),
        p.created_at,
    );
    if intent.hash()? != *hash {
        return Err(Error::TamperedProposal);
    }
    Ok(intent)
}

impl Intent<Approved> {
    /// Rebuild an approved intent from its event stream. Succeeds only when the
    /// replayed state says policy or a human cleared it and it can still be sent
    /// or watched: `Approved`, `Submitted` or a retryable `Failed`.
    pub fn from_history<'a, I>(id: IntentId, events: I) -> Result<(Self, IntentState), Error>
    where
        I: IntoIterator<Item = &'a Event>,
        I::IntoIter: Clone,
    {
        let events = events.into_iter();
        let state = IntentState::replay(events.clone())?;
        match state {
            IntentState::Approved | IntentState::Submitted { .. } => {}
            IntentState::Failed { attempts } if attempts < MAX_SUBMIT_ATTEMPTS => {}
            _ => return Err(Error::NotAllowed),
        }
        Ok((proposal(id, events)?.cast(), state))
    }
}

impl Intent<Proposed> {
    /// Rebuild a proposed intent from its event stream, e.g. to grant an approval.
    pub fn from_history<'a>(
        id: IntentId,
        events: impl IntoIterator<Item = &'a Event>,
    ) -> Result<Self, Error> {
        proposal(id, events)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: IntentId,
        agent: AgentId,
        amount: Amount,
        to: Recipient,
        memo: Option<String>,
        idempotency_key: IdempotencyKey,
        source: Provenance,
        created_at: Timestamp,
    ) -> Self {
        Self {
            id,
            agent,
            amount,
            to,
            memo,
            idempotency_key,
            source,
            created_at,
            state: PhantomData,
        }
    }

    /// Promote on a policy `Allow`. Used by `PolicyEngine::decide`.
    pub fn allow(self, decision: &Decision) -> Result<Intent<Approved>, Error> {
        match decision {
            Decision::Allow => Ok(self.cast()),
            _ => Err(Error::NotAllowed),
        }
    }

    /// Promote on a granted human approval; see `Approval::grant`.
    pub(crate) fn approve(self) -> Intent<Approved> {
        self.cast()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IntentState {
    Proposed,
    AwaitingApproval,
    Approved,
    /// Both carry the number of submit attempts so far, so the executor can tell
    /// a retry from the final attempt.
    Submitted {
        attempts: u8,
    },
    Failed {
        attempts: u8,
    },
    Confirmed,
    Denied,
    Rejected,
    Abandoned,
}

impl IntentState {
    pub fn transition(self, event: &Event) -> Result<IntentState, Error> {
        use IntentState as S;
        let next = match (self, event) {
            (S::Proposed, Event::IntentDecided { decision, .. }) => match decision {
                Decision::Allow => S::Approved,
                Decision::RequireApproval { .. } => S::AwaitingApproval,
                Decision::Deny { .. } => S::Denied,
            },
            // Records who was asked; the state already says we are waiting.
            (S::AwaitingApproval, Event::ApprovalRequested { .. }) => S::AwaitingApproval,
            (S::AwaitingApproval, Event::ApprovalGranted { .. }) => S::Approved,
            (S::AwaitingApproval, Event::ApprovalRejected { .. }) => S::Rejected,
            (S::Approved, Event::PaymentSubmitted { .. }) => S::Submitted { attempts: 1 },
            (S::Failed { attempts }, Event::PaymentSubmitted { .. })
                if attempts < MAX_SUBMIT_ATTEMPTS =>
            {
                S::Submitted {
                    attempts: attempts + 1,
                }
            }
            // The send itself failed, so nothing reached the chain; still an attempt.
            (S::Approved, Event::PaymentFailed { .. }) => S::Failed { attempts: 1 },
            (S::Failed { attempts }, Event::PaymentFailed { .. })
                if attempts < MAX_SUBMIT_ATTEMPTS =>
            {
                S::Failed {
                    attempts: attempts + 1,
                }
            }
            (S::Submitted { .. }, Event::PaymentConfirmed { .. }) => S::Confirmed,
            (S::Submitted { attempts }, Event::PaymentFailed { .. }) => S::Failed { attempts },
            (S::Failed { .. }, Event::PaymentAbandoned { .. }) => S::Abandoned,
            (from, trigger) => return Err(Error::illegal("intent", from, trigger.kind())),
        };
        Ok(next)
    }

    /// Rebuild state from an intent's event stream.
    pub fn replay<'a>(events: impl IntoIterator<Item = &'a Event>) -> Result<IntentState, Error> {
        events
            .into_iter()
            .filter(|e| !matches!(e, Event::IntentProposed { .. }))
            .try_fold(IntentState::Proposed, |state, event| {
                state.transition(event)
            })
    }

    /// Lowercase name, as stored in `intents_view.state`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Approved => "approved",
            Self::Submitted { .. } => "submitted",
            Self::Failed { .. } => "failed",
            Self::Confirmed => "confirmed",
            Self::Denied => "denied",
            Self::Rejected => "rejected",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Confirmed | Self::Denied | Self::Rejected | Self::Abandoned
        )
    }
}
