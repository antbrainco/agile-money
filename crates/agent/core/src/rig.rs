//! `Llm` over Rig's provider catalog. One adapter covers every provider Rig
//! knows; the model reference picks it, e.g. `deepseek/deepseek-flash`.

use std::sync::Arc;

use async_trait::async_trait;
use rig_core::DynModel;
use rig_core::catalog::Catalog;
use rig_core::completion::message::{AssistantContent, CallId, ToolName};
use rig_core::completion::{CompletionRequest, Message as RigMessage, ToolDefinition};
use rig_core::operation::Completion;
use rig_core::providers::registry::ConnectOptions;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use warden_domain::InvalidValue;

use crate::{Error, Llm, Message, Reply, ToolCall, ToolSpec};

pub struct RigLlm {
    model: DynModel<Completion>,
}

impl std::fmt::Debug for RigLlm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RigLlm")
            .field("model", &self.model.name())
            .finish()
    }
}

impl RigLlm {
    /// Connects `reference` (`vendor/model`). Without `api_key`, Rig reads the
    /// vendor's usual variable, e.g. `DEEPSEEK_API_KEY`.
    pub fn connect(reference: &str, api_key: Option<&SecretString>) -> Result<Self, Error> {
        let mut options = ConnectOptions::new();
        if let Some(key) = api_key {
            options = options.api_key(key.expose_secret());
        }
        let model = Catalog::builtin()
            .connect_with(reference, options)
            .map_err(|e| Error::Llm(e.to_string()))?;
        Ok(Self { model })
    }
}

fn tool_name(name: &str) -> Result<ToolName, Error> {
    ToolName::new(name).map_err(|e| Error::Llm(e.to_string()))
}

fn to_rig(message: &Message) -> Result<RigMessage, Error> {
    Ok(match message {
        Message::System { text } => RigMessage::system(text),
        Message::User { text } => RigMessage::user(text.as_inner()),
        Message::Assistant { text } => RigMessage::assistant(text),
        Message::ToolCalls {
            native: Some(native),
            ..
        } => serde_json::from_value(native.clone())?,
        Message::ToolCalls {
            calls,
            native: None,
        } => calls
            .iter()
            .map(|c| {
                Ok(AssistantContent::tool_call(
                    c.id.as_str(),
                    tool_name(&c.name)?,
                    c.args.clone(),
                ))
            })
            .collect::<Result<Vec<_>, Error>>()?
            .into(),
        Message::ToolResult {
            call_id,
            name,
            content,
        } => RigMessage::tool_result(
            CallId::from_wire(call_id.as_str()),
            tool_name(name)?,
            content.to_string(),
        ),
    })
}

#[async_trait]
impl Llm for RigLlm {
    async fn complete(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<Reply, Error> {
        let history = messages
            .iter()
            .map(|m| to_rig(m).map(Arc::new))
            .collect::<Result<Vec<_>, _>>()?;
        let Some(last) = history.last() else {
            return Err(Error::Llm("empty conversation".into()));
        };
        let mut request = CompletionRequest::new((**last).clone());
        request.chat_history = history;
        for spec in tools {
            request.tools.push(Arc::new(ToolDefinition::new(
                tool_name(&spec.name)?,
                spec.description.clone(),
                spec.schema.clone(),
            )));
        }

        let response = self
            .model
            .call(request)
            .await
            .map_err(|e| Error::Llm(e.to_string()))?;
        let calls: Vec<ToolCall> = response
            .tool_calls()
            .map(|c| {
                Ok(ToolCall {
                    id: c
                        .id
                        .wire()
                        .parse()
                        .map_err(|e: InvalidValue| Error::Llm(e.to_string()))?,
                    name: c.function.name.as_str().to_owned(),
                    args: Value::Object(c.function.arguments.clone()),
                })
            })
            .collect::<Result<_, Error>>()?;
        if calls.is_empty() {
            return Ok(Reply::Final {
                text: response.text(),
            });
        }
        let native = response.message().map(serde_json::to_value).transpose()?;
        Ok(Reply::ToolCalls { calls, native })
    }
}
