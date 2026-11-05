//! Two ways the dashboard learns who the owner is, besides the password:
//! a Solana wallet that signs a one-time message, and the owner chat opening a
//! one-time link. Both are held only in memory and work once.

use std::str::FromStr;
use std::sync::Mutex;

use jiff::{SignedDuration, Timestamp};
use solana_signature::Signature;
use totp_rs::Secret;

/// How long a challenge or link stays good.
const WINDOW: SignedDuration = SignedDuration::from_mins(5);
/// Open challenges at once; the Worker rate-limits who asks.
const MAX_OPEN: usize = 32;

/// What a signed message is for: the two are never interchangeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Purpose {
    SignIn,
    Link,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProfileError {
    BadAddress,
    /// No open challenge for this wallet, or it expired.
    NoChallenge,
    BadSignature,
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BadAddress => "That isn't a Solana wallet address.",
            Self::NoChallenge => "That sign-in request expired. Try again.",
            Self::BadSignature => "The wallet's signature didn't match. Try again.",
        })
    }
}

struct Challenge {
    purpose: Purpose,
    address: String,
    message: String,
    expires: Timestamp,
}

#[derive(Default)]
pub struct Linking {
    challenges: Mutex<Vec<Challenge>>,
    /// The token in the owner's t.me link, and when it expires.
    telegram: Mutex<Option<(String, Timestamp)>>,
}

impl Linking {
    /// The message for `address` to sign, naming the dashboard's `host` so a
    /// signature asked for elsewhere doesn't pass here. Replaces any open one
    /// for the same wallet and purpose.
    pub fn challenge(
        &self,
        purpose: Purpose,
        address: &str,
        host: &str,
        now: Timestamp,
    ) -> Result<String, ProfileError> {
        pubkey(address)?;
        let what = match purpose {
            Purpose::SignIn => "Sign in to your Agile Money dashboard.",
            Purpose::Link => "Link this wallet to your Agile Money dashboard, so it can sign in.",
        };
        let message = format!(
            "{host} wants you to sign in with your Solana account:\n{address}\n\n{what} \
             This sends no transaction and costs nothing.\n\nNonce: {}\nIssued At: {now}",
            nonce()
        );
        let mut open = self.challenges.lock().unwrap_or_else(|e| e.into_inner());
        open.retain(|c| c.expires > now && !(c.purpose == purpose && c.address == address));
        if open.len() >= MAX_OPEN {
            open.remove(0);
        }
        open.push(Challenge {
            purpose,
            address: address.to_owned(),
            message: message.clone(),
            expires: now + WINDOW,
        });
        Ok(message)
    }

    /// Checks `signature` (base58) over the open challenge for `address`. The
    /// challenge is used up either way, so each costs a fresh signature.
    pub fn verify(
        &self,
        purpose: Purpose,
        address: &str,
        signature: &str,
        now: Timestamp,
    ) -> Result<(), ProfileError> {
        let key = pubkey(address)?;
        let challenge = {
            let mut open = self.challenges.lock().unwrap_or_else(|e| e.into_inner());
            let at = open
                .iter()
                .position(|c| c.purpose == purpose && c.address == address)
                .ok_or(ProfileError::NoChallenge)?;
            open.remove(at)
        };
        if challenge.expires <= now {
            return Err(ProfileError::NoChallenge);
        }
        let signature = Signature::from_str(signature).map_err(|_| ProfileError::BadSignature)?;
        if signature.verify(&key, challenge.message.as_bytes()) {
            Ok(())
        } else {
            Err(ProfileError::BadSignature)
        }
    }

    /// A fresh token for the owner's `t.me/<bot>?start=<token>` link.
    pub fn telegram_token(&self, now: Timestamp) -> String {
        let token = nonce();
        *self.telegram.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((token.clone(), now + WINDOW));
        token
    }

    /// True once for the current token, if it hasn't expired.
    pub fn redeem(&self, token: &str, now: Timestamp) -> bool {
        let mut slot = self.telegram.lock().unwrap_or_else(|e| e.into_inner());
        match slot.as_ref() {
            Some((want, expires))
                if *expires > now && crate::dashboard::same(want.as_bytes(), token.as_bytes()) =>
            {
                *slot = None;
                true
            }
            _ => false,
        }
    }
}

/// 32 base32 characters from 20 random bytes; fits a Telegram start parameter.
fn nonce() -> String {
    Secret::generate().to_base32()
}

fn pubkey(address: &str) -> Result<[u8; 32], ProfileError> {
    bs58::decode(address)
        .into_vec()
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(ProfileError::BadAddress)
}

/// "7xKX…gAsU", for messages.
pub fn short(address: &str) -> String {
    match (
        address.get(..4),
        address.get(address.len().saturating_sub(4)..),
    ) {
        (Some(head), Some(tail)) if address.len() > 12 => format!("{head}…{tail}"),
        _ => address.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use solana_sdk::signature::{Keypair, Signer};

    use super::*;

    fn now() -> Timestamp {
        Timestamp::from_second(1_800_000_000).unwrap()
    }

    #[test]
    fn a_signed_challenge_verifies_once() {
        let links = Linking::default();
        let wallet = Keypair::new();
        let address = wallet.pubkey().to_string();
        let message = links
            .challenge(Purpose::SignIn, &address, "agile-money.example", now())
            .unwrap();
        assert!(message.starts_with("agile-money.example wants you to sign in"));
        let signature = wallet.sign_message(message.as_bytes()).to_string();
        assert_eq!(
            links.verify(Purpose::SignIn, &address, &signature, now()),
            Ok(())
        );
        assert_eq!(
            links.verify(Purpose::SignIn, &address, &signature, now()),
            Err(ProfileError::NoChallenge)
        );
    }

    #[test]
    fn wrong_signer_purpose_or_time_fails() {
        let links = Linking::default();
        let (wallet, other) = (Keypair::new(), Keypair::new());
        let address = wallet.pubkey().to_string();

        let message = links
            .challenge(Purpose::Link, &address, "h", now())
            .unwrap();
        let forged = other.sign_message(message.as_bytes()).to_string();
        assert_eq!(
            links.verify(Purpose::Link, &address, &forged, now()),
            Err(ProfileError::BadSignature)
        );

        let message = links
            .challenge(Purpose::Link, &address, "h", now())
            .unwrap();
        let signed = wallet.sign_message(message.as_bytes()).to_string();
        assert_eq!(
            links.verify(Purpose::SignIn, &address, &signed, now()),
            Err(ProfileError::NoChallenge)
        );
        assert_eq!(
            links.verify(Purpose::Link, &address, &signed, now() + WINDOW),
            Err(ProfileError::NoChallenge)
        );
        assert_eq!(
            links.challenge(Purpose::Link, "not-a-key", "h", now()),
            Err(ProfileError::BadAddress)
        );
    }

    #[test]
    fn telegram_tokens_redeem_once_before_expiry() {
        let links = Linking::default();
        let token = links.telegram_token(now());
        assert!(token.len() <= 64 && token.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(!links.redeem("guess", now()));
        assert!(links.redeem(&token, now()));
        assert!(!links.redeem(&token, now()));
        let token = links.telegram_token(now());
        assert!(!links.redeem(&token, now() + WINDOW));
    }

    #[test]
    fn short_keeps_both_ends() {
        assert_eq!(
            short("7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU"),
            "7xKX…gAsU"
        );
        assert_eq!(short("abc"), "abc");
    }
}
