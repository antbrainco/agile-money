//! Solana Pay rail: SOL and SPL token (USDC) transfers keyed by a Solana Pay
//! `reference`.
//!
//! The reference is derived from the intent's idempotency key and added to the
//! transfer instruction as a read-only account, so `getSignaturesForAddress(reference)`
//! finds a payment that already landed and a retry never pays twice (rule 10).
//! Live network tests go behind the `live` feature.

use std::str::FromStr;

use async_trait::async_trait;
use jiff::Timestamp;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::api::client_error::Error as ClientError;
use solana_rpc_client::api::config::RpcTransactionConfig;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature, Signer};
use solana_sdk::transaction::{Transaction, VersionedTransaction};
use solana_transaction_status_client_types::option_serializer::OptionSerializer;
use solana_transaction_status_client_types::{
    UiLoadedAddresses, UiTransactionEncoding, UiTransactionStatusMeta, UiTransactionTokenBalance,
};
use url::Url;
use warden_domain::ports::{Rail, RailError};
use warden_domain::{
    Address, Approved, Asset, Intent, Invoice, PaymentRequest, RailId, RailRef, Settlement,
    SolAddress, SolSignature, Submission, TokenContract,
};

pub mod vault;

pub use vault::{Vault, VaultConfig};

pub const RAIL_ID: RailId = RailId::Sol;

pub const DEVNET_URL: &str = "https://api.devnet.solana.com";

/// Native SOL has 9 decimals: 1 SOL = 10^9 lamports.
pub fn sol() -> Asset {
    Asset {
        rail: RailId::Sol,
        code: "SOL"
            .parse()
            .unwrap_or_else(|_| unreachable!("SOL is a valid code")),
        decimals: 9,
        contract: None,
    }
}

/// Circle's USDC on devnet. Test USDC: https://faucet.circle.com
pub const USDC_DEVNET_MINT: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
/// Circle's USDC on mainnet.
pub const USDC_MAINNET_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// USDC, an SPL token with 6 decimals: 1 USDC = 10^6 base units.
pub fn usdc(mint: SolAddress) -> Asset {
    Asset {
        rail: RailId::Sol,
        code: "USDC"
            .parse()
            .unwrap_or_else(|_| unreachable!("USDC is a valid code")),
        decimals: 6,
        contract: Some(TokenContract::SplMint(mint)),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("rail misconfigured: {0}")]
    Config(String),
}

/// `[rail.sol]` plus `WARDEN_RAIL__SOL__KEYPAIR`.
#[derive(Debug, Clone, Deserialize)]
pub struct SolConfig {
    #[serde(default = "default_rpc_url")]
    pub rpc_url: Url,
    /// Base58 secret key, or the JSON byte array `solana-keygen` writes.
    /// Fine for devnet; with real funds, keep the key in a KMS or HSM, not env.
    pub keypair: SecretString,
    /// When set, SOL is paid from this on-chain vault, which holds the funds
    /// and enforces the owner's caps; the keypair above only asks it to pay.
    #[serde(default)]
    pub vault: Option<VaultConfig>,
}

fn default_rpc_url() -> Url {
    Url::parse(DEVNET_URL).unwrap_or_else(|_| unreachable!("DEVNET_URL parses"))
}

/// Which network an RPC endpoint serves, read from its genesis hash. The URL
/// proves nothing: any host can relay to mainnet, and a signed transaction is
/// valid wherever its blockhash came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cluster {
    Mainnet,
    Devnet,
    Testnet,
    /// A local validator or a private network.
    Other,
}

impl Cluster {
    fn from_genesis(hash: &str) -> Self {
        match hash {
            "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d" => Self::Mainnet,
            "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG" => Self::Devnet,
            "4uhcVJyU9pJkvQyS88uRDiswHXSCkY3zQawwpjk2NsNY" => Self::Testnet,
            _ => Self::Other,
        }
    }

    /// An explorer link for a transaction; `None` where the public explorer can't see it.
    pub fn explorer(self, signature: &SolSignature) -> Option<String> {
        let query = match self {
            Self::Mainnet => "",
            Self::Devnet => "?cluster=devnet",
            Self::Testnet => "?cluster=testnet",
            Self::Other => return None,
        };
        Some(format!("https://explorer.solana.com/tx/{signature}{query}"))
    }
}

