//! PayPal rail. Money in: an Orders API checkout link per invoice, captured once
//! the payer approves it. Money out: a Payouts API batch to a PayPal email.
//! Every write carries `PayPal-Request-Id`, so a retry gets the first response
//! back instead of charging or paying twice (rule 10). Sandbox unless
//! `live = true` (rule 5). Live network tests go behind the `live` feature.

use std::time::Duration;

use agile_money_domain::ports::{Rail, RailError};
use agile_money_domain::{
    Address, Amount, Approved, Asset, IdempotencyKey, Intent, Invoice, PaymentRequest,
    PaypalOrderId, RailId, RailRef, Settlement, Submission,
};
use async_trait::async_trait;
use jiff::Timestamp;
use reqwest::header::CONTENT_TYPE;
use reqwest::{Method, Response, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::Instant;
use url::Url;

pub const RAIL_ID: RailId = RailId::Paypal;

pub const SANDBOX_URL: &str = "https://api-m.sandbox.paypal.com";
pub const LIVE_URL: &str = "https://api-m.paypal.com";

/// Whole calls; PayPal answers in well under a second when healthy.
const TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Refresh the access token this long before PayPal says it expires.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);
/// PayPal's limit on `purchase_units[].description`.
const DESCRIPTION_MAX: usize = 127;

/// US dollars: 2 decimals, 1 USD = 100 cents.
pub fn usd() -> Asset {
    Asset {
        rail: RailId::Paypal,
        code: "USD"
            .parse()
            .unwrap_or_else(|_| unreachable!("USD is a valid code")),
        decimals: 2,
        contract: None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("rail misconfigured: {0}")]
    Config(String),
}

/// `[rail.paypal]` plus `AGILE_MONEY_RAIL__PAYPAL__CLIENT_ID` and
/// `AGILE_MONEY_RAIL__PAYPAL__CLIENT_SECRET`, from a REST app at developer.paypal.com.
#[derive(Debug, Clone, Deserialize)]
pub struct PaypalConfig {
    pub client_id: SecretString,
    pub client_secret: SecretString,
    /// Real money. Off means the sandbox (rule 5).
    #[serde(default)]
    pub live: bool,
    /// Overrides the API host, e.g. for a mock. Defaults by `live`.
    pub base_url: Option<Url>,
    /// Where PayPal sends the payer after approving or cancelling. Optional:
    /// approved orders are captured by polling either way.
    pub return_url: Option<Url>,
    pub cancel_url: Option<Url>,
}

/// Where an invoice's order stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Collection {
    /// Not paid yet, or the capture is still pending.
    Waiting,
    Collected(Collected),
    /// It can never be paid: voided, or unknown to these credentials.
    Gone(&'static str),
}

/// What an invoice's order has collected so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collected {
    /// The last completed capture.
    pub capture_id: String,
    /// Sum of completed captures, in the invoice asset's minor units.
    pub minor: u128,
}

struct Token {
    value: SecretString,
    until: Instant,
}

pub struct PaypalRail {
    http: reqwest::Client,
    base: Url,
    client_id: SecretString,
    client_secret: SecretString,
    return_url: Option<Url>,
    cancel_url: Option<Url>,
    token: Mutex<Option<Token>>,
}

impl std::fmt::Debug for PaypalRail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaypalRail")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

