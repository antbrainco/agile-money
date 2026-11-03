//! Money as integer minor units of an explicit asset (rule 4). Never `f64`.

use serde::{Deserialize, Serialize};

use crate::Error;

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
    /// "USDC", "SOL", "USD", "NEAR", "BCH" or a token id.
    pub code: String,
    /// USDC = 6, SOL = 9, USD = 2.
    pub decimals: u8,
    /// SPL mint, NEP-141 account or CashToken category.
    pub contract: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amount {
    /// 400 USDC = 400_000_000.
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
                expected: self.asset.code.clone(),
                actual: other.asset.code.clone(),
            })
        }
    }
}
