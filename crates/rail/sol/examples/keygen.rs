//! Makes a throwaway devnet wallet: `cargo run -p warden-rail-sol --example keygen`.
//! Paste the secret into `.env` as `WARDEN_RAIL__SOL__KEYPAIR`, then fund the address
//! at https://faucet.solana.com. Never use a key printed to a terminal for real money.

use solana_sdk::signature::{Keypair, Signer};

fn main() {
    let keypair = Keypair::new();
    println!("address: {}", keypair.pubkey());
    println!("WARDEN_RAIL__SOL__KEYPAIR={}", keypair.to_base58_string());
}