impl PaypalRail {
    pub fn new(config: &PaypalConfig) -> Result<Self, Error> {
        let live = Url::parse(LIVE_URL).map_err(|e| Error::Config(e.to_string()))?;
        let base = match (&config.base_url, config.live) {
            (Some(url), _) => url.clone(),
            (None, true) => live.clone(),
            (None, false) => Url::parse(SANDBOX_URL).map_err(|e| Error::Config(e.to_string()))?,
        };
        if base.host_str() == live.host_str() && !config.live {
            return Err(Error::Config(
                "the live PayPal API needs rail.paypal.live = true".into(),
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| Error::Config(e.to_string()))?;
        Ok(Self {
            http,
            base,
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            return_url: config.return_url.clone(),
            cancel_url: config.cancel_url.clone(),
            token: Mutex::new(None),
        })
    }

    /// Checks the credentials by fetching an access token.
    pub async fn check(&self) -> Result<(), RailError> {
        self.token().await.map(|_| ())
    }

    /// Captures the invoice's order once the payer has approved it, then sums
    /// its completed captures in `asset`.
    /// Safe to call any number of times: the capture is deduped by its own
    /// `PayPal-Request-Id`.
    pub async fn collect(
        &self,
        order: &PaypalOrderId,
        asset: &Asset,
    ) -> Result<Collection, RailError> {
        let path = format!("/v2/checkout/orders/{order}");
        let (status, body) = self.exchange(Method::GET, &path, None, None).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(Collection::Gone("the PayPal order no longer exists"));
        }
        let mut current: Order = decode(&path, status, body)?;
        if current.status == "VOIDED" {
            return Ok(Collection::Gone("the PayPal order was voided"));
        }
        if current.status == "APPROVED" {
            let capture = format!("capture:{order}");
            current = match self
                .call(
                    Method::POST,
                    &format!("{path}/capture"),
                    Some(&capture),
                    Some(&json!({})),
                )
                .await
            {
                Ok(order) => order,
                // Captured by an attempt whose reply was lost: read it back.
                Err(RailError::Rejected(why)) => {
                    tracing::info!(%order, %why, "capture refused; rereading the order");
                    self.call(Method::GET, &path, None, None).await?
                }
                Err(e) => return Err(e),
            };
            tracing::info!(%order, status = %current.status, "order captured");
        }
        if current.status != "COMPLETED" {
            return Ok(Collection::Waiting);
        }
        let mut collected: Option<Collected> = None;
        for capture in current
            .purchase_units
            .into_iter()
            .flat_map(|u| u.payments.captures)
            .filter(|c| c.status == "COMPLETED" && c.amount.currency_code == asset.code.as_str())
        {
            let minor = parse_value(&capture.amount.value, asset.decimals).ok_or_else(|| {
                RailError::Transport(format!("capture amount {:?}", capture.amount.value))
            })?;
            let so_far = collected.as_ref().map_or(0, |c| c.minor);
            collected = Some(Collected {
                capture_id: capture.id,
                minor: so_far.saturating_add(minor),
            });
        }
        Ok(collected.map_or(Collection::Waiting, Collection::Collected))
    }

    fn url(&self, path: &str) -> Result<Url, RailError> {
        self.base
            .join(path)
            .map_err(|e| RailError::Transport(format!("bad path {path}: {e}")))
    }

    /// A cached OAuth access token, fetched with the client credentials.
    async fn token(&self) -> Result<SecretString, RailError> {
        let mut slot = self.token.lock().await;
        if let Some(token) = slot.as_ref().filter(|t| t.until > Instant::now()) {
            return Ok(token.value.clone());
        }
        let response = self
            .http
            .post(self.url("/v1/oauth2/token")?)
            .basic_auth(
                self.client_id.expose_secret(),
                Some(self.client_secret.expose_secret()),
            )
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body("grant_type=client_credentials")
            .send()
            .await
            .map_err(transport)?;
        let status = response.status();
        let body = read(response).await?;
        if status == StatusCode::UNAUTHORIZED {
            return Err(RailError::Rejected(
                "PayPal refused the client id and secret".into(),
            ));
        }
        if !status.is_success() {
            return Err(classify(status, &body));
        }
        #[derive(Deserialize)]
        struct Granted {
            access_token: SecretString,
            expires_in: u64,
        }
        let granted: Granted = serde_json::from_value(body)
            .map_err(|e| RailError::Transport(format!("token response: {e}")))?;
        let lifetime = Duration::from_secs(granted.expires_in).saturating_sub(TOKEN_MARGIN);
        *slot = Some(Token {
            value: granted.access_token.clone(),
            until: Instant::now() + lifetime,
        });
        Ok(granted.access_token)
    }

    /// One API call, decoded; any non-2xx is an error.
    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        request_id: Option<&str>,
        body: Option<&Value>,
    ) -> Result<T, RailError> {
        let (status, body) = self.exchange(method, path, request_id, body).await?;
        decode(path, status, body)
    }

