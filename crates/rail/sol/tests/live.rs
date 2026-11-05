//! Real devnet transfers. Run with a funded devnet keypair:
//! `AGILE_MONEY_RAIL__SOL__KEYPAIR=... cargo test -p agile-money-rail-sol --features live --test live -- --nocapture`
//! Without the keypair it skips, so `--all-features` in CI stays offline.

#![cfg(feature = "live")]
#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers outside #[test] fns

use agile_money_domain::ports::Rail;
use agile_money_domain::{
    Address, AgentId, Amount, Decision, IdempotencyKey, Intent, IntentId, Provenance, RailRef,
    Recipient, RunId, Settlement,
};
use agile_money_rail_sol::{Cluster, DEVNET_URL, SolConfig, SolRail};
use jiff::Timestamp;
use secrecy::SecretString;
use uuid::Uuid;

fn rail() -> Option<SolRail> {
    let Ok(keypair) = std::env::var("AGILE_MONEY_RAIL__SOL__KEYPAIR") else {
        eprintln!("skipped: set AGILE_MONEY_RAIL__SOL__KEYPAIR to a funded devnet keypair");
        return None;
    };
    let rail = SolRail::new(&SolConfig {
        rpc_url: std::env::var("AGILE_MONEY_RAIL__SOL__RPC_URL")
            .as_deref()
            .unwrap_or(DEVNET_URL)
            .parse()
            .expect("valid RPC URL"),
        keypair: SecretString::from(keypair),
        vault: None,
    })
    .expect("valid keypair");
    Some(rail)
}

#[tokio::test]
async fn pays_once_and_confirms_on_devnet() {
    let Some(rail) = rail() else { return };
    assert_eq!(
        rail.cluster().await.unwrap(),
        Cluster::Devnet,
        "devnet only"
    );
    let fee_budget = 10_000;
    let amount = 1_000; // lamports; paid to ourselves, so only the fee is spent
    assert!(
        rail.balance().await.unwrap() >= amount + fee_budget,
        "fund {} at https://faucet.solana.com",
        rail.address()
    );

    let id = IntentId::new(Uuid::now_v7());
    let intent = Intent::new(
        id,
        AgentId::new(Uuid::now_v7()),
        Amount::new(u128::from(amount), agile_money_rail_sol::sol()),
        Recipient {
            label: "self".into(),
            address: Address::SolPubkey(rail.address()),
        },
        Some("live test".into()),
        IdempotencyKey::for_intent(id),
        Provenance {
            run: RunId::new(Uuid::now_v7()),
            tool_call: "live".parse().unwrap(),
        },
        Timestamp::now(),
    )
    .allow(&Decision::Allow)
    .unwrap();

    let first = rail.submit(&intent).await.unwrap();
    let RailRef::Sol {
        signature: Some(sig),
        ..
    } = &first.rail_ref
    else {
        panic!("no signature in {:?}", first.rail_ref);
    };
    println!("{}", Cluster::Devnet.explorer(sig).unwrap());

    // A retry with the same intent finds the reference instead of paying twice.
    let again = rail.submit(&intent).await.unwrap();
    assert_eq!(again.rail_ref, first.rail_ref);

    assert!(matches!(
        rail.status(&first).await.unwrap(),
        Settlement::Confirmed { .. }
    ));
}
