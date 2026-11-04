//! The owner's second factor: a TOTP code from an authenticator app. Telegram
//! is the first factor, so anything that loosens what Tilly may pay (a new or
//! changed contact, higher limits, resuming after a pause) also needs a code.
//! Tightening (pausing, removing, lowering) never does.

use std::sync::Mutex;

use jiff::{SignedDuration, Timestamp};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use totp_rs::{Builder, Secret, Totp};

/// Wrong codes allowed within `LOCKOUT` before codes are refused for a while.
const MAX_FAILURES: usize = 5;
const LOCKOUT: SignedDuration = SignedDuration::from_mins(15);

/// `[security]`.
#[derive(Debug, Clone, Deserialize)]
pub struct SecurityConfig {
    /// Base32, from `TOTP_SECRET` only; `--example totp_setup` makes one.
    /// Without it nothing can be loosened from chat or the dashboard.
    pub totp_secret: Option<SecretString>,
    /// How long a confirmed contact waits before it can be paid, so a
    /// hijacked session can't add a payee and pay it at once.
    #[serde(default = "cooling_off")]
    pub cooling_off: SignedDuration,
    /// Payments a day that may go through without asking; past this, each
    /// one needs approval, so a manipulated agent can't drain contacts slowly.
    #[serde(default = "payments_per_day")]
    pub payments_per_day: u32,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            totp_secret: None,
            cooling_off: cooling_off(),
            payments_per_day: payments_per_day(),
        }
    }
}

fn cooling_off() -> SignedDuration {
    SignedDuration::from_hours(24)
}

fn payments_per_day() -> u32 {
    5
}

/// Builds the TOTP the authenticator app shares. SHA-1, 6 digits, 30 seconds:
/// what every app supports.
pub fn totp(secret: Secret) -> anyhow::Result<Totp> {
    Builder::new()
        .with_secret(secret)
        .with_issuer(Some("Tilly"))
        .with_account_name("owner")
        .build()
        .map_err(|e| anyhow::anyhow!("TOTP secret: {e:?}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeError {
    NotSetUp,
    Locked {
        until: Timestamp,
    },
    Wrong,
    /// Each code works once (RFC 6238 §5.2).
    Reused,
}

impl std::fmt::Display for CodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSetUp => f.write_str(
                "Authenticator codes aren't set up, so this can't be done from here. \
                 Run `cargo run -p warden-telegram-bot --example totp_setup` on the server.",
            ),
            Self::Locked { until } => write!(
                f,
                "Too many wrong codes. Try again after {} UTC.",
                until.strftime("%H:%M")
            ),
            Self::Wrong => f.write_str("That code is wrong or has expired."),
            Self::Reused => f.write_str("That code was already used. Wait for the next one."),
        }
    }
}

#[derive(Debug, Default)]
struct Attempts {
    last_step: Option<u64>,
    failures: Vec<Timestamp>,
}

/// Checks codes, refuses replays, and locks out after repeated failures.
pub struct Guard {
    totp: Option<Totp>,
    attempts: Mutex<Attempts>,
}

impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Guard")
            .field("enabled", &self.enabled())
            .finish_non_exhaustive()
    }
}

impl Guard {
    pub fn new(secret: Option<&SecretString>) -> anyhow::Result<Self> {
        let totp = secret
            .map(|s| {
                let secret = Secret::try_from_base32(s.expose_secret().trim())
                    .map_err(|e| anyhow::anyhow!("TOTP_SECRET is not base32: {e:?}"))?;
                totp(secret)
            })
            .transpose()?;
        Ok(Self {
            totp,
            attempts: Mutex::default(),
        })
    }

    pub fn enabled(&self) -> bool {
        self.totp.is_some()
    }

    pub fn check(&self, code: &str, now: Timestamp) -> Result<(), CodeError> {
        let totp = self.totp.as_ref().ok_or(CodeError::NotSetUp)?;
        let mut attempts = self.attempts.lock().unwrap_or_else(|e| e.into_inner());
        attempts.failures.retain(|t| *t + LOCKOUT > now);
        if attempts.failures.len() >= MAX_FAILURES {
            let until = attempts.failures[0] + LOCKOUT;
            tracing::warn!(%until, "code refused: locked out");
            return Err(CodeError::Locked { until });
        }
        let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        let time = u64::try_from(now.as_second()).unwrap_or_default();
        let step = (code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()))
            .then(|| totp.check(&code, time))
            .flatten();
        match step {
            Some(step) if attempts.last_step.is_some_and(|last| step <= last) => {
                tracing::warn!("code refused: already used");
                Err(CodeError::Reused)
            }
            Some(step) => {
                attempts.last_step = Some(step);
                attempts.failures.clear();
                Ok(())
            }
            None => {
                attempts.failures.push(now);
                tracing::warn!(failures = attempts.failures.len(), "code refused: wrong");
                Err(CodeError::Wrong)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> (Guard, Totp) {
        let secret = SecretString::from("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP");
        let guard = Guard::new(Some(&secret)).unwrap();
        let totp =
            totp(Secret::try_from_base32("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP").unwrap()).unwrap();
        (guard, totp)
    }

    fn code(totp: &Totp, now: Timestamp) -> String {
        totp.generate(now.as_second() as u64).to_string()
    }

    #[test]
    fn a_right_code_passes_once() {
        let (guard, totp) = guard();
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let code = code(&totp, now);
        assert_eq!(guard.check(&code, now), Ok(()));
        assert_eq!(guard.check(&code, now), Err(CodeError::Reused));
    }

    #[test]
    fn wrong_codes_lock_out_even_the_right_one() {
        let (guard, totp) = guard();
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let right = code(&totp, now);
        let wrong = if right == "000000" {
            "111111"
        } else {
            "000000"
        };
        for _ in 0..MAX_FAILURES {
            assert_eq!(guard.check(wrong, now), Err(CodeError::Wrong));
        }
        assert!(matches!(
            guard.check(&right, now),
            Err(CodeError::Locked { .. })
        ));
        let later = now + LOCKOUT + SignedDuration::from_secs(60);
        assert_eq!(guard.check(&code(&totp, later), later), Ok(()));
    }

    #[test]
    fn without_a_secret_nothing_passes() {
        let guard = Guard::new(None).unwrap();
        assert_eq!(
            guard.check("123456", Timestamp::now()),
            Err(CodeError::NotSetUp)
        );
    }

    #[test]
    fn the_default_cooling_off_parses_from_toml_text() {
        let config: SecurityConfig = serde_json::from_str(r#"{"cooling_off": "2m"}"#).unwrap();
        assert_eq!(config.cooling_off, SignedDuration::from_mins(2));
    }
}