pub struct SolRail {
    rpc: RpcClient,
    payer: Keypair,
    /// SPL tokens this rail may send, besides SOL. All on the classic Token program.
    tokens: Vec<Asset>,
    vault: Option<Vault>,
}

impl std::fmt::Debug for SolRail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SolRail")
            .field("rpc", &self.rpc.url())
            .field("payer", &self.payer.pubkey())
            .field("tokens", &self.tokens)
            .field("vault", &self.vault.map(|v| v.address))
            .finish()
    }
}

impl SolRail {
    pub fn new(config: &SolConfig) -> Result<Self, Error> {
        Ok(Self {
            rpc: RpcClient::new_with_commitment(
                config.rpc_url.to_string(),
                CommitmentConfig::confirmed(),
            ),
            payer: parse_keypair(config.keypair.expose_secret())?,
            tokens: Vec::new(),
            vault: config.vault.as_ref().map(Vault::from_config),
        })
    }

    pub fn vault(&self) -> Option<&Vault> {
        self.vault.as_ref()
    }

    /// The vault's state on chain, which must exist and name this rail's key as
    /// its agent. `None` when no vault is configured.
    pub async fn check_vault(&self) -> Result<Option<vault::State>, RailError> {
        let Some(vault) = &self.vault else {
            return Ok(None);
        };
        let account = self
            .rpc
            .get_account_with_commitment(&vault.address, CommitmentConfig::confirmed())
            .await
            .map_err(classify)?
            .value
            .ok_or_else(|| {
                RailError::Rejected(format!(
                    "vault {} is not open on this network",
                    vault.address
                ))
            })?;
        let state = (account.owner == vault.program)
            .then(|| vault::State::decode(&account.data))
            .flatten()
            .ok_or_else(|| {
                RailError::Rejected(format!(
                    "{} is not a vault of {}",
                    vault.address, vault.program
                ))
            })?;
        if state.limits.agent != self.payer.pubkey() {
            return Err(RailError::Rejected(format!(
                "vault {} answers to {}, not this wallet",
                vault.address, state.limits.agent
            )));
        }
        Ok(Some(state))
    }

    /// Lets the rail send these SPL tokens. Each needs an `SplMint` contract.
    pub fn with_tokens(mut self, tokens: Vec<Asset>) -> Result<Self, Error> {
        if let Some(bad) = tokens.iter().find(|t| mint_of(t).is_none()) {
            return Err(Error::Config(format!("{} is not an SPL token", bad.code)));
        }
        self.tokens = tokens;
        Ok(self)
    }

    /// SOL, then the configured tokens.
    pub fn assets(&self) -> Vec<Asset> {
        std::iter::once(sol()).chain(self.tokens.clone()).collect()
    }

    /// Each token's mint must exist on this network, belong to the Token program
    /// and have the decimals the asset claims. A devnet mint on mainnet fails here.
    pub async fn check_tokens(&self) -> Result<(), RailError> {
        for token in &self.tokens {
            let Some(mint) = mint_of(token) else { continue };
            let account = self
                .rpc
                .get_account_with_commitment(&mint, CommitmentConfig::confirmed())
                .await
                .map_err(classify)?
                .value
                .ok_or_else(|| {
                    RailError::Rejected(format!("{} mint {mint} not found here", token.code))
                })?;
            if account.owner != spl_token_interface::ID {
                return Err(RailError::Rejected(format!(
                    "{} mint {mint} is not a Token program mint",
                    token.code
                )));
            }
            let supply = self.rpc.get_token_supply(&mint).await.map_err(classify)?;
            if supply.decimals != token.decimals {
                return Err(RailError::Rejected(format!(
                    "{} mint {mint} has {} decimals, expected {}",
                    token.code, supply.decimals, token.decimals
                )));
            }
        }
        Ok(())
    }

