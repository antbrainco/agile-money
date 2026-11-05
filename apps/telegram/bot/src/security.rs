//! The owner's second factor: a TOTP code from an authenticator app. Telegram
//! is the first factor, so anything that loosens what Agile Money may pay (a new or
//! changed contact, higher limits, resuming after a pause) also needs a code.
//! Tightening (pausing, removing, lowering) never does.

use std::sync::{Mutex, RwLock};

use jiff::{SignedDuration, Timestamp};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use totp_rs::{Builder, Secret, Totp};

/// Wrong codes allowed within `LOCKOUT` before codes are refused for a while.
const MAX_FAILURES: usize = 5;
const LOCKOUT: SignedDuration = SignedDuration::from_mins(15);
/// How long a dashboard enrolment waits for its first code.
const ENROL_WINDOW: SignedDuration = SignedDuration::from_mins(10);
/// Wrong first codes before an enrolment is thrown away.
const ENROL_TRIES: u8 = 5;

/// `[security]`.
#[derive(Debug, Clone, Deserialize)]
pub struct SecurityConfig {
    /// Base32, from `TOTP_SECRET`; `--example totp_setup` makes one. Without
    /// it the owner can enrol from the dashboard, which stores the secret in
    /// Postgres. Until then nothing can be loosened with a code.
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
        .with_issuer(Some("Agile Money"))
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
                 Set them up under Security in the dashboard.",
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

/// Why a dashboard enrolment didn't go through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrolError {
    AlreadySetUp,
    /// Nothing started, it timed out, or too many wrong codes ended it.
    NotStarted,
    Wrong,
}

impl std::fmt::Display for EnrolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::AlreadySetUp => {
                "Codes are already set up. To move to a new device, remove the old secret on the server first."
            }
            Self::NotStarted => "That setup expired. Start again to get a new QR code.",
            Self::Wrong => {
                "That code doesn't match. Check the clock on your phone and try the next one."
            }
        })
    }
}

/// What the authenticator app needs: the otpauth URL (the QR code) and the
/// same key for typing in by hand.
pub struct Enrolment {
    pub url: String,
    pub key: SecretString,
}

/// A confirmed enrolment, to store before `install` turns codes on.
pub struct Enrolled {
    pub key: SecretString,
    totp: Totp,
    step: u64,
}

struct Pending {
    totp: Totp,
    expires: Timestamp,
    tries: u8,
}

