//! The owner's side of the on-chain vault: open it, change its limits, read it,
//! withdraw, and move SOL into it from the bot's wallet. The owner key signs
//! everything but `fund`; it lives in a keypair file the bot never reads.
//!
//! ```bash
//! set -a; . ./.env; set +a   # fund needs AGILE_MONEY_RAIL__SOL__KEYPAIR
//! export VAULT_PROGRAM=<program id> VAULT_OWNER=~/.config/solana/owner.json
//! cargo run -p agile-money-rail-sol --example vault -- show
//! cargo run -p agile-money-rail-sol --example vault -- open --agent <bot wallet> --per-tx 0.5 --per-day 1 --to <addr> ...
//! cargo run -p agile-money-rail-sol --example vault -- limits --agent <bot wallet> --per-tx 0.5 --per-day 1 --to <addr> ...
//! cargo run -p agile-money-rail-sol --example vault -- fund-owner 1   # fees and the deploy
//! cargo run -p agile-money-rail-sol --example vault -- fund 1
//! cargo run -p agile-money-rail-sol --example vault -- withdraw 0.5
//! ```
//!
//! Prints only public keys, amounts and signatures.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agile_money_rail_sol::vault::{Limits, State, Vault};
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer, read_keypair_file};
use solana_sdk::transaction::Transaction;

const LAMPORTS: u64 = 1_000_000_000;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}"))
}

fn expand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", env("HOME")),
        None => path.to_owned(),
    }
}

/// "1.5" SOL to lamports.
fn lamports(sol: &str) -> u64 {
    let (whole, frac) = sol.split_once('.').unwrap_or((sol, ""));
    assert!(frac.len() <= 9, "at most 9 decimals");
    let frac = format!("{frac:0<9}");
    whole.parse::<u64>().expect("amount") * LAMPORTS + frac.parse::<u64>().expect("amount")
}

fn sol(lamports: u64) -> String {
    format!("{}.{:09}", lamports / LAMPORTS, lamports % LAMPORTS)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn limits(args: &[String]) -> Limits {
    let (mut agent, mut per_tx, mut per_day, mut to) = (None, None, None, Vec::new());
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--agent" => agent = Some(value.parse::<Pubkey>().expect("agent pubkey")),
            "--per-tx" => per_tx = Some(lamports(value)),
            "--per-day" => per_day = Some(lamports(value)),
            "--to" => to.push(value.parse::<Pubkey>().expect("recipient pubkey")),
            other => panic!("unknown flag {other}"),
        }
    }
    Limits {
        agent: agent.expect("--agent"),
        max_per_tx: per_tx.expect("--per-tx"),
        max_per_day: per_day.expect("--per-day"),
        recipients: to,
    }
}

async fn send(rpc: &RpcClient, ix: Instruction, payer: &Keypair) {
    let blockhash = rpc.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[payer], blockhash);
    match rpc.send_and_confirm_transaction(&tx).await {
        Ok(signature) => println!("sent {signature}"),
        Err(e) => panic!("refused: {e}"),
    }
}

async fn show(rpc: &RpcClient, vault: &Vault) {
    println!("program {}", vault.program);
    println!("owner   {}", vault.owner);
    println!("vault   {}", vault.address);
    let Some(account) = rpc
        .get_account_with_commitment(&vault.address, CommitmentConfig::confirmed())
        .await
        .unwrap()
        .value
    else {
        println!("not open yet");
        return;
    };
    let rent = rpc
        .get_minimum_balance_for_rent_exemption(account.data.len())
        .await
        .unwrap();
    let state = State::decode(&account.data).expect("a vault account");
    println!(
        "balance {} SOL ({} spendable)",
        sol(account.lamports),
        sol(account.lamports.saturating_sub(rent))
    );
    println!("agent   {}", state.limits.agent);
    println!("per tx  {} SOL", sol(state.limits.max_per_tx));
    println!(
        "per day {} SOL ({} spent since {})",
        sol(state.limits.max_per_day),
        sol(state.spent),
        state.window_start
    );
    for to in &state.limits.recipients {
        println!("to      {to}");
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rpc = RpcClient::new_with_commitment(
        std::env::var("AGILE_MONEY_RAIL__SOL__RPC_URL")
            .unwrap_or_else(|_| agile_money_rail_sol::DEVNET_URL.into()),
        CommitmentConfig::confirmed(),
    );
    let program: Pubkey = env("VAULT_PROGRAM").parse().expect("VAULT_PROGRAM pubkey");
    let owner = read_keypair_file(expand(&env("VAULT_OWNER"))).expect("VAULT_OWNER keypair file");
    let vault = Vault::new(program, owner.pubkey());
    match args.first().map(String::as_str) {
        Some("show") => show(&rpc, &vault).await,
        Some("open") => send(&rpc, vault.init(&limits(&args[1..])), &owner).await,
        Some("limits") => send(&rpc, vault.configure(&limits(&args[1..])), &owner).await,
        Some("withdraw") => {
            let amount = lamports(args.get(1).expect("amount in SOL"));
            send(&rpc, vault.withdraw(amount), &owner).await
        }
        Some(cmd @ ("fund" | "fund-owner")) => {
            let amount = lamports(args.get(1).expect("amount in SOL"));
            let secret = env("AGILE_MONEY_RAIL__SOL__KEYPAIR");
            let secret = secret.trim();
            let wallet = if secret.starts_with('[') {
                let bytes: Vec<u8> = serde_json::from_str(secret).expect("keypair JSON");
                Keypair::try_from(bytes.as_slice()).expect("keypair bytes")
            } else {
                Keypair::from_base58_string(secret)
            };
            // fund fills the vault; fund-owner gives the owner key SOL for fees.
            let to = if cmd == "fund" {
                vault.address
            } else {
                vault.owner
            };
            println!("from    {}", wallet.pubkey());
            println!("to      {to}");
            let ix = solana_system_interface::instruction::transfer(&wallet.pubkey(), &to, amount);
            send(&rpc, ix, &wallet).await
        }
        _ => panic!(
            "usage: vault show | open ... | limits ... | fund <SOL> | fund-owner <SOL> | withdraw <SOL>"
        ),
    }
}