    /// The wallet's balance of `asset` in minor units. No token account yet means 0.
    pub async fn balance_of(&self, asset: &Asset) -> Result<u128, RailError> {
        let Some(mint) = mint_of(asset) else {
            let holder = self.vault.map_or(self.payer.pubkey(), |v| v.address);
            return self
                .rpc
                .get_balance(&holder)
                .await
                .map(u128::from)
                .map_err(classify);
        };
        let account = token_account(&self.payer.pubkey(), &mint);
        let exists = self
            .rpc
            .get_account_with_commitment(&account, CommitmentConfig::confirmed())
            .await
            .map_err(classify)?
            .value
            .is_some();
        if !exists {
            return Ok(0);
        }
        let balance = self
            .rpc
            .get_token_account_balance(&account)
            .await
            .map_err(classify)?;
        balance
            .amount
            .parse()
            .map_err(|_| transport(format!("token balance {:?}", balance.amount)))
    }

    /// The paying wallet's address.
    pub fn address(&self) -> SolAddress {
        sol_address(&self.payer.pubkey())
    }

    /// The network the RPC endpoint is really on.
    pub async fn cluster(&self) -> Result<Cluster, RailError> {
        let genesis = self.rpc.get_genesis_hash().await.map_err(classify)?;
        Ok(Cluster::from_genesis(&genesis.to_string()))
    }

    /// Wallet balance in lamports.
    pub async fn balance(&self) -> Result<u64, RailError> {
        self.rpc
            .get_balance(&self.payer.pubkey())
            .await
            .map_err(classify)
    }

    /// Successful transactions carrying `reference` that paid this wallet `asset`,
    /// oldest first. Solana Pay wallets attach the request's reference to the
    /// transfer, so this is how an invoice payment is found.
    pub async fn received(
        &self,
        reference: &SolAddress,
        asset: &Asset,
    ) -> Result<Vec<Received>, RailError> {
        let reference = Pubkey::new_from_array(reference.to_bytes());
        let found = self
            .rpc
            .get_signatures_for_address(&reference)
            .await
            .map_err(classify)?;
        let wallet = self.payer.pubkey();
        let mint = mint_of(asset);
        let mut received = Vec::new();
        for status in found.into_iter().rev().filter(|s| s.err.is_none()) {
            let signature =
                Signature::from_str(&status.signature).map_err(|e| transport(e.to_string()))?;
            let tx = self
                .rpc
                .get_transaction_with_config(
                    &signature,
                    RpcTransactionConfig {
                        encoding: Some(UiTransactionEncoding::Base64),
                        commitment: Some(CommitmentConfig::confirmed()),
                        max_supported_transaction_version: Some(0),
                        min_context_slot: None,
                    },
                )
                .await
                .map_err(classify)?;
            let (Some(meta), Some(decoded)) =
                (tx.transaction.meta, tx.transaction.transaction.decode())
            else {
                continue;
            };
            if meta.err.is_some() {
                continue;
            }
            let minor = credited(
                &account_keys(&decoded, &meta),
                (&meta.pre_balances, &meta.post_balances),
                (
                    &token_balances(&meta.pre_token_balances),
                    &token_balances(&meta.post_token_balances),
                ),
                &wallet,
                mint.as_ref(),
            );
            if minor > 0 {
                received.push(Received {
                    signature: sol_signature(&signature),
                    minor,
                });
            }
        }
        Ok(received)
    }

    /// The first signature on `reference` that did not fail, if any.
    async fn landed(&self, reference: &Pubkey) -> Result<Option<Signature>, RailError> {
        let found = self
            .rpc
            .get_signatures_for_address(reference)
            .await
            .map_err(classify)?;
        found
            .into_iter()
            .rev()
            .find(|s| s.err.is_none())
            .map(|s| Signature::from_str(&s.signature).map_err(|e| transport(e.to_string())))
            .transpose()
    }
}

/// One transaction that paid into this wallet under a Solana Pay reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    pub signature: SolSignature,
    /// What it added to the wallet, in the asset's minor units.
    pub minor: u128,
}

/// A token balance line from a transaction's metadata, cut down to what crediting needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TokenBalance {
    mint: Pubkey,
    owner: Option<Pubkey>,
    amount: u128,
}

