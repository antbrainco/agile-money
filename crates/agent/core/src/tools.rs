//! The tools an agent gets. Each one reads or proposes; none can sign.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use jiff::SignedDuration;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use warden_domain::ports::IdGen;
use warden_domain::{
    Address, Amount, Asset, Client, IdempotencyKey, Intent, IntentId, IntentState, Invoice,
    InvoiceId, InvoiceState, LineItem, PaymentRequest, Provenance, RailRef, Recipient, SolAddress,
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

/// Puts an invoice in front of the owner: records it, asks the rail for a payment
/// request, and sends the owner something to forward to the client. Whoever
/// implements it also watches for the payment.
#[async_trait]
pub trait Biller: Send + Sync {
    async fn issue(&self, invoice: Invoice) -> Result<PaymentRequest, Error>;
}

/// How long a payment request is watched for.
pub const REQUEST_LIFETIME: SignedDuration = SignedDuration::from_hours(7 * 24);

/// Asks a client to pay the owner. Money comes in, so no policy applies.
pub struct RequestPayment {
    assets: Vec<Asset>,
    biller: Arc<dyn Biller>,
    ids: Arc<dyn IdGen>,
}

impl std::fmt::Debug for RequestPayment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestPayment")
            .field("assets", &self.assets)
            .finish_non_exhaustive()
    }
}

impl RequestPayment {
    pub fn new(assets: Vec<Asset>, biller: Arc<dyn Biller>, ids: Arc<dyn IdGen>) -> Self {
        Self {
            assets,
            biller,
            ids,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RequestPaymentArgs {
    /// Who pays: the client's name, as the owner gave it.
    pub from: String,
    /// Decimal amount in the asset's major unit, e.g. "12.50".
    pub amount: String,
    /// Asset code, e.g. "USDC".
    pub asset: String,
    /// What it is for, in the owner's words. The payer sees it; on chain it is public.
    pub description: String,
}

#[async_trait]
impl Tool for RequestPayment {
    type Args = RequestPaymentArgs;

    const NAME: &'static str = "request_payment";
    const DESCRIPTION: &'static str = "Ask a client to pay the owner. Creates an invoice with a payment link and QR code (Solana Pay for crypto, PayPal checkout for USD), sends them to the owner to forward, and tells the owner when the payment arrives.";

    async fn call(&self, ctx: &ToolCtx, args: Self::Args) -> Result<ToolOutput, Error> {
        let asset = self
            .assets
            .iter()
            .find(|a| a.code.as_str().eq_ignore_ascii_case(args.asset.trim()))
            .ok_or_else(|| Error::Invalid(format!("unsupported asset {}", args.asset)))?
            .clone();
        let total = Amount::new(parse_major(&args.amount, asset.decimals)?, asset);
        let name: String = args.from.trim().chars().take(60).collect();
        if name.is_empty() {
            return Err(Error::Invalid("say who the request is for".into()));
        }
        let invoice = Invoice {
            id: InvoiceId::new(self.ids.next_uuid()),
            to: Client { name, email: None },
            lines: vec![LineItem {
                description: args.description.trim().chars().take(100).collect(),
                quantity: 1,
                unit_price: total.clone(),
            }],
            total: total.clone(),
            rail: total.asset.rail,
            state: InvoiceState::Sent,
            due: Some(ctx.now + REQUEST_LIFETIME),
        };
        let id = invoice.id;
        let client = invoice.to.name.clone();
        let request = self.biller.issue(invoice).await?;
        Ok(ToolOutput::Data(json!({
            "status": "sent_to_owner",
            "invoice_id": id.to_string(),
            "amount": total.to_string(),
            "from": client,
            "url": request.url,
            "note": "The owner now has the QR code and link to forward to the client. They will be told when the payment arrives; nothing has been paid yet.",
        })))
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
    use std::sync::Mutex;

    use jiff::Timestamp;
    use uuid::Uuid;
    use warden_domain::ports::SequentialIds;
    use warden_domain::{AgentId, RailId, RunId, SolAddress};

    use super::*;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<Invoice>>);

    #[async_trait]
    impl Biller for Recorder {
        async fn issue(&self, invoice: Invoice) -> Result<PaymentRequest, Error> {
            self.0.lock().unwrap().push(invoice);
            Ok(PaymentRequest {
                url: "solana:pay".into(),
                qr_png: None,
                rail_ref: RailRef::Sol {
                    reference: SolAddress::from_bytes([3; 32]),
                    signature: None,
                },
            })
        }
    }

    fn usdc() -> Asset {
        Asset {
            rail: RailId::Sol,
            code: "USDC".parse().unwrap(),
            decimals: 6,
            contract: None,
        }
    }

    fn ctx() -> ToolCtx {
        ToolCtx {
            agent: AgentId::new(Uuid::from_u128(1)),
            run: RunId::new(Uuid::from_u128(2)),
            call_id: "call-1".parse().unwrap(),
            now: Timestamp::UNIX_EPOCH,
        }
    }

    #[tokio::test]
    async fn request_payment_issues_one_invoice() {
        let biller = Arc::new(Recorder::default());
        let tool = RequestPayment::new(
            vec![usdc()],
            biller.clone(),
            Arc::new(SequentialIds::default()),
        );
        let args = RequestPaymentArgs {
            from: " Acme ".into(),
            amount: "12.50".into(),
            asset: "usdc".into(),
            description: "logo design".into(),
        };
        let ToolOutput::Data(out) = tool.call(&ctx(), args).await.unwrap() else {
            panic!("expected data");
        };
        assert_eq!(out["status"], "sent_to_owner");
        assert_eq!(out["url"], "solana:pay");
        let issued = biller.0.lock().unwrap();
        let [invoice] = issued.as_slice() else {
            panic!("expected one invoice");
        };
        assert_eq!(invoice.to.name, "Acme");
        assert_eq!(invoice.total, Amount::new(12_500_000, usdc()));
        assert_eq!(invoice.lines[0].description, "logo design");
        assert_eq!(invoice.state, InvoiceState::Sent);
        assert_eq!(invoice.due, Some(Timestamp::UNIX_EPOCH + REQUEST_LIFETIME));
    }

    #[tokio::test]
    async fn request_payment_refuses_unknown_assets() {
        let biller = Arc::new(Recorder::default());
        let tool = RequestPayment::new(
            vec![usdc()],
            biller.clone(),
            Arc::new(SequentialIds::default()),
        );
        let args = RequestPaymentArgs {
            from: "Acme".into(),
            amount: "1".into(),
            asset: "EUR".into(),
            description: "x".into(),
        };
        assert!(tool.call(&ctx(), args).await.is_err());
        assert!(biller.0.lock().unwrap().is_empty());
    }

    #[test]
    fn parses_major_units() {
        assert_eq!(parse_major("0.25", 9).unwrap(), 250_000_000);
        assert_eq!(parse_major("1", 6).unwrap(), 1_000_000);
        assert!(parse_major("0.0000000001", 9).is_err());
        assert!(parse_major("-1", 9).is_err());
        assert!(parse_major("abc", 9).is_err());
    }
}
