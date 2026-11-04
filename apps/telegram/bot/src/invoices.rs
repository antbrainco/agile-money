//! Payment requests: the owner gets a QR code and link to forward to a client
//! (Solana Pay, or PayPal checkout for USD), and a watcher tells them when the
//! money arrives.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jiff::Timestamp;
use teloxide::prelude::*;
use teloxide::types::InputFile;
use warden_agent_core::Error as AgentError;
use warden_agent_core::tools::{Biller, REQUEST_LIFETIME};
use warden_domain::ports::{EventStore, Rail, StoreError};
use warden_domain::{Amount, Event, Invoice, PaymentRequest, RailId, RailRef, StreamId};
use warden_rail_paypal::PaypalRail;
use warden_rail_sol::{Cluster, SolRail};
use warden_store::PgStore;

use crate::telegram::ChatTurn;

/// How often open requests are checked on chain.
pub const WATCH_EVERY: Duration = Duration::from_secs(10);

/// Pixels per QR module, and the quiet zone around the code in modules.
const SCALE: usize = 8;
const QUIET: usize = 4;

/// Records the invoice, then sends the owner its QR code and link.
#[derive(Debug, Clone)]
pub struct TelegramBiller {
    pub rail: Arc<SolRail>,
    pub paypal: Option<Arc<PaypalRail>>,
    pub store: PgStore,
    pub bot: Bot,
    pub chat: ChatId,
}

#[async_trait]
impl Biller for TelegramBiller {
    async fn issue(&self, invoice: Invoice) -> Result<PaymentRequest, AgentError> {
        let fail = |e: &dyn std::fmt::Display| AgentError::Billing(e.to_string());
        let rail: &dyn Rail = match invoice.total.asset.rail {
            RailId::Sol => self.rail.as_ref(),
            RailId::Paypal => match &self.paypal {
                Some(paypal) => paypal.as_ref(),
                None => return Err(AgentError::Billing("PayPal is not set up".into())),
            },
            other => {
                return Err(AgentError::Billing(format!(
                    "{other:?} can't take requests"
                )));
            }
        };
        let request = rail.request(&invoice).await.map_err(|e| fail(&e))?;
        let png = qr_png(&request.url).map_err(|e| fail(&e))?;
        self.store
            .append(
                &StreamId::Invoice(invoice.id),
                0,
                vec![Event::InvoiceIssued {
                    invoice: invoice.id,
                    details: invoice.clone(),
                    request: request.clone(),
                }],
                vec![],
            )
            .await
            .map_err(|e| fail(&e))?;
        // Sent inside the agent's turn, so it lands above the reply about it.
        self.bot
            .send_photo(self.chat, InputFile::memory(png).file_name("request.png"))
            .caption(caption(&invoice, &request))
            .await
            .map_err(|e| fail(&e))?;
        tracing::info!(invoice = %invoice.id, total = %invoice.total, "payment request issued");
        Ok(request)
    }
}

fn what_for(invoice: &Invoice) -> String {
    invoice
        .lines
        .first()
        .map(|l| l.description.trim())
        .filter(|d| !d.is_empty())
        .map(|d| format!(" for {d}"))
        .unwrap_or_default()
}

fn caption(invoice: &Invoice, request: &PaymentRequest) -> String {
    let how = match request.rail_ref {
        RailRef::Paypal { .. } => "they can pay it by PayPal or card",
        _ => "any Solana Pay wallet can pay it",
    };
    format!(
        "Payment request: {} from {}{}.\n\nForward this QR code or link; {how}. I'll tell \
         you when it arrives.\n\n{}",
        invoice.total,
        invoice.to.name,
        what_for(invoice),
        request.url
    )
}

/// The URL as a black-on-white QR code PNG.
fn qr_png(url: &str) -> anyhow::Result<Vec<u8>> {
    let code = qrcode::QrCode::new(url.as_bytes())?;
    let modules = code.width();
    let dark: Vec<bool> = code
        .to_colors()
        .into_iter()
        .map(|c| c == qrcode::Color::Dark)
        .collect();
    let side = (modules + 2 * QUIET) * SCALE;
    let mut pixels = vec![255u8; side * side];
    for (i, _) in dark.iter().enumerate().filter(|(_, d)| **d) {
        let (x, y) = ((i % modules + QUIET) * SCALE, (i / modules + QUIET) * SCALE);
        for row in pixels[y * side..].chunks_mut(side).take(SCALE) {
            row[x..x + SCALE].fill(0);
        }
    }
    let side = u32::try_from(side)?;
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, side, side);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&pixels)?;
    Ok(out)
}