impl TokenBalance {
    fn from_ui(ui: &UiTransactionTokenBalance) -> Option<Self> {
        let owner: Option<&String> = ui.owner.as_ref().into();
        Some(Self {
            mint: Pubkey::from_str(&ui.mint).ok()?,
            owner: owner.and_then(|o| Pubkey::from_str(o).ok()),
            amount: ui.ui_token_amount.amount.parse().ok()?,
        })
    }
}

fn token_balances(list: &OptionSerializer<Vec<UiTransactionTokenBalance>>) -> Vec<TokenBalance> {
    let list: Option<&Vec<_>> = list.as_ref().into();
    list.map(|l| l.iter().filter_map(TokenBalance::from_ui).collect())
        .unwrap_or_default()
}

/// Static keys, then any loaded from lookup tables, in the order balances index them.
fn account_keys(tx: &VersionedTransaction, meta: &UiTransactionStatusMeta) -> Vec<Pubkey> {
    let mut keys = tx.message.static_account_keys().to_vec();
    let loaded: Option<&UiLoadedAddresses> = meta.loaded_addresses.as_ref().into();
    if let Some(loaded) = loaded {
        keys.extend(
            loaded
                .writable
                .iter()
                .chain(&loaded.readonly)
                .filter_map(|k| Pubkey::from_str(k).ok()),
        );
    }
    keys
}

/// How much one transaction added to `wallet`: lamports when `mint` is `None`,
/// else that mint's tokens across the wallet's token accounts. A net outflow is 0.
fn credited(
    keys: &[Pubkey],
    (pre, post): (&[u64], &[u64]),
    (pre_tokens, post_tokens): (&[TokenBalance], &[TokenBalance]),
    wallet: &Pubkey,
    mint: Option<&Pubkey>,
) -> u128 {
    match mint {
        None => keys
            .iter()
            .position(|k| k == wallet)
            .and_then(|i| Some(post.get(i)?.saturating_sub(*pre.get(i)?)))
            .map_or(0, u128::from),
        Some(mint) => {
            let held = |list: &[TokenBalance]| -> u128 {
                list.iter()
                    .filter(|b| &b.mint == mint && b.owner.as_ref() == Some(wallet))
                    .map(|b| b.amount)
                    .sum()
            };
            held(post_tokens).saturating_sub(held(pre_tokens))
        }
    }
}

/// Solana Pay reference for an idempotency key: any 32 bytes form a valid key.
pub fn reference_for(idempotency_key: &str) -> Pubkey {
    let digest = Sha256::digest(format!("tilly:sol:reference:{idempotency_key}"));
    Pubkey::new_from_array(digest.into())
}

fn parse_keypair(secret: &str) -> Result<Keypair, Error> {
    let secret = secret.trim();
    if secret.starts_with('[') {
        let bytes: Vec<u8> = serde_json::from_str(secret)
            .map_err(|e| Error::Config(format!("keypair json: {e}")))?;
        Keypair::try_from(bytes.as_slice()).map_err(|e| Error::Config(format!("keypair: {e}")))
    } else {
        Keypair::try_from_base58_string(secret)
            .map_err(|e| Error::Config(format!("keypair base58: {e}")))
    }
}

fn mint_of(asset: &Asset) -> Option<Pubkey> {
    match &asset.contract {
        Some(TokenContract::SplMint(mint)) => Some(Pubkey::new_from_array(mint.to_bytes())),
        _ => None,
    }
}

/// The associated token account holding `mint` for `owner`.
fn token_account(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    spl_associated_token_account_interface::address::get_associated_token_address_with_program_id(
        owner,
        mint,
        &spl_token_interface::ID,
    )
}

