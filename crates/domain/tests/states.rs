use jiff::Timestamp;
use proptest::prelude::*;
use uuid::Uuid;
use warden_domain::*;

fn usdc(minor: u128) -> Amount {
    Amount::new(
        minor,
        Asset {
            rail: RailId::Sol,
            code: "USDC".into(),
            decimals: 6,
            contract: None,
        },
    )
}

fn intent(minor: u128, memo: Option<&str>) -> Intent<Proposed> {
    Intent::new(
        IntentId::new(Uuid::from_u128(1)),
        AgentId::new(Uuid::from_u128(2)),
        usdc(minor),
        Recipient {
            label: "Acme Ltd".into(),
            address: Address::SolPubkey("Acme111".into()),
        },
        memo.map(String::from),
        "key-1".into(),
        Provenance {
            run: RunId::new(Uuid::from_u128(3)),
            tool_call: "call-1".into(),
        },
        Timestamp::UNIX_EPOCH,
    )
}

fn rail_ref() -> RailRef {
    RailRef::Sol {
        reference: "ref".into(),
        signature: None,
    }
}

#[test]
fn intent_happy_path_with_retry() {
    let id = IntentId::new(Uuid::from_u128(1));
    let events = [
        Event::IntentDecided {
            intent: id,
            decision: Decision::Allow,
            policy: PolicyId::new(Uuid::from_u128(9)),
            version: 1,
        },
        Event::PaymentSubmitted {
            intent: id,
            rail_ref: rail_ref(),
        },
        Event::PaymentFailed {
            intent: id,
            reason: "timeout".into(),
            retryable: true,
        },
        Event::PaymentSubmitted {
            intent: id,
            rail_ref: rail_ref(),
        },
        Event::PaymentConfirmed {
            intent: id,
            rail_ref: rail_ref(),
        },
    ];
    assert_eq!(
        IntentState::replay(&events).unwrap(),
        IntentState::Confirmed
    );
}

#[test]
fn intent_cannot_submit_before_decision() {
    let id = IntentId::new(Uuid::from_u128(1));
    let err = IntentState::Proposed.transition(&Event::PaymentSubmitted {
        intent: id,
        rail_ref: rail_ref(),
    });
    assert!(err.is_err());
}

#[test]
fn approval_grants_matching_intent() {
    let now = Timestamp::UNIX_EPOCH;
    let i = intent(400_000_000, None);
    let mut approval = Approval::new(
        ApprovalId::new(Uuid::from_u128(5)),
        &i,
        Channel::Telegram,
        now,
    )
    .unwrap();
    let approved = approval.grant(i, now).unwrap();
    assert_eq!(approval.state, ApprovalState::Granted);
    assert_eq!(approved.amount.minor, 400_000_000);
}

#[test]
fn approval_rejects_changed_intent() {
    let now = Timestamp::UNIX_EPOCH;
    let mut approval = Approval::new(
        ApprovalId::new(Uuid::from_u128(5)),
        &intent(100, None),
        Channel::Dashboard,
        now,
    )
    .unwrap();
    assert!(approval.grant(intent(100, Some("changed")), now).is_err());
    assert_eq!(approval.state, ApprovalState::Superseded);
}

#[test]
fn approval_expires_after_fifteen_minutes() {
    let now = Timestamp::UNIX_EPOCH;
    let i = intent(100, None);
    let mut approval = Approval::new(
        ApprovalId::new(Uuid::from_u128(5)),
        &i,
        Channel::Telegram,
        now,
    )
    .unwrap();
    let later = now
        .checked_add(jiff::SignedDuration::from_mins(15))
        .unwrap();
    assert!(approval.grant(i, later).is_err());
    assert_eq!(approval.state, ApprovalState::Expired);
}

#[test]
fn event_serializes_with_snake_case_tag() {
    let e = Event::PaymentFailed {
        intent: IntentId::new(Uuid::from_u128(1)),
        reason: "x".into(),
        retryable: false,
    };
    let json = serde_json::to_value(&e).unwrap();
    assert_eq!(json["type"], e.kind());
}

proptest! {
    #[test]
    fn hash_changes_with_amount(a in 0u128..u128::MAX, b in 0u128..u128::MAX) {
        prop_assume!(a != b);
        prop_assert_ne!(intent(a, None).hash().unwrap(), intent(b, None).hash().unwrap());
    }
}
