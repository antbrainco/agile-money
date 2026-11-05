//! Validated values that travel as strings on the wire. Each one checks itself
//! when built or deserialized, so the JSON shape (and every stored intent hash)
//! is the same as a bare string's, but a malformed value never gets past serde.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{IntentId, RunId};

/// A string that failed a value type's checks. The value itself is left out:
/// it may come from the model or a counterparty.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind}: {reason}")]
pub struct InvalidValue {
    pub kind: &'static str,
    pub reason: &'static str,
}

impl InvalidValue {
    fn new(kind: &'static str, reason: &'static str) -> Self {
        Self { kind, reason }
    }
}

/// Plumbing shared by every value type: parse, display, and serde as a string.
macro_rules! string_codec {
    ($name:ident) => {
        impl TryFrom<String> for $name {
            type Error = InvalidValue;
            fn try_from(value: String) -> Result<Self, InvalidValue> {
                value.parse()
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.to_string()
            }
        }
    };
}

/// A checked string, stored as given.
macro_rules! text {
    ($(#[$meta:meta])* $name:ident, $check:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = InvalidValue;
            fn from_str(value: &str) -> Result<Self, InvalidValue> {
                let check: fn(&str) -> Result<(), &'static str> = $check;
                check(value).map_err(|why| InvalidValue::new(stringify!($name), why))?;
                Ok(Self(value.to_owned()))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        string_codec!($name);
    };
}

/// Fixed-length bytes, written as base58 or hex.
macro_rules! bytes {
    ($(#[$meta:meta])* $name:ident, $len:literal, $codec:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name([u8; $len]);

        impl $name {
            pub const fn from_bytes(bytes: [u8; $len]) -> Self {
                Self(bytes)
            }

            pub const fn to_bytes(self) -> [u8; $len] {
                self.0
            }
        }

        impl FromStr for $name {
            type Err = InvalidValue;
            fn from_str(value: &str) -> Result<Self, InvalidValue> {
                $codec::decode(value)
                    .map(Self)
                    .map_err(|why| InvalidValue::new(stringify!($name), why))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&$codec::encode(&self.0))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self)
            }
        }

        string_codec!($name);
    };
}

mod base58 {
    pub fn decode<const N: usize>(value: &str) -> Result<[u8; N], &'static str> {
        let mut out = [0u8; N];
        match bs58::decode(value).onto(&mut out[..]) {
            Ok(n) if n == N => Ok(out),
            Ok(_) | Err(bs58::decode::Error::BufferTooSmall) => Err("wrong length"),
            Err(_) => Err("not base58"),
        }
    }

    pub fn encode(bytes: &[u8]) -> String {
        bs58::encode(bytes).into_string()
    }
}

mod hex32 {
    pub fn decode<const N: usize>(value: &str) -> Result<[u8; N], &'static str> {
        let mut out = [0u8; N];
        hex::decode_to_slice(value, &mut out).map_err(|e| match e {
            hex::FromHexError::InvalidStringLength | hex::FromHexError::OddLength => "wrong length",
            hex::FromHexError::InvalidHexCharacter { .. } => "not hex",
        })?;
        Ok(out)
    }

    pub fn encode(bytes: &[u8]) -> String {
        hex::encode(bytes)
    }
}

fn bounded(value: &str, max: usize) -> Result<(), &'static str> {
    match value.len() {
        0 => Err("empty"),
        n if n > max => Err("too long"),
        _ => Ok(()),
    }
}

// --- Solana -----------------------------------------------------------------

bytes!(
    /// An ed25519 public key: a wallet, mint or Solana Pay reference.
    SolAddress,
    32,
    base58
);
bytes!(
    /// A transaction signature, which is also its id on Solana.
    SolSignature,
    64,
    base58
);

// --- NEAR -------------------------------------------------------------------