/// The instructions that move `amount` to `to`, with `reference` on the transfer.
/// For a token, the recipient's token account is created first if missing; the
/// payer covers its rent, and the create is a no-op when it exists.
fn transfer_instructions(
    payer: &Pubkey,
    to: &Pubkey,
    asset: &Asset,
    amount: u64,
    reference: Pubkey,
) -> Result<Vec<Instruction>, RailError> {
    let reference = AccountMeta::new_readonly(reference, false);
    let Some(mint) = mint_of(asset) else {
        let mut transfer = solana_system_interface::instruction::transfer(payer, to, amount);
        transfer.accounts.push(reference);
        return Ok(vec![transfer]);
    };
    let create =
        spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent(
            payer,
            to,
            &mint,
            &spl_token_interface::ID,
        );
    let mut transfer = spl_token_interface::instruction::transfer_checked(
        &spl_token_interface::ID,
        &token_account(payer, &mint),
        &mint,
        &token_account(to, &mint),
        payer,
        &[],
        amount,
        asset.decimals,
    )
    .map_err(|e| RailError::Rejected(format!("token transfer: {e}")))?;
    // Solana Pay puts references on the transfer. The Token program ignores
    // accounts past the authority unless the authority is a multisig.
    transfer.accounts.push(reference);
    Ok(vec![create, transfer])
}

fn recipient(intent: &Intent<Approved>) -> Result<Pubkey, RailError> {
    match &intent.to.address {
        Address::SolPubkey(key) => Ok(Pubkey::new_from_array(key.to_bytes())),
        _ => Err(RailError::Unsupported),
    }
}

fn sol_address(key: &Pubkey) -> SolAddress {
    SolAddress::from_bytes(key.to_bytes())
}

fn sol_signature(signature: &Signature) -> SolSignature {
    SolSignature::from_bytes(<[u8; 64]>::from(*signature))
}

fn classify(err: ClientError) -> RailError {
    match err.get_transaction_error() {
        Some(tx) => RailError::Rejected(tx.to_string()),
        None => transport(err.to_string()),
    }
}

/// Like `classify`, but a refusal from the vault (instruction 0) reads as its reason.
fn classify_vault(err: ClientError) -> RailError {
    use solana_sdk::instruction::InstructionError;
    use solana_sdk::transaction::TransactionError;
    match err.get_transaction_error() {
        Some(TransactionError::InstructionError(0, InstructionError::Custom(code))) => {
            match vault::refusal(code) {
                Some(reason) => RailError::Rejected(format!("the vault refused: {reason}")),
                None => RailError::Rejected(format!("the vault refused (code {code})")),
            }
        }
        _ => classify(err),
    }
}

fn transport(msg: String) -> RailError {
    RailError::Transport(msg)
}

#[async_trait]
impl Rail for SolRail {
    fn id(&self) -> RailId {
        RAIL_ID
    }

    /// SOL and the tokens given to `with_tokens`.
    fn supports(&self, asset: &Asset) -> bool {
        *asset == sol() || self.tokens.contains(asset)
    }

    /// A Solana Pay transfer request URL, paid to this wallet.
    async fn request(&self, inv: &Invoice) -> Result<PaymentRequest, RailError> {
        if !self.supports(&inv.total.asset) {
            return Err(RailError::Unsupported);
        }
        let reference = reference_for(&format!("invoice:{}", inv.id));
        let token = mint_of(&inv.total.asset)
            .map(|mint| format!("&spl-token={mint}"))
            .unwrap_or_default();
        let url = format!(
            "solana:{}?amount={}{token}&reference={reference}&label={}&message={}",
            self.payer.pubkey(),
            inv.total.major(),
            encode("Tilly"),
            encode(&message(inv)),
        );
        Ok(PaymentRequest {
            url,
            qr_png: None,
            rail_ref: RailRef::Sol {
                reference: sol_address(&reference),
                signature: None,
            },
        })
    }

