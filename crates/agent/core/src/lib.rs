//! The agent loop: model replies drive a `RunState`, and tools can read or propose, never pay.
//! Providers (DeepSeek, OpenAI, Claude) plug in through the `Llm` port via Rig adapters.

mod agent;
mod rig;
pub mod tools;

use async_trait::async_trait;
use jiff::Timestamp;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use warden_domain::{AgentId, Intent, Invoice, Proposed, RunId, ToolCallId, Untrusted};

pub use agent::{Agent, RunReport, SYSTEM_PROMPT};
pub use rig::RigLlm;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("model call failed: {0}")]
    Llm(String),
    #[error("unknown tool {0}")]
    UnknownTool(String),
    #[error("bad tool arguments: {0}")]
    Args(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Domain(#[from] warden_domain::Error),
    #[error(transparent)]
    Policy(#[from] warden_policy::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Deepseek,
    Openai,
    Claude,
}

impl Provider {
    /// Rig catalog reference for this provider's default model.
    pub fn default_model(self) -> &'static str {
        match self {
            Self::Deepseek => "deepseek/deepseek-flash",
            Self::Openai => "openai/gpt-5.5",
            Self::Claude => "anthropic/claude-sonnet-5-5",
        }
    }
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