text!(
    /// A named (`alice.near`) or implicit (64 hex chars) NEAR account.
    NearAccount,
    |v| {
        if !(2..=64).contains(&v.len()) {
            return Err("must be 2 to 64 characters");
        }
        let separator = |c: char| matches!(c, '-' | '_' | '.');
        if !v
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || separator(c))
        {
            return Err("only lowercase letters, digits, - _ and .");
        }
        let bytes = v.as_bytes();
        let sep = |b: u8| separator(char::from(b));
        if sep(bytes[0]) || sep(bytes[v.len() - 1]) || bytes.windows(2).any(|w| sep(w[0]) && sep(w[1])) {
            return Err("separators must sit between letters or digits");
        }
        Ok(())
    }
);
bytes!(
    /// A NEAR transaction hash.
    NearTxHash,
    32,
    base58
);

// --- Bitcoin Cash -----------------------------------------------------------

/// A CashAddr, always stored with its network prefix, checksum verified.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CashAddr(String);

impl CashAddr {
    const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    const PREFIXES: [&str; 3] = ["bitcoincash", "bchtest", "bchreg"];

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The BCH code from the CashAddr spec; a valid address sums to zero.
    fn polymod(values: impl Iterator<Item = u8>) -> u64 {
        const GEN: [u64; 5] = [
            0x98_f2bc_8e61,
            0x79_b76d_99e2,
            0xf3_3e5f_b3c4,
            0xae_2eab_e2a8,
            0x1e_4f43_e470,
        ];
        let mut c: u64 = 1;
        for d in values {
            let top = c >> 35;
            c = ((c & 0x07_ffff_ffff) << 5) ^ u64::from(d);
            for (i, g) in GEN.iter().enumerate() {
                if top >> i & 1 == 1 {
                    c ^= g;
                }
            }
        }
        c ^ 1
    }
}

impl FromStr for CashAddr {
    type Err = InvalidValue;
    fn from_str(value: &str) -> Result<Self, InvalidValue> {
        let bad = |why| InvalidValue::new("CashAddr", why);
        if value != value.to_lowercase() && value != value.to_uppercase() {
            return Err(bad("mixed case"));
        }
        let value = value.to_lowercase();
        let (prefix, payload) = value
            .split_once(':')
            .unwrap_or(("bitcoincash", value.as_str()));
        if !Self::PREFIXES.contains(&prefix) {
            return Err(bad("unknown network prefix"));
        }
        if payload.len() < 42 {
            return Err(bad("too short"));
        }
        let digits = payload
            .bytes()
            .map(|b| {
                Self::CHARSET
                    .iter()
                    .position(|&c| c == b)
                    .and_then(|i| u8::try_from(i).ok())
            })
            .collect::<Option<Vec<u8>>>()
            .ok_or_else(|| bad("not CashAddr base32"))?;
        let checked = prefix
            .bytes()
            .map(|b| b & 0x1f)
            .chain([0])
            .chain(digits.iter().copied());
        if Self::polymod(checked) != 0 {
            return Err(bad("checksum mismatch"));
        }
        Ok(Self(format!("{prefix}:{payload}")))
    }
}

impl fmt::Display for CashAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

string_codec!(CashAddr);

bytes!(
    /// A Bitcoin Cash transaction id, hex as block explorers show it.
    BchTxid,
    32,
    hex32
);
bytes!(
    /// A CashToken category id.
    CashTokenCategory,
    32,
    hex32
);

// --- Concordium -------------------------------------------------------------

/// A Concordium account: base58check of version byte 1 and a 32-byte address.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ConcordiumAccount([u8; 32]);

impl ConcordiumAccount {
    const VERSION: u8 = 1;

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    fn checksum(body: &[u8]) -> [u8; 4] {
        let twice = Sha256::digest(Sha256::digest(body));
        [twice[0], twice[1], twice[2], twice[3]]
    }
}

