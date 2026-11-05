//! The Chat Completions wire: OpenAI, DeepSeek, and anything OpenAI-compatible
//! (Cloudflare AI Gateway's `/compat`, OpenRouter, vLLM, Ollama).

use std::collections::BTreeMap;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use secrecy::SecretString;
use serde_json::{Map, Value, json};

use super::{
    ChatRequest, SseEvent, StreamDecoder, Wire, bearer, malformed, merge, native, native_for,
    tool_call, tool_schema,
};
use crate::{Chunk, Error, Message, Reply};

const WIRE: &str = "chat_completions";

/// Where the Chat Completions dialects differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// `max_completion_tokens`, `reasoning_effort`, usage in the stream.
    OpenAi,
    /// Thinking models stream `reasoning_content` and need it sent back with
    /// their tool calls, which replaying the native message does.
    DeepSeek,
    /// The baseline: `max_tokens`, nothing else assumed.
    Compatible,
}

#[derive(Debug, Clone)]
pub struct ChatCompletions {
    flavor: Flavor,
    reasoning_effort: Option<String>,
    headers: HeaderMap,
}

impl ChatCompletions {
    pub fn new(flavor: Flavor) -> Self {
        Self {
            flavor,
            reasoning_effort: None,
            headers: HeaderMap::new(),
        }
    }

    /// OpenAI reasoning models: `minimal`, `low`, `medium` or `high`.
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    /// E.g. `OpenAI-Organization` and `OpenAI-Project`.
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }
}

fn message(message: &Message) -> Value {
    match message {
        Message::System { text } => json!({ "role": "system", "content": text }),
        Message::User { text } => json!({ "role": "user", "content": text.as_inner() }),
        Message::Assistant { text } => json!({ "role": "assistant", "content": text }),
        Message::ToolCalls { calls, native } => match native_for(WIRE, native.as_ref()) {
            Some(message) => message.clone(),
            None => json!({
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": calls.iter().map(|c| json!({
                    "id": c.id.as_str(),
                    "type": "function",
                    "function": { "name": c.name, "arguments": c.args.to_string() },
                })).collect::<Vec<_>>(),
            }),
        },
        Message::ToolResult {
            call_id, content, ..
        } => json!({
            "role": "tool",
            "tool_call_id": call_id.as_str(),
            "content": content.to_string(),
        }),
    }
}

