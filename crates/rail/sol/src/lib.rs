//! Solana Pay rail: native SOL transfers keyed by a Solana Pay `reference`.
//!
//! The reference is derived from the intent's idempotency key and added to the
//! transfer as a read-only account, so `getSignaturesForAddress(reference)`
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
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::instruction::AccountMeta;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature, Signer};
use solana_sdk::transaction::Transaction;
use url::Url;
use warden_domain::ports::{Rail, RailError};
use warden_domain::{
    Address, Approved, Asset, Intent, Invoice, PaymentRequest, RailId, RailRef, Settlement,
    SolAddress, SolSignature, Submission,
};

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
}

impl std::fmt::Debug for SolRail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SolRail")
            .field("rpc", &self.rpc.url())
            .field("payer", &self.payer.pubkey())
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
        })
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

fn transport(msg: String) -> RailError {
    RailError::Transport(msg)
}

#[async_trait]
impl Rail for SolRail {
    fn id(&self) -> RailId {
        RAIL_ID
    }

    /// Native SOL only for now; SPL tokens carry a mint in `contract`.
    fn supports(&self, asset: &Asset) -> bool {
        *asset == sol()
    }

    /// A Solana Pay transfer request URL, paid to this wallet.
    async fn request(&self, inv: &Invoice) -> Result<PaymentRequest, RailError> {
        if !self.supports(&inv.total.asset) {
            return Err(RailError::Unsupported);
        }
        let reference = reference_for(&format!("invoice:{}", inv.id));
        let url = format!(
            "solana:{}?amount={}&reference={reference}&label={}&message={}",
            self.payer.pubkey(),
            inv.total.major(),
            encode("Tilly"),
            encode(&format!("Invoice for {}", inv.to.name)),
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
        let lamports = u64::try_from(intent.amount.minor)
            .map_err(|_| RailError::Rejected("amount exceeds u64 lamports".into()))?;
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

        let mut transfer =
            solana_system_interface::instruction::transfer(&self.payer.pubkey(), &to, lamports);
        transfer
            .accounts
            .push(AccountMeta::new_readonly(reference, false));
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
        let tx = Transaction::new_signed_with_payer(
            &[transfer, memo],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        );
        let signature = self
            .rpc
            .send_and_confirm_transaction(&tx)
            .await
            .map_err(classify)?;
        tracing::info!(%signature, lamports, "sent");
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

    #[test]
    fn query_values_are_encoded() {
        assert_eq!(
            encode("Invoice for Acme & Co"),
            "Invoice%20for%20Acme%20%26%20Co"
        );
    }
}
