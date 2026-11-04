//! The guardrail: runs an intent through its policy's ordered rules and returns a `Decision`.
//! First `Deny` wins, then any `RequireApproval`, else `Allow`. Rules are pure functions.

use async_trait::async_trait;
use jiff::{Timestamp, civil::Time};
use warden_domain::{
    Amount, Approval, Approved, Decision, Intent, Policy, Proposed, Rule, SpendSnapshot,
    TimeZoneName,
};

mod pipeline;

pub use pipeline::{Payments, Proposed as ProposeOutcome, Resolution};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Domain(#[from] warden_domain::Error),
    #[error(transparent)]
    Store(#[from] warden_domain::ports::StoreError),
    #[error("approval channel unavailable: {0}")]
    Channel(String),
    #[error("no approval {0} on this intent")]
    UnknownApproval(String),
    #[error("intent is {0}, not awaiting approval")]
    NotAwaiting(&'static str),
}

/// Sends approval requests to a human (Telegram and the dashboard at once; rule 8).
#[async_trait]
pub trait Approver: Send + Sync {
    async fn request(&self, approval: &Approval, intent: &Intent<Proposed>) -> Result<(), Error>;
}

/// What `PolicyEngine::decide` hands back: either a signable intent or the reason it is not.
#[derive(Debug)]
pub enum Outcome {
    Allowed(Intent<Approved>),
    NeedsApproval(Intent<Proposed>, Vec<String>),
    Denied(Vec<String>),
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PolicyEngine;

impl PolicyEngine {
    pub fn new() -> Self {
        Self
    }

    /// Pure evaluation, for display and property tests.
    pub fn evaluate(
        &self,
        policy: Option<&Policy>,
        intent: &Intent<Proposed>,
        spend: &SpendSnapshot,
        now: Timestamp,
    ) -> Decision {
        let Some(policy) = policy else {
            return deny("no policy for this agent");
        };
        if policy.agent != intent.agent {
            return deny("policy belongs to another agent");
        }
        // Rule 2: without explicit allowlists, any asset or recipient is unknown.
        if !policy
            .rules
            .iter()
            .any(|r| matches!(r, Rule::AllowAssets { .. }))
        {
            return deny("policy has no asset allowlist");
        }
        if !policy
            .rules
            .iter()
            .any(|r| matches!(r, Rule::AllowRecipients { .. }))
        {
            return deny("policy has no recipient allowlist");
        }

        let mut approvals = Vec::new();
        for rule in &policy.rules {
            match check(rule, intent, spend, now) {
                Decision::Allow => {}
                Decision::RequireApproval { reasons } => approvals.extend(reasons),
                deny @ Decision::Deny { .. } => return deny,
            }
        }
        if approvals.is_empty() {
            Decision::Allow
        } else {
            Decision::RequireApproval { reasons: approvals }
        }
    }

    /// Evaluate and, on `Allow`, promote the intent to `Intent<Approved>`.
    pub fn decide(
        &self,
        policy: Option<&Policy>,
        intent: Intent<Proposed>,
        spend: &SpendSnapshot,
        now: Timestamp,
    ) -> Result<(Decision, Outcome), Error> {
        let decision = self.evaluate(policy, &intent, spend, now);
        let outcome = match &decision {
            Decision::Allow => Outcome::Allowed(intent.allow(&decision)?),
            Decision::RequireApproval { reasons } => {
                Outcome::NeedsApproval(intent, reasons.clone())
            }
            Decision::Deny { reasons } => Outcome::Denied(reasons.clone()),
        };
        Ok((decision, outcome))
    }
}

fn check(
    rule: &Rule,
    intent: &Intent<Proposed>,
    spend: &SpendSnapshot,
    now: Timestamp,
) -> Decision {
    let amount = &intent.amount;
    match rule {
        Rule::AllowAssets { assets } if !assets.contains(&amount.asset) => {
            deny(format!("asset {} is not allowed", amount.asset.code))
        }
        Rule::AllowRecipients { addresses } if !addresses.contains(&intent.to.address) => {
            deny(format!("{} is not a saved contact", intent.to.label))
        }
        Rule::MaxPerTx { amount: cap } if cap.asset == amount.asset && amount.minor > cap.minor => {
            deny(format!("{amount} exceeds the {cap} per-payment cap"))
        }
        Rule::MaxPerWindow { amount: cap, .. } if cap.asset == amount.asset => {
            match window_total(spend, amount) {
                Ok(total) if total.minor <= cap.minor => Decision::Allow,
                Ok(_) => deny(format!(
                    "{amount} would pass the {cap} limit for the window, counting payments already sent or pending"
                )),
                Err(_) => deny("spend snapshot does not match the intent's asset"),
            }
        }
        Rule::ApprovalAbove { amount: threshold }
            if threshold.asset == amount.asset && amount.minor > threshold.minor =>
        {
            require(format!(
                "{amount} is above the {threshold} approval threshold"
            ))
        }
        Rule::QuietHours { from, to, tz } => quiet_hours(now, *from, *to, tz),
        Rule::ApprovalAfterCount { count } if spend.payments >= *count => require(format!(
            "already {} payments in this window; past {count}, each one asks you",
            spend.payments
        )),
        Rule::ApproveFirstPayment if !spend.recipient_paid => require(format!(
            "first payment to {} ({})",
            intent.to.label,
            fingerprint(&intent.to.address)
        )),
        _ => Decision::Allow,
    }
}

fn quiet_hours(now: Timestamp, from: Time, to: Time, tz: &TimeZoneName) -> Decision {
    if in_window(now.to_zoned(tz.zone()).time(), from, to) {
        require("inside quiet hours")
    } else {
        Decision::Allow
    }
}

/// Rule 3: settled + reserved + this intent.
fn window_total(spend: &SpendSnapshot, amount: &Amount) -> Result<Amount, warden_domain::Error> {
    spend
        .settled
        .checked_add(&spend.reserved)?
        .checked_add(amount)
}

fn in_window(t: Time, from: Time, to: Time) -> bool {
    if from <= to {
        from <= t && t < to
    } else {
        t >= from || t < to
    }
}

/// The start and end of an address, enough to check it against the real one by eye.
pub fn fingerprint(address: &warden_domain::Address) -> String {
    let text = address.to_string();
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= 12 {
        return text;
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

fn deny(reason: impl Into<String>) -> Decision {
    Decision::Deny {
        reasons: vec![reason.into()],
    }
}

fn require(reason: impl Into<String>) -> Decision {
    Decision::RequireApproval {
        reasons: vec![reason.into()],
    }
}