impl Wire for ChatCompletions {
    fn name(&self) -> &'static str {
        WIRE
    }

    fn path(&self) -> &'static str {
        "chat/completions"
    }

    fn auth(&self, key: &SecretString) -> Result<(HeaderName, HeaderValue), Error> {
        bearer(key)
    }

    fn headers(&self) -> HeaderMap {
        self.headers.clone()
    }

    fn body(&self, request: &ChatRequest<'_>, stream: bool) -> Result<Value, Error> {
        let mut body = json!({
            "model": request.model,
            "messages": request.messages.iter().map(message).collect::<Vec<_>>(),
            "stream": stream,
        });
        let object = body
            .as_object_mut()
            .ok_or_else(|| Error::Invalid("body".into()))?;
        if !request.tools.is_empty() {
            object.insert(
                "tools".into(),
                request
                    .tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": tool_schema(&t.schema),
                            },
                        })
                    })
                    .collect(),
            );
        }
        if let Some(max) = request.max_tokens {
            let field = match self.flavor {
                Flavor::OpenAi => "max_completion_tokens",
                Flavor::DeepSeek | Flavor::Compatible => "max_tokens",
            };
            object.insert(field.into(), max.into());
        }
        if let Some(t) = request.temperature {
            object.insert("temperature".into(), json!(t));
        }
        if let (Flavor::OpenAi, Some(effort)) = (self.flavor, &self.reasoning_effort) {
            object.insert("reasoning_effort".into(), effort.as_str().into());
        }
        if stream && self.flavor != Flavor::Compatible {
            object.insert("stream_options".into(), json!({ "include_usage": true }));
        }
        merge(&mut body, request.extra);
        Ok(body)
    }

    fn parse(&self, body: Value) -> Result<Reply, Error> {
        log_usage(&body);
        let message = body
            .pointer("/choices/0/message")
            .ok_or_else(|| malformed("no choices[0].message"))?;
        let calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|calls| {
                calls
                    .iter()
                    .map(|c| {
                        let text = |p: &str| c.pointer(p).and_then(Value::as_str).unwrap_or("");
                        tool_call(
                            text("/id"),
                            text("/function/name"),
                            text("/function/arguments"),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        if calls.is_empty() {
            let text = message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Ok(Reply::Final { text: text.into() });
        }
        Ok(Reply::ToolCalls {
            calls,
            native: Some(native(WIRE, message.clone())),
        })
    }

    fn decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(Decoder::default())
    }
}

fn log_usage(body: &Value) {
    if let Some(usage) = body.get("usage").filter(|u| !u.is_null()) {
        tracing::debug!(
            input = usage.get("prompt_tokens").and_then(|v| v.as_u64()),
            output = usage.get("completion_tokens").and_then(|v| v.as_u64()),
            "llm usage"
        );
    }
}

#[derive(Debug, Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
}

#[derive(Debug, Default)]
struct Decoder {
    text: String,
    reasoning: String,
    /// By the stream's `index`, which orders calls.
    calls: BTreeMap<u64, PartialCall>,
}

impl StreamDecoder for Decoder {
    fn event(&mut self, event: &SseEvent) -> Result<Vec<Chunk>, Error> {
        if event.data == "[DONE]" {
            return Ok(Vec::new());
        }
        let data: Value = serde_json::from_str(&event.data)?;
        if let Some(error) = data.get("error") {
            return Err(Error::Llm(format!("stream error: {error}")));
        }
        log_usage(&data);
        let Some(delta) = data.pointer("/choices/0/delta") else {
            return Ok(Vec::new());
        };
        let mut chunks = Vec::new();
        if let Some(text) = delta.get("content").and_then(Value::as_str)
            && !text.is_empty()
        {
            self.text.push_str(text);
            chunks.push(Chunk::Text(text.into()));
        }
        if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str)
            && !text.is_empty()
        {
            self.reasoning.push_str(text);
            chunks.push(Chunk::Reasoning(text.into()));
        }
        for call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
            let partial = self.calls.entry(index).or_default();
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                partial.id.push_str(id);
            }
            if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                if partial.name.is_empty() && !name.is_empty() {
                    chunks.push(Chunk::ToolCall { name: name.into() });
                }
                partial.name.push_str(name);
            }
            if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                partial.args.push_str(args);
            }
        }
        Ok(chunks)
    }

    fn finish(self: Box<Self>) -> Result<Reply, Error> {
        if self.calls.is_empty() {
            return Ok(Reply::Final { text: self.text });
        }
        let calls = self
            .calls
            .values()
            .map(|c| tool_call(&c.id, &c.name, &c.args))
            .collect::<Result<Vec<_>, _>>()?;
        let mut message = Map::new();
        message.insert("role".into(), "assistant".into());
        message.insert(
            "content".into(),
            if self.text.is_empty() {
                Value::Null
            } else {
                self.text.into()
            },
        );
        if !self.reasoning.is_empty() {
            message.insert("reasoning_content".into(), self.reasoning.into());
        }
        message.insert(
            "tool_calls".into(),
            self.calls
                .values()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "type": "function",
                        "function": { "name": c.name, "arguments": c.args },
                    })
                })
                .collect(),
        );
        Ok(Reply::ToolCalls {
            calls,
            native: Some(native(WIRE, Value::Object(message))),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolSpec;
    use agile_money_domain::Untrusted;

    fn sse(data: &str) -> SseEvent {
        SseEvent {
            event: None,
            data: data.into(),
        }
    }

    #[test]
    fn openai_body_uses_its_own_token_field_and_effort() {
        let wire = ChatCompletions::new(Flavor::OpenAi).with_reasoning_effort("low");
        let messages = [
            Message::System { text: "sys".into() },
            Message::User {
                text: Untrusted::new("hi".into()),
            },
        ];
        let tools = [ToolSpec {
            name: "list_contacts".into(),
            description: "d".into(),
            schema: json!({ "$schema": "x", "title": "T", "type": "object" }),
        }];
        let mut extra = Map::new();
        extra.insert("seed".into(), 7.into());
        let body = wire
            .body(
                &ChatRequest {
                    model: "gpt-5.5",
                    messages: &messages,
                    tools: &tools,
                    max_tokens: Some(100),
                    temperature: None,
                    extra: &extra,
                },
                true,
            )
            .unwrap();
        assert_eq!(body["max_completion_tokens"], 100);
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["seed"], 7);
        assert_eq!(
            body["tools"][0]["function"]["parameters"],
            json!({ "type": "object" })
        );
        assert_eq!(
            body["messages"][1],
            json!({ "role": "user", "content": "hi" })
        );
    }

    #[test]
    fn deepseek_stream_keeps_reasoning_for_the_replay() {
        let mut decoder: Box<dyn StreamDecoder> = Box::new(Decoder::default());
        for data in [
            r#"{"choices":[{"delta":{"reasoning_content":"think"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_0","function":{"name":"list_contacts","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"q\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}}]}"#,
            "[DONE]",
        ] {
            decoder.event(&sse(data)).unwrap();
        }
        let Reply::ToolCalls { calls, native } = decoder.finish().unwrap() else {
            panic!("expected tool calls");
        };
        assert_eq!(calls[0].name, "list_contacts");
        assert_eq!(calls[0].args, json!({ "q": 1 }));
        let replay = message(&Message::ToolCalls { calls, native });
        assert_eq!(replay["reasoning_content"], "think");
        assert_eq!(
            replay["tool_calls"][0]["function"]["arguments"],
            "{\"q\":1}"
        );
    }

    #[test]
    fn text_streams_as_chunks() {
        let mut decoder = Decoder::default();
        let chunks = decoder
            .event(&sse(r#"{"choices":[{"delta":{"content":"Hel"}}]}"#))
            .unwrap();
        assert_eq!(chunks, [Chunk::Text("Hel".into())]);
        decoder
            .event(&sse(r#"{"choices":[{"delta":{"content":"lo"}}]}"#))
            .unwrap();
        assert_eq!(
            Box::new(decoder).finish().unwrap(),
            Reply::Final {
                text: "Hello".into()
            }
        );
    }

    #[test]
    fn native_from_another_wire_is_rebuilt() {
        let call = tool_call("toolu_1", "list_contacts", "{}").unwrap();
        let replay = message(&Message::ToolCalls {
            calls: vec![call],
            native: Some(native("anthropic_messages", json!([]))),
        });
        assert_eq!(replay["tool_calls"][0]["id"], "toolu_1");
    }
}