    /// One API call's status and body. A 401 drops the cached token and tries
    /// once more.
    async fn exchange(
        &self,
        method: Method,
        path: &str,
        request_id: Option<&str>,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value), RailError> {
        for attempt in 0..2 {
            let token = self.token().await?;
            let mut request = self
                .http
                .request(method.clone(), self.url(path)?)
                .bearer_auth(token.expose_secret())
                // The whole resource back, so a create carries its links and a
                // capture its amounts.
                .header("Prefer", "return=representation");
            if let Some(id) = request_id {
                request = request.header("PayPal-Request-Id", id);
            }
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await.map_err(transport)?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                *self.token.lock().await = None;
                continue;
            }
            return Ok((status, read(response).await?));
        }
        Err(RailError::Rejected(
            "PayPal refused a fresh access token".into(),
        ))
    }
}

#[derive(Debug, Deserialize)]
struct Order {
    id: PaypalOrderId,
    status: String,
    #[serde(default)]
    links: Vec<Link>,
    #[serde(default)]
    purchase_units: Vec<Unit>,
}

impl Order {
    fn link(&self, rel: &str) -> Option<&str> {
        self.links
            .iter()
            .find(|l| l.rel == rel)
            .map(|l| l.href.as_str())
    }
}

#[derive(Debug, Deserialize)]
struct Link {
    href: String,
    rel: String,
}

#[derive(Debug, Deserialize)]
struct Unit {
    #[serde(default)]
    payments: Payments,
}

#[derive(Debug, Default, Deserialize)]
struct Payments {
    #[serde(default)]
    captures: Vec<Capture>,
}

#[derive(Debug, Deserialize)]
struct Capture {
    id: String,
    status: String,
    amount: Money,
}

#[derive(Debug, Deserialize)]
struct Money {
    currency_code: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct Batch {
    batch_header: BatchHeader,
    #[serde(default)]
    items: Vec<BatchItem>,
}

#[derive(Debug, Deserialize)]
struct BatchHeader {
    payout_batch_id: PaypalOrderId,
    batch_status: String,
}

#[derive(Debug, Deserialize)]
struct BatchItem {
    transaction_status: String,
}

#[async_trait]
impl Rail for PaypalRail {
    fn id(&self) -> RailId {
        RAIL_ID
    }

    fn supports(&self, asset: &Asset) -> bool {
        *asset == usd()
    }

    /// A PayPal checkout link for the invoice total. The order's `invoice_id`
    /// is the invoice's, so PayPal also refuses to capture one invoice twice.
    async fn request(&self, inv: &Invoice) -> Result<PaymentRequest, RailError> {
        if !self.supports(&inv.total.asset) {
            return Err(RailError::Unsupported);
        }
        let request_id = key(format!("invoice:{}", inv.id))?;
        let mut experience = json!({
            "brand_name": "Agile Money",
            "user_action": "PAY_NOW",
            "shipping_preference": "NO_SHIPPING",
        });
        if let Some(url) = &self.return_url {
            experience["return_url"] = json!(url);
        }
        if let Some(url) = &self.cancel_url {
            experience["cancel_url"] = json!(url);
        }
        let body = json!({
            "intent": "CAPTURE",
            "purchase_units": [{
                "reference_id": inv.id.to_string(),
                "invoice_id": inv.id.to_string(),
                "description": description(inv),
                "amount": {
                    "currency_code": inv.total.asset.code.as_str(),
                    "value": value(&inv.total),
                },
            }],
            "payment_source": { "paypal": { "experience_context": experience } },
        });
        let order: Order = self
            .call(
                Method::POST,
                "/v2/checkout/orders",
                Some(request_id.as_str()),
                Some(&body),
            )
            .await?;
        let url = order
            .link("payer-action")
            .or_else(|| order.link("approve"))
            .ok_or_else(|| {
                RailError::Transport(format!("order {} has no approval link", order.id))
            })?
            .to_owned();
        tracing::info!(invoice = %inv.id, order = %order.id, "checkout order created");
        Ok(PaymentRequest {
            url,
            qr_png: None,
            rail_ref: RailRef::Paypal {
                order_id: order.id,
                request_id,
            },
        })
    }