impl FromStr for ConcordiumAccount {
    type Err = InvalidValue;
    fn from_str(value: &str) -> Result<Self, InvalidValue> {
        let bad = |why| InvalidValue::new("ConcordiumAccount", why);
        let raw: [u8; 37] = base58::decode(value).map_err(bad)?;
        let (body, sum) = raw.split_at(33);
        if body[0] != Self::VERSION {
            return Err(bad("not an account address"));
        }
        if sum != Self::checksum(body) {
            return Err(bad("checksum mismatch"));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&body[1..]);
        Ok(Self(out))
    }
}

impl fmt::Display for ConcordiumAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut raw = Vec::with_capacity(37);
        raw.push(Self::VERSION);
        raw.extend_from_slice(&self.0);
        let sum = Self::checksum(&raw);
        raw.extend_from_slice(&sum);
        f.write_str(&base58::encode(&raw))
    }
}

impl fmt::Debug for ConcordiumAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConcordiumAccount({self})")
    }
}

string_codec!(ConcordiumAccount);

bytes!(
    /// A Concordium transaction hash.
    ConcordiumTxHash,
    32,
    hex32
);

// --- PayPal and email ---------------------------------------------------------

text!(
    /// A PayPal order id, e.g. `5O190127TN364715T`.
    PaypalOrderId,
    |v| {
        bounded(v, 36)?;
        if v.chars().all(|c| c.is_ascii_alphanumeric()) {
            Ok(())
        } else {
            Err("only letters and digits")
        }
    }
);

text!(
    /// An email address. Only the shape is checked; delivery proves the rest.
    EmailAddress,
    |v| {
        bounded(v, 254)?;
        let (local, domain) = v.rsplit_once('@').ok_or("missing @")?;
        if local.is_empty() || v.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("malformed");
        }
        if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
            return Err("malformed domain");
        }
        Ok(())
    }
);

// --- Agile Money's own -------------------------------------------------------------

text!(
    /// Dedupes a payment end to end (rule 10). Printable ASCII, no spaces.
    IdempotencyKey,
    |v| {
        bounded(v, 200)?;
        if v.bytes().all(|b| b.is_ascii_graphic()) {
            Ok(())
        } else {
            Err("only printable ASCII without spaces")
        }
    }
);

impl IdempotencyKey {
    /// One payment per tool call: a retried run reuses the key, a new call does not.
    pub fn for_tool_call(run: RunId, call: &ToolCallId) -> Result<Self, InvalidValue> {
        format!("{run}:{call}").parse()
    }

    /// For a one-off intent that no tool call proposed, e.g. a test.
    pub fn for_intent(intent: IntentId) -> Self {
        Self(format!("intent:{intent}"))
    }
}

text!(
    /// The provider's id for one tool call, echoed back with its result.
    ToolCallId,
    |v| {
        bounded(v, 256)?;
        if v.chars().any(char::is_control) {
            Err("control characters")
        } else {
            Ok(())
        }
    }
);

text!(
    /// A ticker such as `SOL` or `USDC`.
    AssetCode,
    |v| {
        bounded(v, 16)?;
        if v.chars().all(|c| c.is_ascii_alphanumeric()) {
            Ok(())
        } else {
            Err("only letters and digits")
        }
    }
);

text!(
    /// An IANA time zone known to this machine, e.g. `Europe/London`.
    TimeZoneName,
    |v| {
        jiff::tz::TimeZone::get(v)
            .map(|_| ())
            .map_err(|_| "unknown time zone")
    }
);

impl TimeZoneName {
    pub fn zone(&self) -> jiff::tz::TimeZone {
        // Checked on construction; a zone database can't lose a zone mid-run.
        jiff::tz::TimeZone::get(&self.0).unwrap_or(jiff::tz::TimeZone::UTC)
    }
}

/// Which token an asset is, when it isn't the rail's native coin.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum TokenContract {
    SplMint(SolAddress),
    /// A NEP-141 fungible token, which is itself an account.
    Nep141(NearAccount),
    CashToken(CashTokenCategory),
}

impl fmt::Display for TokenContract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SplMint(mint) => mint.fmt(f),
            Self::Nep141(account) => account.fmt(f),
            Self::CashToken(category) => category.fmt(f),
        }
    }
}
