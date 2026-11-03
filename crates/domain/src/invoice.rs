//! Invoices the owner sends to clients, and their lifecycle.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{Amount, EmailAddress, Error, InvoiceId, RailId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineItem {
    pub description: String,
    pub quantity: u32,
    pub unit_price: Amount,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invoice {
    pub id: InvoiceId,
    pub to: Client,
    pub lines: Vec<LineItem>,
    pub total: Amount,
    pub rail: RailId,
    pub state: InvoiceState,
    pub due: Option<Timestamp>,
}

/// Who an invoice bills. They pay us, so there is no payout address here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Client {
    pub name: String,
    /// Where the invoice is sent, when it goes by email.
    pub email: Option<EmailAddress>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InvoiceState {
    Draft,
    Sent,
    PartiallyPaid,
    Paid,
    Overdue,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceTrigger {
    Issued,
    PaymentSeen { total_reached: bool },
    DuePassed,
    Cancelled,
}

impl InvoiceState {
    pub fn transition(self, trigger: InvoiceTrigger) -> Result<InvoiceState, Error> {
        use InvoiceState as S;
        use InvoiceTrigger as T;
        match (self, trigger) {
            (S::Draft, T::Issued) => Ok(S::Sent),
            (
                S::Sent,
                T::PaymentSeen {
                    total_reached: false,
                },
            ) => Ok(S::PartiallyPaid),
            (
                S::PartiallyPaid,
                T::PaymentSeen {
                    total_reached: false,
                },
            ) => Ok(S::PartiallyPaid),
            (
                S::Sent | S::PartiallyPaid | S::Overdue,
                T::PaymentSeen {
                    total_reached: true,
                },
            ) => Ok(S::Paid),
            (S::Sent | S::PartiallyPaid, T::DuePassed) => Ok(S::Overdue),
            (S::Draft | S::Sent | S::Overdue, T::Cancelled) => Ok(S::Cancelled),
            (from, trigger) => Err(Error::illegal("invoice", from, trigger)),
        }
    }
}
