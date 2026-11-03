//! A provider-agnostic LLM client. Three seams, each a trait:
//!
//! - [`RoundTripper`] moves one HTTP request. Layers add headers, retries and tracing.
//! - [`Wire`] speaks one API format: it builds the request body and reads replies,
//!   whole or streamed. [`ChatCompletions`] covers OpenAI, DeepSeek and every
//!   OpenAI-compatible endpoint; [`AnthropicMessages`] covers Claude.
//! - [`Llm`](crate::Llm) is what the agent sees. [`LlmClient`] implements it over
//!   any round tripper and wire, so a gateway such as Cloudflare AI Gateway is only
//!   a different base URL and header.

mod anthropic;
mod client;
pub mod http;
mod openai;
mod settings;
pub mod sse;

use std::fmt;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value};

pub use anthropic::AnthropicMessages;
pub use client::LlmClient;
pub use http::{HttpError, HttpSettings, RoundTripper};
pub use openai::{ChatCompletions, Flavor};
pub use settings::{
    AnthropicSettings, Detection, GatewaySettings, LlmSettings, OpenAiSettings, Provider, Source,
};
pub use sse::SseEvent;

use crate::{Chunk, Error, Message, Reply, ToolCall, ToolSpec};

/// One model call, before a wire turns it into a body.
#[derive(Debug, Clone, Copy)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolSpec],
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Merged into the body last, for provider parameters this crate does not model.
    pub extra: &'a Map<String, Value>,
}

/// One API format.
pub trait Wire: Send + Sync + fmt::Debug {
    /// Tags `native` messages, so a message is only replayed to the wire that made it.
    fn name(&self) -> &'static str;
    /// Appended to the base URL, e.g. `chat/completions`.
    fn path(&self) -> &'static str;
    /// The header that carries the provider key.
    fn auth(&self, key: &SecretString) -> Result<(HeaderName, HeaderValue), Error>;
    /// Fixed headers the API wants on every call, e.g. `anthropic-version`.
    fn headers(&self) -> HeaderMap {
        HeaderMap::new()
    }
    fn body(&self, request: &ChatRequest<'_>, stream: bool) -> Result<Value, Error>;
    fn parse(&self, body: Value) -> Result<Reply, Error>;
    fn decoder(&self) -> Box<dyn StreamDecoder>;
}

/// Turns one streamed reply's events into chunks, then into the whole reply.
pub trait StreamDecoder: Send {
    fn event(&mut self, event: &SseEvent) -> Result<Vec<Chunk>, Error>;
    fn finish(self: Box<Self>) -> Result<Reply, Error>;
}

fn bearer(key: &SecretString) -> Result<(HeaderName, HeaderValue), Error> {
    secret_header(
        reqwest::header::AUTHORIZATION,
        &format!("Bearer {}", key.expose_secret()),
    )
}

fn secret_header(name: HeaderName, value: &str) -> Result<(HeaderName, HeaderValue), Error> {
    let mut value =
        HeaderValue::from_str(value).map_err(|_| Error::Invalid(format!("bad {name} value")))?;
    value.set_sensitive(true);
    Ok((name, value))
}

/// The provider's own message, tagged with the wire that wrote it.
fn native(wire: &str, message: Value) -> Value {
    serde_json::json!({ "wire": wire, "message": message })
}

/// `native`'s message, if `wire` wrote it.
fn native_for<'a>(wire: &str, native: Option<&'a Value>) -> Option<&'a Value> {
    let native = native?;
    (native.get("wire")?.as_str()? == wire)
        .then(|| native.get("message"))
        .flatten()
}

/// A tool schema as providers take it: schemars' `$schema` and `title` dropped.
fn tool_schema(schema: &Value) -> Value {
    let mut schema = schema.clone();
    if let Value::Object(object) = &mut schema {
        object.remove("$schema");
        object.remove("title");
    }
    schema
}

/// A tool call the model streamed or returned. Arguments that are not JSON are
/// passed on as a string, so the tool rejects them and the model sees why.
fn tool_call(id: &str, name: &str, args: &str) -> Result<ToolCall, Error> {
    let args = if args.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(args).unwrap_or_else(|_| Value::String(args.to_owned()))
    };
    Ok(ToolCall {
        id: id
            .parse()
            .map_err(|e: warden_domain::InvalidValue| Error::Llm(e.to_string()))?,
        name: name.to_owned(),
        args,
    })
}

fn merge(body: &mut Value, extra: &Map<String, Value>) {
    if let Value::Object(object) = body {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
}

fn malformed(what: &str) -> Error {
    Error::Llm(format!("malformed provider reply: {what}"))
}