    #[tracing::instrument(skip_all, fields(intent = %intent.id))]
    async fn submit(&self, intent: &Intent<Approved>) -> Result<Submission, RailError> {
        if !self.supports(&intent.amount.asset) {
            return Err(RailError::Unsupported);
        }
        let to = recipient(intent)?;
        let amount = u64::try_from(intent.amount.minor)
            .map_err(|_| RailError::Rejected("amount exceeds u64".into()))?;
        let reference = reference_for(intent.idempotency_key.as_str());
        let submission = |signature: Signature| Submission {
            intent: intent.id,
            rail_ref: RailRef::Sol {
                reference: sol_address(&reference),
                signature: Some(sol_signature(&signature)),
            },
            at: Timestamp::now(),
        };

        // A retry after a timeout: the first attempt may have landed.
        if let Some(signature) = self.landed(&reference).await? {
            tracing::info!(%signature, "already paid; not sending again");
            return Ok(submission(signature));
        }

        let via_vault = self
            .vault
            .filter(|_| mint_of(&intent.amount.asset).is_none());
        let mut instructions = match &via_vault {
            Some(vault) => vec![vault.pay(
                &self.payer.pubkey(),
                &to,
                amount,
                &[AccountMeta::new_readonly(reference, false)],
            )],
            None => transfer_instructions(
                &self.payer.pubkey(),
                &to,
                &intent.amount.asset,
                amount,
                reference,
            )?,
        };
        let memo_text = match &intent.memo {
            Some(memo) => format!("tilly {}: {memo}", intent.id),
            None => format!("tilly {}", intent.id),
        };
        let memo = spl_memo_interface::instruction::build_memo(
            &spl_memo_interface::v3::id(),
            memo_text.as_bytes(),
            &[],
        );
        let blockhash = self.rpc.get_latest_blockhash().await.map_err(classify)?;
        instructions.push(memo);
        let tx = Transaction::new_signed_with_payer(
            &instructions,
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        );
        let signature = self
            .rpc
            .send_and_confirm_transaction(&tx)
            .await
            .map_err(|e| match via_vault {
                Some(_) => classify_vault(e),
                None => classify(e),
            })?;
        tracing::info!(%signature, amount = %intent.amount, vault = via_vault.is_some(), "sent");
        Ok(submission(signature))
    }

    async fn status(&self, sub: &Submission) -> Result<Settlement, RailError> {
        let RailRef::Sol {
            reference,
            signature,
        } = &sub.rail_ref
        else {
            return Err(RailError::Unsupported);
        };
        let signature = match signature {
            Some(s) => Signature::from(s.to_bytes()),
            None => {
                let reference = Pubkey::new_from_array(reference.to_bytes());
                match self.landed(&reference).await? {
                    Some(s) => s,
                    None => return Ok(Settlement::Pending),
                }
            }
        };
        let statuses = self
            .rpc
            .get_signature_statuses(&[signature])
            .await
            .map_err(classify)?;
        Ok(match statuses.value.into_iter().next().flatten() {
            Some(s) if s.err.is_some() => Settlement::Failed {
                reason: s.err.map(|e| e.to_string()).unwrap_or_default(),
            },
            Some(s) if s.satisfies_commitment(CommitmentConfig::confirmed()) => {
                Settlement::Confirmed {
                    at: Timestamp::now(),
                }
            }
            _ => Settlement::Pending,
        })
    }
}

/// The Solana Pay `message` the payer's wallet shows: what the invoice is for.
/// It is public, readable on chain by anyone who looks up the transaction.
fn message(inv: &Invoice) -> String {
    match inv.lines.first() {
        Some(line) if !line.description.trim().is_empty() => {
            format!("{} - {}", inv.to.name, line.description.trim())
        }
        _ => format!("Invoice for {}", inv.to.name),
    }
}

