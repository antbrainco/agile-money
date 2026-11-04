#![allow(clippy::unwrap_used)] // helpers outside #[test] fns, which clippy.toml does not cover
use jiff::{SignedDuration, Timestamp};
use proptest::prelude::*;
use uuid::Uuid;
use warden_domain::*;
use warden_policy::{Outcome, PolicyEngine};

fn usdc() -> Asset {
    Asset {
        rail: RailId::Sol,
        code: "USDC".parse().unwrap(),
        decimals: 6,
        contract: None,
    }
}

fn acme() -> Address {
    Address::SolPubkey(SolAddress::from_bytes([1; 32]))
}

fn agent() -> AgentId {
    AgentId::new(Uuid::from_u128(2))
}

fn intent(minor: u128, to: Address) -> Intent<Proposed> {
    Intent::new(
        IntentId::new(Uuid::from_u128(1)),
        agent(),
        Amount::new(minor, usdc()),
        Recipient {
            label: "Acme Ltd".into(),
            address: to,
        },
        None,
        "key-1".parse().unwrap(),
        Provenance {
            run: RunId::new(Uuid::from_u128(3)),
            tool_call: "call-1".parse().unwrap(),
        },
        Timestamp::UNIX_EPOCH,
    )
}

fn policy() -> Policy {
    Policy {
        id: PolicyId::new(Uuid::from_u128(9)),
        version: 1,
        agent: agent(),
        rules: vec![
            Rule::AllowAssets {
                assets: vec![usdc()],
            },
            Rule::AllowRecipients {
                addresses: vec![acme()],
            },
            Rule::MaxPerTx {
                amount: Amount::new(1_000_000_000, usdc()),
            },
            Rule::MaxPerWindow {
                amount: Amount::new(2_000_000_000, usdc()),
                window: SignedDuration::from_hours(24),
            },
            Rule::ApprovalAbove {
                amount: Amount::new(100_000_000, usdc()),
            },
        ],
    }
}

fn spend(settled: u128, reserved: u128) -> SpendSnapshot {
    SpendSnapshot {
        settled: Amount::new(settled, usdc()),
        reserved: Amount::new(reserved, usdc()),
        window_start: Timestamp::UNIX_EPOCH,
        recipient_paid: true,
        payments: 0,
    }
}

fn denied(d: &Decision) -> bool {
    matches!(d, Decision::Deny { .. })
}

fn eval(p: Option<&Policy>, i: &Intent<Proposed>, s: &SpendSnapshot) -> Decision {
    PolicyEngine::new().evaluate(p, i, s, Timestamp::UNIX_EPOCH)
}

#[test]
fn missing_policy_denies() {
    assert!(matches!(
        eval(None, &intent(1, acme()), &spend(0, 0)),
        Decision::Deny { .. }
    ));
}

#[test]
fn small_payment_is_allowed_and_signable() {
    let (decision, outcome) = PolicyEngine::new()
        .decide(
            Some(&policy()),
            intent(50_000_000, acme()),
            &spend(0, 0),
            Timestamp::UNIX_EPOCH,
        )
        .unwrap();
    assert_eq!(decision, Decision::Allow);
    assert!(matches!(outcome, Outcome::Allowed(_)));
}

#[test]
fn above_threshold_needs_approval() {
    let d = eval(Some(&policy()), &intent(400_000_000, acme()), &spend(0, 0));
    assert!(matches!(d, Decision::RequireApproval { .. }));
}

#[test]
fn reserved_spend_counts_toward_window() {
    let d = eval(
        Some(&policy()),
        &intent(50_000_000, acme()),
        &spend(1_000_000_000, 960_000_000),
    );
    assert!(matches!(d, Decision::Deny { .. }));
}

#[test]
fn quiet_hours_need_approval() {
    let mut p = policy();
    p.rules.push(Rule::QuietHours {
        from: jiff::civil::time(22, 0, 0, 0),
        to: jiff::civil::time(7, 0, 0, 0),
        tz: "UTC".parse().unwrap(),
    });
    let d = eval(Some(&p), &intent(1, acme()), &spend(0, 0));
    assert!(matches!(d, Decision::RequireApproval { .. }));
}

#[test]
fn first_payment_to_an_address_needs_approval_however_small() {
    let mut p = policy();
    p.rules.push(Rule::ApproveFirstPayment);
    let first = SpendSnapshot {
        recipient_paid: false,
        ..spend(0, 0)
    };
    match eval(Some(&p), &intent(1, acme()), &first) {
        Decision::RequireApproval { reasons } => {
            assert!(reasons[0].starts_with("first payment to Acme Ltd"));
        }
        other => panic!("expected approval, got {other:?}"),
    }
    assert_eq!(
        eval(Some(&p), &intent(1, acme()), &spend(0, 0)),
        Decision::Allow
    );
}

#[test]
fn past_the_count_every_payment_asks_however_small() {
    let mut p = policy();
    p.rules.push(Rule::ApprovalAfterCount { count: 5 });
    let busy = |payments| SpendSnapshot {
        payments,
        ..spend(0, 0)
    };
    assert_eq!(
        eval(Some(&p), &intent(1, acme()), &busy(4)),
        Decision::Allow
    );
    match eval(Some(&p), &intent(1, acme()), &busy(5)) {
        Decision::RequireApproval { reasons } => {
            assert!(reasons[0].starts_with("already 5 payments"))
        }
        other => panic!("expected approval, got {other:?}"),
    }
}

proptest! {
    #[test]
    fn nothing_passes_above_per_tx_cap(minor in 1_000_000_001u128..u128::MAX) {
        prop_assert!(denied(&eval(Some(&policy()), &intent(minor, acme()), &spend(0, 0))));
    }

    #[test]
    fn unknown_recipient_never_passes(addr in proptest::array::uniform32(any::<u8>()), minor in 0u128..1_000) {
        prop_assume!(addr != [1; 32]);
        let to = Address::SolPubkey(SolAddress::from_bytes(addr));
        let d = eval(Some(&policy()), &intent(minor, to), &spend(0, 0));
        prop_assert!(denied(&d));
    }

    #[test]
    fn window_cap_holds(settled in 0u128..3_000_000_000, reserved in 0u128..3_000_000_000, minor in 0u128..1_000_000_000) {
        let d = eval(Some(&policy()), &intent(minor, acme()), &spend(settled, reserved));
        if settled + reserved + minor > 2_000_000_000 {
            prop_assert!(denied(&d));
        }
    }
}
