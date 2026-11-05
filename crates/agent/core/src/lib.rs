//! The agent loop: model replies drive a `RunState`, and tools can read or propose, never pay.
//! Providers (DeepSeek, OpenAI, Claude, anything OpenAI-compatible) plug in
//! through the `Llm` port; [`llm::LlmClient`] implements it over HTTP.

mod agent;
pub mod llm;
mod prompt;
pub mod tools;

use std::pin::Pin;

use agile_money_domain::{AgentId, Intent, Invoice, Proposed, RunId, ToolCallId, Untrusted};
use async_trait::async_trait;
use futures_util::Stream;
use jiff::Timestamp;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use agent::{Agent, NoObserver, Paused, RunObserver, RunReport};
pub use llm::{Detection, LlmClient, LlmSettings, Provider, Source};
pub use prompt::{Briefing, SYSTEM_PROMPT, system_prompt};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("model call failed: {0}")]
    Llm(String),
    #[error("model API returned {status}: {message}")]
    Api { status: u16, message: String },
    #[error(transparent)]
    Http(#[from] llm::HttpError),
    #[error("unknown tool {0}")]
    UnknownTool(String),
    #[error("bad tool arguments: {0}")]
    Args(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Domain(#[from] agile_money_domain::Error),
    #[error(transparent)]
    Policy(#[from] agile_money_policy::Error),
    #[error("could not issue the request: {0}")]
    Billing(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    System {
        text: String,
    },
    User {
        text: Untrusted<String>,
    },
    Assistant {
        text: String,
    },
    /// The model asked for tools. `native` keeps the provider's own message
    /// (reasoning included) so it can be sent back unchanged.
    ToolCalls {
        calls: Vec<ToolCall>,
        native: Option<Value>,
    },
    ToolResult {
        call_id: ToolCallId,
        name: String,
        content: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    ToolCalls {
        calls: Vec<ToolCall>,
        native: Option<Value>,
    },
    Final {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// One piece of a streamed reply. The stream ends with exactly one `Done`.
#[derive(Debug, Clone, PartialEq)]
pub enum Chunk {
    /// Answer text as it is written.
    Text(String),
    /// Thinking text, from models that stream it. Never shown as the answer.
    Reasoning(String),
    /// The model started a tool call; its arguments are still arriving.
    ToolCall {
        name: String,
    },
    Done(Reply),
}

pub type ReplyStream = Pin<Box<dyn Stream<Item = Result<Chunk, Error>> + Send>>;

/// A finished reply as a stream: its text, if final, then `Done`.
pub fn whole(reply: Reply) -> ReplyStream {
    let mut chunks = Vec::new();
    if let Reply::Final { text } = &reply {
        chunks.push(Ok(Chunk::Text(text.clone())));
    }
    chunks.push(Ok(Chunk::Done(reply)));
    Box::pin(futures_util::stream::iter(chunks))
}

/// Model port. Prompts and tool schemas are provider-neutral.
#[async_trait]
pub trait Llm: Send + Sync {
    async fn complete(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<Reply, Error>;

    /// The reply as it is generated. By default, the whole reply at once.
    async fn stream(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<ReplyStream, Error> {
        Ok(whole(self.complete(messages, tools).await?))
    }
}

/// What a tool hands back. There is deliberately no variant that pays.
#[derive(Debug)]
pub enum ToolOutput {
    Data(Value),
    ProposedPayment(Intent<Proposed>),
    DraftInvoice(Invoice),
}

/// Where a tool call comes from, for provenance and idempotency keys.
#[derive(Debug, Clone)]
pub struct ToolCtx {
    pub agent: AgentId,
    pub run: RunId,
    pub call_id: ToolCallId,
    pub now: Timestamp,
}

/// A typed agent tool; its argument schema comes from `schemars`.
#[async_trait]
pub trait Tool: Send + Sync {
    type Args: JsonSchema + DeserializeOwned + Send;

    const NAME: &'static str;
    const DESCRIPTION: &'static str;

    async fn call(&self, ctx: &ToolCtx, args: Self::Args) -> Result<ToolOutput, Error>;

    fn spec() -> ToolSpec
    where
        Self: Sized,
    {
        ToolSpec {
            name: Self::NAME.into(),
            description: Self::DESCRIPTION.into(),
            schema: serde_json::to_value(schemars::schema_for!(Self::Args)).unwrap_or(Value::Null),
        }
    }
}

/// Object-safe view of a `Tool`, so the loop can hold different tools in one list.
#[async_trait]
pub trait DynTool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn call_json(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, Error>;
}

#[async_trait]
impl<T: Tool> DynTool for T {
    fn spec(&self) -> ToolSpec {
        T::spec()
    }

    async fn call_json(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, Error> {
        self.call(ctx, serde_json::from_value(args)?).await
    }
}
