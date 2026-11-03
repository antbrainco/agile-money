//! Warden's core vocabulary: money, intents, policies, approvals, invoices and events.
//! Pure types and state machines only; every I/O boundary is a trait in [`ports`].

mod approval;
mod decision;
mod error;
mod event;
mod id;
mod intent;
mod invoice;
pub mod memory;
mod money;
mod policy;
pub mod ports;
mod rail;
mod recipient;
mod run;
mod untrusted;
mod value;

pub use approval::{Approval, ApprovalState, ApprovalTrigger, Channel};
pub use decision::Decision;
pub use error::Error;
pub use event::{Event, EventEnvelope, StreamId};
pub use id::{AgentId, ApprovalId, IntentId, InvoiceId, PolicyId, RunId, UserId};
pub use intent::{
    Approved, Intent, IntentHash, IntentState, MAX_SUBMIT_ATTEMPTS, Proposal, Proposed, Provenance,
};
pub use invoice::{Client, Invoice, InvoiceState, InvoiceTrigger, LineItem};
pub use money::{Amount, Asset, RailId};
pub use policy::{Policy, Rule, SpendSnapshot};
pub use rail::{PaymentRequest, RailRef, Settlement, Submission};
pub use recipient::{Address, Recipient};
pub use run::{RunState, RunTrigger, STEP_LIMIT};
pub use untrusted::Untrusted;
pub use value::{
    AssetCode, BchTxid, CashAddr, CashTokenCategory, ConcordiumAccount, ConcordiumTxHash,
    EmailAddress, IdempotencyKey, InvalidValue, NearAccount, NearTxHash, PaypalOrderId, SolAddress,
    SolSignature, TimeZoneName, TokenContract, ToolCallId,
};
