//! Money as integer minor units of an explicit asset (rule 4). Never `f64`.

use serde::{Deserialize, Serialize};

use crate::{AssetCode, Error, TokenContract};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RailId {
    Sol,
    Paypal,
    Near,
    Bch,
    Concordium,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Asset {
    pub rail: RailId,
    /// "USDC", "SOL", "USD", "NEAR" or "BCH".
    pub code: AssetCode,
    /// USDC = 6, SOL = 9, USD = 2.
    pub decimals: u8,
    /// `None` for the rail's native coin.
    pub contract: Option<TokenContract>,
}

impl std::fmt::Display for Asset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.contract {
            None => self.code.fmt(f),
            Some(contract) => write!(f, "{} ({contract})", self.code),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amount {
    /// 400 USDC = 400_000_000. A decimal string on the wire: JSON numbers lose
    /// precision past 2^53 and `serde_json::Value` cannot hold a `u128`.
    #[serde(with = "minor_units")]
    pub minor: u128,
    pub asset: Asset,
}

impl Amount {
    pub fn new(minor: u128, asset: Asset) -> Self {
        Self { minor, asset }
    }

    pub fn zero(asset: Asset) -> Self {
        Self { minor: 0, asset }
    }

    pub fn checked_add(&self, other: &Amount) -> Result<Amount, Error> {
        self.same_asset(other)?;
        let minor = self.minor.checked_add(other.minor).ok_or(Error::Overflow)?;
        Ok(Amount::new(minor, self.asset.clone()))
    }

    pub fn same_asset(&self, other: &Amount) -> Result<(), Error> {
        if self.asset == other.asset {
            Ok(())
        } else {
            Err(Error::AssetMismatch {
                expected: Box::new(self.asset.clone()),
                actual: Box::new(other.asset.clone()),
            })
        }
    }
}

impl Amount {
    /// The amount in major units without the asset code, e.g. `1.5` for 1_500_000_000 lamports.
    pub fn major(&self) -> String {
        let scale = 10u128.pow(u32::from(self.asset.decimals));
        let (whole, frac) = (self.minor / scale, self.minor % scale);
        if frac == 0 {
            return whole.to_string();
        }
        let frac = format!("{frac:0width$}", width = usize::from(self.asset.decimals));
        format!("{whole}.{}", frac.trim_end_matches('0'))
    }
}

impl std::fmt::Display for Amount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.major(), self.asset.code)
    }
}

mod minor_units {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(minor: &u128, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(minor)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u128, D::Error> {
        String::deserialize(d)?.parse().map_err(D::Error::custom)
    }
}