/// Checks open requests on chain and records the ones that have been paid.
#[derive(Debug, Clone)]
pub struct Watcher {
    pub rail: Arc<SolRail>,
    pub paypal: Option<Arc<PaypalRail>>,
    pub store: PgStore,
    pub bot: Bot,
    pub chat: ChatId,
    pub cluster: Cluster,
    pub turn: ChatTurn,
}

impl Watcher {
    pub async fn run(self) {
        let mut every = tokio::time::interval(WATCH_EVERY);
        loop {
            every.tick().await;
            if let Err(e) = self.check().await {
                tracing::warn!(error = %e, "payment request check failed");
            }
        }
    }

    async fn check(&self) -> anyhow::Result<()> {
        let open = self
            .store
            .open_invoices(Timestamp::now() - REQUEST_LIFETIME)
            .await?;
        for (invoice, request) in open {
            let arrived = match self.arrived(&invoice, request.rail_ref).await {
                Ok(Some(arrived)) => arrived,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(invoice = %invoice.id, error = %e, "could not read payments");
                    continue;
                }
            };
            if arrived.minor < invoice.total.minor {
                tracing::info!(invoice = %invoice.id, received = arrived.minor, "partly paid");
                continue;
            }
            let received = Amount::new(arrived.minor, invoice.total.asset.clone());
            let paid = Event::InvoicePaid {
                invoice: invoice.id,
                received: received.clone(),
                rail_ref: arrived.rail_ref,
            };
            match self
                .store
                .append(&StreamId::Invoice(invoice.id), 1, vec![paid], vec![])
                .await
            {
                Ok(_) => {}
                // Already recorded, e.g. by an earlier pass.
                Err(StoreError::Conflict(_)) => continue,
                Err(e) => return Err(e.into()),
            }
            tracing::info!(invoice = %invoice.id, %received, "payment request paid");
            let link = arrived
                .link
                .map(|url| format!("\n{url}"))
                .unwrap_or_default();
            let text = format!(
                "{} paid {received}{}{}.{link}",
                invoice.to.name,
                what_for(&invoice),
                arrived.by
            );
            let _turn = self.turn.lock().await;
            if let Err(e) = self.bot.send_message(self.chat, text).await {
                tracing::warn!(error = %e, "paid notice not delivered");
            }
        }
        Ok(())
    }

    /// What has reached the request so far; `None` while nothing has.
    async fn arrived(
        &self,
        invoice: &Invoice,
        rail_ref: RailRef,
    ) -> anyhow::Result<Option<Arrived>> {
        let asset = &invoice.total.asset;
        match rail_ref {
            RailRef::Sol { reference, .. } => {
                let received = self.rail.received(&reference, asset).await?;
                let Some(last) = received.last() else {
                    return Ok(None);
                };
                Ok(Some(Arrived {
                    minor: received.iter().map(|r| r.minor).sum(),
                    link: self.cluster.explorer(&last.signature),
                    by: "",
                    rail_ref: RailRef::Sol {
                        reference,
                        signature: Some(last.signature),
                    },
                }))
            }
            RailRef::Paypal {
                order_id,
                request_id,
            } => {
                let Some(paypal) = &self.paypal else {
                    return Ok(None);
                };
                let Some(collected) = paypal.collect(&order_id, asset).await? else {
                    return Ok(None);
                };
                Ok(Some(Arrived {
                    minor: collected.minor,
                    link: None,
                    by: " by PayPal",
                    rail_ref: RailRef::Paypal {
                        order_id,
                        request_id,
                    },
                }))
            }
            _ => Ok(None),
        }
    }
}

struct Arrived {
    minor: u128,
    rail_ref: RailRef,
    /// Where the owner can see the payment.
    link: Option<String>,
    /// How it was paid, for the notice.
    by: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_is_a_square_png() {
        let png = qr_png("solana:As4rsEAo282b3HR1ZN12Ec4sCfwQTGf6nM1mkQWBbb4u?amount=5").unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let decoder = png::Decoder::new(std::io::Cursor::new(png));
        let info = decoder.read_info().unwrap().info().clone();
        assert_eq!(info.width, info.height);
        assert_eq!(info.width as usize % SCALE, 0);
    }
}
