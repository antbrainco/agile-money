//! The tools an agent gets. Each one reads or proposes; none can sign.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use warden_domain::ports::IdGen;
use warden_domain::{
    Address, Amount, Asset, IdempotencyKey, Intent, IntentId, IntentState, Provenance, RailRef,
    Recipient, SolAddress,
};
use warden_policy::Payments;

use crate::{Error, Tool, ToolCtx, ToolOutput};

/// Parses a decimal amount in major units ("0.25") into minor units.
pub fn parse_major(text: &str, decimals: u8) -> Result<u128, Error> {
    let invalid = |why: &str| Error::Invalid(format!("amount {text:?}: {why}"));
    let value = Decimal::from_str(text.trim()).map_err(|_| invalid("not a decimal number"))?;
    if value <= Decimal::ZERO {
        return Err(invalid("must be positive"));
    }
    let scale = Decimal::from(10u64.pow(u32::from(decimals)));
    let minor = value
        .checked_mul(scale)
        .ok_or_else(|| invalid("too large"))?;
    if !minor.fract().is_zero() {
        return Err(invalid(&format!("at most {decimals} decimal places")));
    }
    minor.to_u128().ok_or_else(|| invalid("too large"))
}

/// Proposes a payment to a known contact or an exact address.
pub struct ProposePayment {
    contacts: Vec<Recipient>,
    assets: Vec<Asset>,
    ids: Arc<dyn IdGen>,
}

impl std::fmt::Debug for ProposePayment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProposePayment")
            .field("contacts", &self.contacts)
            .field("assets", &self.assets)
            .finish_non_exhaustive()
    }
}

impl ProposePayment {
    pub fn new(contacts: Vec<Recipient>, assets: Vec<Asset>, ids: Arc<dyn IdGen>) -> Self {
        Self {
            contacts,
            assets,
            ids,
        }
    }

    fn recipient(&self, to: &str) -> Result<Recipient, Error> {
        let to = to.trim();
        if let Some(contact) = self
            .contacts
            .iter()
            .find(|c| c.label.eq_ignore_ascii_case(to))
        {
            return Ok(contact.clone());
        }
        // An exact address; policy decides whether it may be paid.
        let address: SolAddress = to.parse().map_err(|_| {
            Error::Invalid(format!(
                "{to} is neither a contact nor a Solana address; call list_contacts"
            ))
        })?;
        Ok(Recipient {
            label: to.chars().take(8).collect::<String>() + "…",
            address: Address::SolPubkey(address),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProposePaymentArgs {
    /// Contact name from list_contacts, or an exact Solana address.
    pub to: String,
    /// Decimal amount in the asset's major unit, e.g. "0.25".
    pub amount: String,
    /// Asset code, e.g. "SOL".
    pub asset: String,
    /// Short note shown to the recipient on chain.
    pub memo: Option<String>,
}

#[async_trait]
impl Tool for ProposePayment {
    type Args = ProposePaymentArgs;

    const NAME: &'static str = "propose_payment";
    const DESCRIPTION: &'static str = "Propose a payment. It is checked against the owner's spending policy: it may be sent, held for the owner's approval, or denied. Returns which.";

    async fn call(&self, ctx: &ToolCtx, args: Self::Args) -> Result<ToolOutput, Error> {
        let asset = self
            .assets
            .iter()
            .find(|a| a.code.as_str().eq_ignore_ascii_case(args.asset.trim()))
            .ok_or_else(|| Error::Invalid(format!("unsupported asset {}", args.asset)))?
            .clone();
        let minor = parse_major(&args.amount, asset.decimals)?;
        let memo = args
            .memo
            .map(|m| m.chars().take(140).collect::<String>())
            .filter(|m| !m.trim().is_empty());
        Ok(ToolOutput::ProposedPayment(Intent::new(
            IntentId::new(self.ids.next_uuid()),
            ctx.agent,
            Amount::new(minor, asset),
            self.recipient(&args.to)?,
            memo,
            IdempotencyKey::for_tool_call(ctx.run, &ctx.call_id)
                .map_err(|e| Error::Invalid(e.to_string()))?,
            Provenance {
                run: ctx.run,
                tool_call: ctx.call_id.clone(),
            },
            ctx.now,
        )))
    }
}

/// Lists the owner's saved payees.
#[derive(Debug)]
pub struct ListContacts {
    contacts: Vec<Recipient>,
}

impl ListContacts {
    pub fn new(contacts: Vec<Recipient>) -> Self {
        Self { contacts }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoArgs {}

#[async_trait]
impl Tool for ListContacts {
    type Args = NoArgs;

    const NAME: &'static str = "list_contacts";
    const DESCRIPTION: &'static str = "List the owner's saved payees with their addresses.";

    async fn call(&self, _: &ToolCtx, _: Self::Args) -> Result<ToolOutput, Error> {
        Ok(ToolOutput::Data(json!(self.contacts)))
    }
}

/// Reports where a proposed payment is.
#[derive(Debug)]
pub struct PaymentStatus {
    payments: Payments,
}

impl PaymentStatus {
    pub fn new(payments: Payments) -> Self {
        Self { payments }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PaymentStatusArgs {
    /// The intent_id propose_payment returned.
    pub intent_id: String,
}

#[async_trait]
impl Tool for PaymentStatus {
    type Args = PaymentStatusArgs;

    const NAME: &'static str = "payment_status";
    const DESCRIPTION: &'static str = "Look up a proposed payment by intent_id: whether it is awaiting approval, sent, confirmed, denied or failed.";

    async fn call(&self, _: &ToolCtx, args: Self::Args) -> Result<ToolOutput, Error> {
        let uuid = args
            .intent_id
            .trim()
            .parse()
            .map_err(|_| Error::Invalid(format!("bad intent_id {}", args.intent_id)))?;
        let Some((state, rail_ref)) = self.payments.status(IntentId::new(uuid)).await? else {
            return Ok(ToolOutput::Data(json!({ "error": "no such payment" })));
        };
        let signature = match rail_ref {
            Some(RailRef::Sol { signature, .. }) => signature,
            _ => None,
        };
        Ok(ToolOutput::Data(json!({
            "state": state.name(),
            "terminal": IntentState::is_terminal(state),
            "signature": signature,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_major;

    #[test]
    fn parses_major_units() {
        assert_eq!(parse_major("0.25", 9).unwrap(), 250_000_000);
        assert_eq!(parse_major("1", 6).unwrap(), 1_000_000);
        assert!(parse_major("0.0000000001", 9).is_err());
        assert!(parse_major("-1", 9).is_err());
        assert!(parse_major("abc", 9).is_err());
    }
}