/// Checks codes, refuses replays, and locks out after repeated failures.
pub struct Guard {
    totp: RwLock<Option<Totp>>,
    attempts: Mutex<Attempts>,
    pending: Mutex<Option<Pending>>,
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
            totp: RwLock::new(totp),
            attempts: Mutex::default(),
            pending: Mutex::default(),
        })
    }

    pub fn enabled(&self) -> bool {
        self.totp
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Starts enrolling a new authenticator, replacing any unfinished start.
    /// Only while codes are off: an enrolled device is never replaced here.
    pub fn begin(&self, now: Timestamp) -> Result<Enrolment, EnrolError> {
        if self.enabled() {
            return Err(EnrolError::AlreadySetUp);
        }
        let secret = Secret::generate();
        let key = SecretString::from(secret.to_base32());
        let totp = totp(secret).map_err(|_| EnrolError::NotStarted)?;
        let url = totp.to_url().map_err(|_| EnrolError::NotStarted)?;
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(Pending {
            totp,
            expires: now + ENROL_WINDOW,
            tries: 0,
        });
        Ok(Enrolment { url, key })
    }

    /// Checks the first code from the new authenticator. On a match the
    /// enrolment is handed back (and forgotten here) for the caller to store.
    pub fn finish(&self, code: &str, now: Timestamp) -> Result<Enrolled, EnrolError> {
        if self.enabled() {
            return Err(EnrolError::AlreadySetUp);
        }
        let mut slot = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let pending = slot
            .as_mut()
            .filter(|p| p.expires > now)
            .ok_or(EnrolError::NotStarted)?;
        let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        let time = u64::try_from(now.as_second()).unwrap_or_default();
        let step = (code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()))
            .then(|| pending.totp.check(&code, time))
            .flatten();
        let Some(step) = step else {
            pending.tries += 1;
            if pending.tries >= ENROL_TRIES {
                *slot = None;
                tracing::warn!("enrolment dropped after wrong codes");
                return Err(EnrolError::NotStarted);
            }
            return Err(EnrolError::Wrong);
        };
        let Some(Pending { totp, .. }) = slot.take() else {
            return Err(EnrolError::NotStarted);
        };
        Ok(Enrolled {
            key: SecretString::from(totp.secret().to_base32()),
            totp,
            step,
        })
    }

    /// Turns codes on with a stored enrolment. Its first code can't be used again.
    pub fn install(&self, enrolled: Enrolled) {
        let mut totp = self.totp.write().unwrap_or_else(|e| e.into_inner());
        if totp.is_none() {
            *totp = Some(enrolled.totp);
            let mut attempts = self.attempts.lock().unwrap_or_else(|e| e.into_inner());
            attempts.last_step = Some(enrolled.step);
            attempts.failures.clear();
        }
    }

    pub fn check(&self, code: &str, now: Timestamp) -> Result<(), CodeError> {
        let totp = self.totp.read().unwrap_or_else(|e| e.into_inner());
        let totp = totp.as_ref().ok_or(CodeError::NotSetUp)?;
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

    /// The app's side of an enrolment: the TOTP built from the shown key.
    fn phone(enrolment: &Enrolment) -> Totp {
        totp(Secret::try_from_base32(enrolment.key.expose_secret()).unwrap()).unwrap()
    }

    #[test]
    fn enrolling_turns_codes_on_and_burns_the_first_code() {
        let guard = Guard::new(None).unwrap();
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        let enrolment = guard.begin(now).unwrap();
        assert!(
            enrolment
                .url
                .starts_with("otpauth://totp/Agile%20Money:owner?")
        );
        let app = phone(&enrolment);
        let first = code(&app, now);
        let enrolled = guard.finish(&first, now).unwrap();
        assert_eq!(enrolled.key.expose_secret(), enrolment.key.expose_secret());
        assert!(!guard.enabled(), "nothing changes until it is stored");
        guard.install(enrolled);
        assert!(guard.enabled());
        assert_eq!(guard.check(&first, now), Err(CodeError::Reused));
        let later = now + SignedDuration::from_secs(30);
        assert_eq!(guard.check(&code(&app, later), later), Ok(()));
        assert!(matches!(guard.begin(later), Err(EnrolError::AlreadySetUp)));
    }

    #[test]
    fn an_enrolled_secret_is_never_replaced() {
        let (guard, _) = guard();
        assert!(matches!(
            guard.begin(Timestamp::now()),
            Err(EnrolError::AlreadySetUp)
        ));
    }

    #[test]
    fn enrolments_expire_and_end_after_wrong_codes() {
        let guard = Guard::new(None).unwrap();
        let now = Timestamp::from_second(1_800_000_000).unwrap();
        assert!(matches!(
            guard.finish("123456", now),
            Err(EnrolError::NotStarted)
        ));

        let app = phone(&guard.begin(now).unwrap());
        let late = now + ENROL_WINDOW + SignedDuration::from_secs(1);
        assert!(matches!(
            guard.finish(&code(&app, late), late),
            Err(EnrolError::NotStarted)
        ));

        let app = phone(&guard.begin(now).unwrap());
        let right = code(&app, now);
        let wrong = if right == "000000" {
            "111111"
        } else {
            "000000"
        };
        for _ in 1..ENROL_TRIES {
            assert!(matches!(guard.finish(wrong, now), Err(EnrolError::Wrong)));
        }
        assert!(matches!(
            guard.finish(wrong, now),
            Err(EnrolError::NotStarted)
        ));
        assert!(matches!(
            guard.finish(&right, now),
            Err(EnrolError::NotStarted)
        ));
        assert!(!guard.enabled());
    }

    #[test]
    fn the_default_cooling_off_parses_from_toml_text() {
        let config: SecurityConfig = serde_json::from_str(r#"{"cooling_off": "2m"}"#).unwrap();
        assert_eq!(config.cooling_off, SignedDuration::from_mins(2));
    }
}
