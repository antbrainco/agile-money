//! The agent loop: model replies drive a `RunState`, and tools can read or propose, never pay.
//! Providers (DeepSeek, OpenAI, Claude) plug in through the `Llm` port via Rig adapters.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use warden_domain::{Intent, Invoice, Proposed, Untrusted};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("model call failed: {0}")]
    Llm(String),
    #[error("unknown tool {0}")]
    UnknownTool(String),
    #[error("bad tool arguments: {0}")]
    Args(#[from] serde_json::Error),
    #[error(transparent)]
    Domain(#[from] warden_domain::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Deepseek,
    Openai,
    Claude,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    System { text: String },
    User { text: Untrusted<String> },
    Assistant { text: String },
    ToolResult { call_id: String, content: Value },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    ToolCalls { calls: Vec<ToolCall> },
    Final { text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// Model port. Prompts and tool schemas are provider-neutral.
#[async_trait]
pub trait Llm: Send + Sync {
    async fn complete(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<Reply, Error>;
}

/// What a tool hands back. There is deliberately no variant that pays.
#[derive(Debug)]
pub enum ToolOutput {
    Data(Value),
    ProposedPayment(Intent<Proposed>),
    DraftInvoice(Invoice),
}

/// A typed agent tool; its argument schema comes from `schemars`.
#[async_trait]
pub trait Tool: Send + Sync {
    type Args: JsonSchema + for<'de> Deserialize<'de> + Send;

    const NAME: &'static str;
    const DESCRIPTION: &'static str;

    async fn call(&self, args: Self::Args) -> Result<ToolOutput, Error>;

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