    /// One payout to the recipient's PayPal email. The idempotency key is both
    /// the `PayPal-Request-Id` and the batch's `sender_batch_id`, which PayPal
    /// refuses to reuse.
    #[tracing::instrument(skip_all, fields(intent = %intent.id))]
    async fn submit(&self, intent: &Intent<Approved>) -> Result<Submission, RailError> {
        if !self.supports(&intent.amount.asset) {
            return Err(RailError::Unsupported);
        }
        let Address::Email(email) = &intent.to.address else {
            return Err(RailError::Rejected(
                "PayPal pays a PayPal account's email".into(),
            ));
        };
        let note = intent
            .memo
            .clone()
            .unwrap_or_else(|| format!("Payment from Agile Money ({})", intent.id));
        let key = intent.idempotency_key.as_str();
        let body = json!({
            "sender_batch_header": {
                "sender_batch_id": key,
                "email_subject": "You have a payment",
                "email_message": note,
            },
            "items": [{
                "recipient_type": "EMAIL",
                "receiver": email.as_str(),
                "amount": {
                    "value": value(&intent.amount),
                    "currency": intent.amount.asset.code.as_str(),
                },
                "note": note,
                "sender_item_id": intent.id.to_string(),
            }],
        });
        let batch: Batch = self
            .call(Method::POST, "/v1/payments/payouts", Some(key), Some(&body))
            .await?;
        let batch_id = batch.batch_header.payout_batch_id;
        tracing::info!(%batch_id, amount = %intent.amount, "payout sent");
        Ok(Submission {
            intent: intent.id,
            rail_ref: RailRef::Paypal {
                order_id: batch_id,
                request_id: intent.idempotency_key.clone(),
            },
            at: Timestamp::now(),
        })
    }

    /// Where a payout from `submit` stands. An unclaimed payout (no PayPal
    /// account at that email yet) stays pending: PayPal holds it for 30 days.
    async fn status(&self, sub: &Submission) -> Result<Settlement, RailError> {
        let RailRef::Paypal { order_id, .. } = &sub.rail_ref else {
            return Err(RailError::Unsupported);
        };
        let batch: Batch = self
            .call(
                Method::GET,
                &format!("/v1/payments/payouts/{order_id}"),
                None,
                None,
            )
            .await?;
        Ok(settlement(&batch))
    }
}

fn settlement(batch: &Batch) -> Settlement {
    let failed = |status: &str| Settlement::Failed {
        reason: format!("payout {}", status.to_ascii_lowercase()),
    };
    match batch.items.first().map(|i| i.transaction_status.as_str()) {
        Some("SUCCESS") => Settlement::Confirmed {
            at: Timestamp::now(),
        },
        Some(s @ ("FAILED" | "RETURNED" | "BLOCKED" | "REFUNDED" | "REVERSED" | "DENIED")) => {
            failed(s)
        }
        Some(_) => Settlement::Pending,
        None => match batch.batch_header.batch_status.as_str() {
            s @ ("DENIED" | "CANCELED") => failed(s),
            _ => Settlement::Pending,
        },
    }
}

/// What the payer sees on PayPal's checkout page.
fn description(inv: &Invoice) -> String {
    let text = match inv.lines.first() {
        Some(line) if !line.description.trim().is_empty() => {
            format!("{} - {}", inv.to.name, line.description.trim())
        }
        _ => format!("Invoice for {}", inv.to.name),
    };
    text.chars().take(DESCRIPTION_MAX).collect()
}

fn key(text: String) -> Result<IdempotencyKey, RailError> {
    text.parse()
        .map_err(|e| RailError::Rejected(format!("request id: {e}")))
}

/// PayPal's decimal string with every decimal written out, e.g. `"2.50"`.
fn value(amount: &Amount) -> String {
    let decimals = usize::from(amount.asset.decimals);
    if decimals == 0 {
        return amount.minor.to_string();
    }
    let scale = 10u128.pow(u32::from(amount.asset.decimals));
    format!(
        "{}.{:0decimals$}",
        amount.minor / scale,
        amount.minor % scale
    )
}

/// `"2.5"` or `"2.50"` back to minor units; `None` for anything else.
fn parse_value(text: &str, decimals: u8) -> Option<u128> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(frac) || frac.len() > usize::from(decimals) {
        return None;
    }
    let scale = 10u128.checked_pow(u32::from(decimals))?;
    let frac = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<width$}", width = usize::from(decimals))
            .parse::<u128>()
            .ok()?
    };
    whole
        .parse::<u128>()
        .ok()?
        .checked_mul(scale)?
        .checked_add(frac)
}

