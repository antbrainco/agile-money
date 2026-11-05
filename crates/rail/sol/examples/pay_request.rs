//! Pays a Solana Pay transfer request on devnet the way a wallet would, from a
//! throwaway payer that the bot's wallet funds first. Tests the payment watcher
//! without a phone wallet.
//!
//! ```bash
//! set -a; . ./.env; set +a
//! cargo run -p warden-rail-sol --example pay_request -- 'solana:...'
//! ```
//!
//! Prints only public keys and signatures.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::str::FromStr;

use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};
use solana_sdk::transaction::Transaction;
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;
use spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent;
use url::Url;

const FEE_FUND: u64 = 10_000_000; // 0.01 SOL for fees and the payer's token account rent

fn keypair(secret: &str) -> Keypair {
    let secret = secret.trim();
    if secret.starts_with('[') {
        let bytes: Vec<u8> = serde_json::from_str(secret).expect("keypair JSON");
        Keypair::try_from(bytes.as_slice()).expect("keypair bytes")
    } else {
        Keypair::from_base58_string(secret)
    }
}

/// "2" or "1.5" in major units to base units.
fn minor(amount: &str, decimals: u32) -> u64 {
    let (whole, frac) = amount.split_once('.').unwrap_or((amount, ""));
    assert!(frac.len() <= decimals as usize, "too many decimals");
    let frac = format!("{frac:0<width$}", width = decimals as usize);
    whole.parse::<u64>().unwrap() * 10u64.pow(decimals) + frac.parse::<u64>().unwrap_or(0)
}

async fn send(rpc: &RpcClient, ixs: &[Instruction], signers: &[&Keypair]) -> String {
    let blockhash = rpc.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(ixs, Some(&signers[0].pubkey()), signers, blockhash);
    rpc.send_and_confirm_transaction(&tx)
        .await
        .unwrap()
        .to_string()
}

#[tokio::main]
async fn main() {
    let link = std::env::args().nth(1).expect("pass the solana: link");
    // `solana:<recipient>?...` has no authority; parse the query off a fake base.
    let rest = link.strip_prefix("solana:").expect("a solana: link");
    let (recipient, query) = rest.split_once('?').expect("a query");
    let recipient = Pubkey::from_str(recipient).unwrap();
    let query = Url::parse(&format!("http://x/?{query}")).unwrap();
    let param = |k: &str| {
        query
            .query_pairs()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.into_owned())
    };
    let amount = param("amount").expect("amount");
    let mint = Pubkey::from_str(&param("spl-token").expect("an spl-token request")).unwrap();
    let reference = Pubkey::from_str(&param("reference").expect("reference")).unwrap();

    let rpc = RpcClient::new_with_commitment(
        std::env::var("SOLANA_RPC_URL").unwrap_or_else(|_| "https://api.devnet.solana.com".into()),
        CommitmentConfig::confirmed(),
    );
    let genesis = rpc.get_genesis_hash().await.unwrap().to_string();
    assert_eq!(
        genesis, "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG",
        "not devnet"
    );
    let decimals = u32::from(rpc.get_token_supply(&mint).await.unwrap().decimals);
    let units = minor(&amount, decimals);

    let funder = keypair(&std::env::var("WARDEN_RAIL__SOL__KEYPAIR").expect("bot keypair"));
    let payer = Keypair::new();
    let token = spl_token_interface::ID;
    let ata = |owner: &Pubkey| get_associated_token_address_with_program_id(owner, &mint, &token);
    println!("payer {}", payer.pubkey());

    // 1. The bot's wallet funds the throwaway payer: SOL for fees, then the tokens.
    let fund = send(
        &rpc,
        &[
            solana_system_interface::instruction::transfer(
                &funder.pubkey(),
                &payer.pubkey(),
                FEE_FUND,
            ),
            create_associated_token_account_idempotent(
                &funder.pubkey(),
                &payer.pubkey(),
                &mint,
                &token,
            ),
            spl_token_interface::instruction::transfer_checked(
                &token,
                &ata(&funder.pubkey()),
                &mint,
                &ata(&payer.pubkey()),
                &funder.pubkey(),
                &[],
                units,
                decimals as u8,
            )
            .unwrap(),
        ],
        &[&funder],
    )
    .await;
    println!("funded {fund}");

    // 2. The payer pays the request, as a Solana Pay wallet does: transfer_checked
    //    to the recipient's token account with the reference as a read-only key.
    let mut transfer = spl_token_interface::instruction::transfer_checked(
        &token,
        &ata(&payer.pubkey()),
        &mint,
        &ata(&recipient),
        &payer.pubkey(),
        &[],
        units,
        decimals as u8,
    )
    .unwrap();
    transfer
        .accounts
        .push(AccountMeta::new_readonly(reference, false));
    let paid = send(
        &rpc,
        &[
            create_associated_token_account_idempotent(&payer.pubkey(), &recipient, &mint, &token),
            transfer,
        ],
        &[&payer],
    )
    .await;
    println!("paid {paid}");
    println!("https://explorer.solana.com/tx/{paid}?cluster=devnet");
}