/// Percent-encodes a Solana Pay query value.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clusters_come_from_genesis_hashes() {
        let devnet = Cluster::from_genesis("EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG");
        assert_eq!(devnet, Cluster::Devnet);
        assert_eq!(Cluster::from_genesis("local"), Cluster::Other);
        let sig = SolSignature::from_bytes([1; 64]);
        assert!(devnet.explorer(&sig).unwrap().ends_with("?cluster=devnet"));
        assert!(Cluster::Other.explorer(&sig).is_none());
    }

    #[test]
    fn keys_convert_both_ways() {
        let key = Pubkey::new_unique();
        assert_eq!(sol_address(&key).to_string(), key.to_string());
        let sig = Signature::from([7; 64]);
        assert_eq!(sol_signature(&sig).to_string(), sig.to_string());
    }

    #[test]
    fn reference_is_stable_per_key() {
        assert_eq!(reference_for("k"), reference_for("k"));
        assert_ne!(reference_for("k"), reference_for("j"));
    }

    #[test]
    fn keypair_parses_both_formats() {
        let kp = Keypair::new();
        let b58 = parse_keypair(&kp.to_base58_string()).unwrap();
        let json = serde_json::to_string(&kp.to_bytes().to_vec()).unwrap();
        let arr = parse_keypair(&json).unwrap();
        assert_eq!(b58.pubkey(), kp.pubkey());
        assert_eq!(arr.pubkey(), kp.pubkey());
    }

    fn devnet_usdc() -> Asset {
        usdc(USDC_DEVNET_MINT.parse().unwrap())
    }

    #[test]
    fn sol_transfer_carries_the_reference() {
        let (payer, to, reference) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let ixs = transfer_instructions(&payer, &to, &sol(), 5, reference).unwrap();
        assert_eq!(ixs.len(), 1);
        assert_eq!(ixs[0].program_id, solana_system_interface::program::ID);
        assert!(
            ixs[0]
                .accounts
                .iter()
                .any(|a| a.pubkey == reference && !a.is_writable)
        );
    }

    #[test]
    fn token_transfer_creates_the_account_then_moves_tokens() {
        let (payer, to, reference) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let asset = devnet_usdc();
        let mint = mint_of(&asset).unwrap();
        let ixs = transfer_instructions(&payer, &to, &asset, 2_500_000, reference).unwrap();
        assert_eq!(ixs.len(), 2);
        assert_eq!(
            ixs[0].program_id,
            spl_associated_token_account_interface::program::ID
        );
        let transfer = &ixs[1];
        assert_eq!(transfer.program_id, spl_token_interface::ID);
        let keys: Vec<Pubkey> = transfer.accounts.iter().map(|a| a.pubkey).collect();
        // transfer_checked: source, mint, destination, authority, then the reference.
        assert_eq!(
            keys,
            [
                token_account(&payer, &mint),
                mint,
                token_account(&to, &mint),
                payer,
                reference
            ]
        );
        assert!(transfer.accounts[3].is_signer);
        assert!(!transfer.accounts[4].is_writable && !transfer.accounts[4].is_signer);
    }

    #[test]
    fn only_spl_tokens_are_accepted_as_tokens() {
        let config = SolConfig {
            rpc_url: default_rpc_url(),
            keypair: SecretString::from(Keypair::new().to_base58_string()),
            vault: None,
        };
        let rail = SolRail::new(&config).unwrap();
        assert!(rail.with_tokens(vec![sol()]).is_err());
        let rail = SolRail::new(&config)
            .unwrap()
            .with_tokens(vec![devnet_usdc()])
            .unwrap();
        assert!(rail.supports(&devnet_usdc()));
        assert!(rail.supports(&sol()));
        assert_eq!(rail.assets().len(), 2);
    }

    fn balance(mint: Pubkey, owner: Pubkey, amount: u128) -> TokenBalance {
        TokenBalance {
            mint,
            owner: Some(owner),
            amount,
        }
    }

    #[test]
    fn lamports_credited_to_the_wallet() {
        let (payer, wallet, reference) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let keys = [payer, wallet, reference];
        let pre = [5_000_000, 1_000, 0];
        let post = [3_995_000, 1_001_000, 0];
        let credit = |who| credited(&keys, (&pre, &post), (&[], &[]), who, None);
        assert_eq!(credit(&wallet), 1_000_000);
        // The payer's own balance went down, which is not a credit.
        assert_eq!(credit(&payer), 0);
        assert_eq!(credit(&Pubkey::new_unique()), 0);
    }

    #[test]
    fn tokens_credited_only_for_the_wallet_and_mint() {
        let (payer, wallet, mint, other_mint) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        // The wallet's token account did not exist before, so it has no pre line.
        let pre = [balance(mint, payer, 9_000_000)];
        let post = [
            balance(mint, payer, 4_000_000),
            balance(mint, wallet, 5_000_000),
            balance(other_mint, wallet, 7),
        ];
        let credit = |who, m| credited(&[], (&[], &[]), (&pre, &post), who, Some(m));
        assert_eq!(credit(&wallet, &mint), 5_000_000);
        assert_eq!(credit(&wallet, &other_mint), 7);
        assert_eq!(credit(&payer, &mint), 0);
    }

    #[test]
    fn query_values_are_encoded() {
        assert_eq!(
            encode("Invoice for Acme & Co"),
            "Invoice%20for%20Acme%20%26%20Co"
        );
    }
}