fn decode<T: DeserializeOwned>(
    path: &str,
    status: StatusCode,
    body: Value,
) -> Result<T, RailError> {
    if !status.is_success() {
        return Err(classify(status, &body));
    }
    serde_json::from_value(body)
        .map_err(|e| RailError::Transport(format!("{path}: unexpected reply: {e}")))
}

async fn read(response: Response) -> Result<Value, RailError> {
    let bytes = response.bytes().await.map_err(transport)?;
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|e| RailError::Transport(format!("not JSON: {e}")))
}

fn transport(e: reqwest::Error) -> RailError {
    if e.is_timeout() {
        RailError::Timeout
    } else {
        RailError::Transport(e.without_url().to_string())
    }
}

/// Rule 13: only failures where PayPal did nothing, or a retry is deduped, are
/// retryable. Every write carries a request id, so 408, 429 and 5xx qualify.
fn classify(status: StatusCode, body: &Value) -> RailError {
    let name = body["name"]
        .as_str()
        .or_else(|| body["error"].as_str())
        .unwrap_or("error");
    let detail = body["details"][0]["issue"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .or_else(|| body["error_description"].as_str())
        .unwrap_or_default();
    let message = format!("PayPal {}: {name} {detail}", status.as_u16())
        .trim_end()
        .to_owned();
    if status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
    {
        RailError::Transport(message)
    } else {
        RailError::Rejected(message)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agile_money_domain::{
        AgentId, Client, Decision, IntentId, InvoiceId, InvoiceState, LineItem, Provenance,
        Recipient, RunId,
    };
    use uuid::Uuid;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ORDER: &str = "5O190127TN364715T";

    fn cents(minor: u128) -> Amount {
        Amount::new(minor, usd())
    }

    async fn rail(server: &MockServer) -> PaypalRail {
        Mock::given(method("POST"))
            .and(path("/v1/oauth2/token"))
            .and(header("authorization", "Basic aWQ6c2VjcmV0")) // id:secret
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "token-1",
                "token_type": "Bearer",
                "expires_in": 32400,
            })))
            .mount(server)
            .await;
        PaypalRail::new(&PaypalConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
            live: false,
            base_url: Some(server.uri().parse().unwrap()),
            return_url: Some("https://agile-money.example/paid".parse().unwrap()),
            cancel_url: None,
        })
        .unwrap()
    }

    async fn token_fetches(server: &MockServer) -> usize {
        let requests = server.received_requests().await.unwrap_or_default();
        requests
            .iter()
            .filter(|r| r.url.path() == "/v1/oauth2/token")
            .count()
    }

    fn invoice() -> Invoice {
        let total = cents(1250);
        Invoice {
            id: InvoiceId::new(Uuid::from_u128(7)),
            to: Client {
                name: "Dana".into(),
                email: None,
            },
            lines: vec![LineItem {
                description: "logo design".into(),
                quantity: 1,
                unit_price: total.clone(),
            }],
            total,
            rail: RailId::Paypal,
            state: InvoiceState::Sent,
            due: None,
        }
    }

    fn order(status: &str, captures: Value) -> Value {
        json!({
            "id": ORDER,
            "status": status,
            "purchase_units": [{ "payments": { "captures": captures } }],
            "links": [],
        })
    }

    fn intent(to: Address) -> Intent<Approved> {
        let id = IntentId::new(Uuid::from_u128(9));
        Intent::new(
            id,
            AgentId::new(Uuid::from_u128(1)),
            cents(500),
            Recipient {
                label: "Acme".into(),
                address: to,
            },
            Some("fonts".into()),
            IdempotencyKey::for_intent(id),
            Provenance {
                run: RunId::new(Uuid::from_u128(2)),
                tool_call: "call-1".parse().unwrap(),
            },
            Timestamp::now(),
        )
        .allow(&Decision::Allow)
        .unwrap()
    }

    fn email() -> Address {
        Address::Email("acme@example.com".parse().unwrap())
    }

    #[tokio::test]
    async fn request_creates_a_deduped_checkout_order() {
        let server = MockServer::start().await;
        let rail = rail(&server).await;
        let request_id = format!("invoice:{}", Uuid::from_u128(7));
        Mock::given(method("POST"))
            .and(path("/v2/checkout/orders"))
            .and(header("authorization", "Bearer token-1"))
            .and(header("paypal-request-id", request_id.as_str()))
            .and(body_partial_json(json!({
                "intent": "CAPTURE",
                "purchase_units": [{
                    "description": "Dana - logo design",
                    "amount": { "currency_code": "USD", "value": "12.50" },
                }],
                "payment_source": { "paypal": { "experience_context": {
                    "return_url": "https://agile-money.example/paid",
                    "user_action": "PAY_NOW",
                }}},
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": ORDER,
                "status": "PAYER_ACTION_REQUIRED",
                "links": [
                    { "href": "https://api.sandbox.paypal.com/v2/checkout/orders/x", "rel": "self" },
                    { "href": "https://www.sandbox.paypal.com/checkoutnow?token=5O190127TN364715T", "rel": "payer-action" },
                ],
            })))
            .expect(2)
            .mount(&server)
            .await;

        let first = rail.request(&invoice()).await.unwrap();
        let again = rail.request(&invoice()).await.unwrap();
        assert_eq!(first, again);
        assert_eq!(token_fetches(&server).await, 1, "the token is cached");
        assert_eq!(
            first.url,
            "https://www.sandbox.paypal.com/checkoutnow?token=5O190127TN364715T"
        );
        assert_eq!(
            first.rail_ref,
            RailRef::Paypal {
                order_id: ORDER.parse().unwrap(),
                request_id: request_id.parse().unwrap(),
            }
        );
    }

    #[tokio::test]
    async fn only_usd_is_requested() {
        let server = MockServer::start().await;
        let rail = PaypalRail::new(&PaypalConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
            live: false,
            base_url: Some(server.uri().parse().unwrap()),
            return_url: None,
            cancel_url: None,
        })
        .unwrap();
        let mut inv = invoice();
        inv.total.asset.code = "EUR".parse().unwrap();
        assert!(matches!(
            rail.request(&inv).await,
            Err(RailError::Unsupported)
        ));
    }

    #[tokio::test]
    async fn an_approved_order_is_captured_once() {
        let server = MockServer::start().await;
        let rail = rail(&server).await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/checkout/orders/{ORDER}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(order("APPROVED", json!([]))))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/v2/checkout/orders/{ORDER}/capture")))
            .and(header(
                "paypal-request-id",
                format!("capture:{ORDER}").as_str(),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(order(
                "COMPLETED",
                json!([{ "id": "3C679366HH908993F", "status": "COMPLETED",
                         "amount": { "currency_code": "USD", "value": "12.50" } }]),
            )))
            .expect(1)
            .mount(&server)
            .await;

        let got = rail.collect(&ORDER.parse().unwrap(), &usd()).await.unwrap();
        assert_eq!(
            got,
            Collection::Collected(Collected {
                capture_id: "3C679366HH908993F".into(),
                minor: 1250,
            })
        );
    }

    #[tokio::test]
    async fn an_unapproved_order_is_left_alone() {
        let server = MockServer::start().await;
        let rail = rail(&server).await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/checkout/orders/{ORDER}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(order("PAYER_ACTION_REQUIRED", json!([]))),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/v2/checkout/orders/{ORDER}/capture")))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&server)
            .await;
        let got = rail.collect(&ORDER.parse().unwrap(), &usd()).await.unwrap();
        assert_eq!(got, Collection::Waiting);
    }

    #[tokio::test]
    async fn pending_captures_do_not_count() {
        let server = MockServer::start().await;
        let rail = rail(&server).await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/checkout/orders/{ORDER}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(order(
                "COMPLETED",
                json!([{ "id": "A", "status": "PENDING",
                         "amount": { "currency_code": "USD", "value": "12.50" } }]),
            )))
            .mount(&server)
            .await;
        let got = rail.collect(&ORDER.parse().unwrap(), &usd()).await.unwrap();
        assert_eq!(got, Collection::Waiting);
    }

    #[tokio::test]
    async fn vanished_and_voided_orders_are_gone() {
        let server = MockServer::start().await;
        let rail = rail(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/checkout/orders/GONE"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "name": "RESOURCE_NOT_FOUND",
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/checkout/orders/{ORDER}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(order("VOIDED", json!([]))))
            .mount(&server)
            .await;
        let gone = rail
            .collect(&"GONE".parse().unwrap(), &usd())
            .await
            .unwrap();
        assert!(matches!(gone, Collection::Gone(_)), "{gone:?}");
        let voided = rail.collect(&ORDER.parse().unwrap(), &usd()).await.unwrap();
        assert!(matches!(voided, Collection::Gone(_)), "{voided:?}");
    }

    #[tokio::test]
    async fn submit_sends_one_payout_keyed_by_the_intent() {
        let server = MockServer::start().await;
        let rail = rail(&server).await;
        let intent = intent(email());
        let key = intent.idempotency_key.as_str().to_owned();
        Mock::given(method("POST"))
            .and(path("/v1/payments/payouts"))
            .and(header("paypal-request-id", key.as_str()))
            .and(body_partial_json(json!({
                "sender_batch_header": { "sender_batch_id": key },
                "items": [{
                    "recipient_type": "EMAIL",
                    "receiver": "acme@example.com",
                    "amount": { "value": "5.00", "currency": "USD" },
                    "note": "fonts",
                }],
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "batch_header": { "payout_batch_id": "5UXD2E8A7EBQJ", "batch_status": "PENDING" },
            })))
            .expect(1)
            .mount(&server)
            .await;

        let sub = rail.submit(&intent).await.unwrap();
        assert_eq!(
            sub.rail_ref,
            RailRef::Paypal {
                order_id: "5UXD2E8A7EBQJ".parse().unwrap(),
                request_id: intent.idempotency_key.clone(),
            }
        );
    }

    #[tokio::test]
    async fn submit_needs_an_email() {
        let server = MockServer::start().await;
        let rail = rail(&server).await;
        let to = Address::SolPubkey(
            "7Np41oeYqPefeNQEHSv1UDhYrehxin3NStELsSKCT4K2"
                .parse()
                .unwrap(),
        );
        let err = rail.submit(&intent(to)).await.unwrap_err();
        assert!(matches!(err, RailError::Rejected(_)), "{err:?}");
        assert!(!err.retryable());
    }

    #[tokio::test]
    async fn status_follows_the_payout_item() {
        let server = MockServer::start().await;
        let rail = rail(&server).await;
        Mock::given(method("GET"))
            .and(path("/v1/payments/payouts/5UXD2E8A7EBQJ"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "batch_header": { "payout_batch_id": "5UXD2E8A7EBQJ", "batch_status": "SUCCESS" },
                "items": [{ "transaction_status": "SUCCESS" }],
            })))
            .mount(&server)
            .await;
        let sub = Submission {
            intent: IntentId::new(Uuid::from_u128(9)),
            rail_ref: RailRef::Paypal {
                order_id: "5UXD2E8A7EBQJ".parse().unwrap(),
                request_id: "intent:x".parse().unwrap(),
            },
            at: Timestamp::now(),
        };
        assert!(matches!(
            rail.status(&sub).await.unwrap(),
            Settlement::Confirmed { .. }
        ));
    }

    #[test]
    fn payout_statuses_map_to_settlements() {
        let batch = |batch: &str, item: Option<&str>| Batch {
            batch_header: BatchHeader {
                payout_batch_id: "B".parse().unwrap(),
                batch_status: batch.into(),
            },
            items: item
                .map(|s| BatchItem {
                    transaction_status: s.into(),
                })
                .into_iter()
                .collect(),
        };
        assert_eq!(
            settlement(&batch("SUCCESS", Some("UNCLAIMED"))),
            Settlement::Pending
        );
        assert_eq!(
            settlement(&batch("SUCCESS", Some("RETURNED"))),
            Settlement::Failed {
                reason: "payout returned".into()
            }
        );
        assert_eq!(settlement(&batch("PROCESSING", None)), Settlement::Pending);
        assert!(matches!(
            settlement(&batch("DENIED", None)),
            Settlement::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn server_errors_retry_and_refusals_do_not() {
        // A 404 on an order is `Gone`; other refusals stay errors.
        let server = MockServer::start().await;
        let rail = rail(&server).await;
        Mock::given(method("GET"))
            .and(path("/v2/checkout/orders/BUSY"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/checkout/orders/GONE"))
            .respond_with(ResponseTemplate::new(422).set_body_json(json!({
                "name": "UNPROCESSABLE_ENTITY",
                "details": [{ "issue": "ORDER_NOT_APPROVED" }],
            })))
            .mount(&server)
            .await;
        let busy = rail
            .collect(&"BUSY".parse().unwrap(), &usd())
            .await
            .unwrap_err();
        assert!(busy.retryable(), "{busy:?}");
        let gone = rail
            .collect(&"GONE".parse().unwrap(), &usd())
            .await
            .unwrap_err();
        assert!(!gone.retryable());
        assert_eq!(
            gone.to_string(),
            "rail rejected the payment: PayPal 422: UNPROCESSABLE_ENTITY ORDER_NOT_APPROVED"
        );
    }

    #[tokio::test]
    async fn an_expired_token_is_refreshed_once() {
        let server = MockServer::start().await;
        let rail = rail(&server).await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/checkout/orders/{ORDER}")))
            .respond_with(ResponseTemplate::new(401))
            .expect(2)
            .mount(&server)
            .await;
        let err = rail
            .collect(&ORDER.parse().unwrap(), &usd())
            .await
            .unwrap_err();
        assert!(matches!(err, RailError::Rejected(_)), "{err:?}");
        assert_eq!(token_fetches(&server).await, 2, "one refresh, then give up");
    }

    #[test]
    fn the_live_api_needs_the_opt_in() {
        let config = |live: bool, base: Option<&str>| PaypalConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
            live,
            base_url: base.map(|b| b.parse().unwrap()),
            return_url: None,
            cancel_url: None,
        };
        assert!(PaypalRail::new(&config(false, Some(LIVE_URL))).is_err());
        let sandbox = PaypalRail::new(&config(false, None)).unwrap();
        assert_eq!(sandbox.base.as_str(), "https://api-m.sandbox.paypal.com/");
        let live = PaypalRail::new(&config(true, None)).unwrap();
        assert_eq!(live.base.as_str(), "https://api-m.paypal.com/");
    }

    #[test]
    fn values_are_exact_decimal_strings() {
        assert_eq!(value(&cents(1250)), "12.50");
        assert_eq!(value(&cents(5)), "0.05");
        assert_eq!(parse_value("12.50", 2), Some(1250));
        assert_eq!(parse_value("12.5", 2), Some(1250));
        assert_eq!(parse_value("12", 2), Some(1200));
        assert_eq!(parse_value("0.001", 2), None);
        assert_eq!(parse_value("-1.00", 2), None);
        assert_eq!(parse_value("", 2), None);
    }
}
