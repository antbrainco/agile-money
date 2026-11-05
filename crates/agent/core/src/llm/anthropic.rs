//! Anthropic's Messages wire, for Claude.

use std::collections::BTreeMap;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};

use super::{
    ChatRequest, SseEvent, StreamDecoder, Wire, malformed, merge, native, native_for,
    secret_header, tool_call, tool_schema,
};
use crate::{Chunk, Error, Message, Reply, ToolCall};

const WIRE: &str = "anthropic_messages";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Messages requires `max_tokens`; this is used when none is set.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

#[derive(Debug, Clone)]
pub struct AnthropicMessages {
    headers: HeaderMap,
    thinking_budget: Option<u32>,
}

impl AnthropicMessages {
    pub fn new(version: &str, betas: &[String]) -> Result<Self, Error> {
        let value = |v: &str| {
            HeaderValue::from_str(v).map_err(|_| Error::Invalid(format!("bad header value {v}")))
        };
        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", value(version)?);
        if !betas.is_empty() {
            headers.insert("anthropic-beta", value(&betas.join(","))?);
        }
        Ok(Self {
            headers,
            thinking_budget: None,
        })
    }

    /// Extended thinking with this many tokens. Must be below `max_tokens`.
    pub fn with_thinking(mut self, budget: u32) -> Self {
        self.thinking_budget = Some(budget);
        self
    }
}

/// Claude takes alternating turns, system text apart, and tool results as user
/// content blocks; consecutive results go in one user message.
fn turns(messages: &[Message]) -> (Vec<&str>, Vec<Value>) {
    let mut system = Vec::new();
    let mut turns: Vec<Value> = Vec::new();
    for message in messages {
        let turn = match message {
            Message::System { text } => {
                system.push(text.as_str());
                continue;
            }
            Message::User { text } => json!({ "role": "user", "content": text.as_inner() }),
            Message::Assistant { text } => json!({ "role": "assistant", "content": text }),
            Message::ToolCalls { calls, native } => json!({
                "role": "assistant",
                "content": native_for(WIRE, native.as_ref())
                    .cloned()
                    .unwrap_or_else(|| tool_uses(calls)),
            }),
            Message::ToolResult {
                call_id, content, ..
            } => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": call_id.as_str(),
                    "content": content.to_string(),
                });
                if let Some(blocks) = turns
                    .last_mut()
                    .filter(|t| t["role"] == "user")
                    .and_then(|t| t.get_mut("content"))
                    .and_then(Value::as_array_mut)
                {
                    blocks.push(block);
                    continue;
                }
                json!({ "role": "user", "content": [block] })
            }
        };
        turns.push(turn);
    }
    (system, turns)
}

fn tool_uses(calls: &[ToolCall]) -> Value {
    calls
        .iter()
        .map(
            |c| json!({ "type": "tool_use", "id": c.id.as_str(), "name": c.name, "input": c.args }),
        )
        .collect()
}

/// A reply from its content blocks: tool calls if any, else the text.
fn reply(content: Vec<Value>) -> Result<Reply, Error> {
    let calls = content
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| {
            let mut call = tool_call(
                b["id"].as_str().unwrap_or_default(),
                b["name"].as_str().unwrap_or_default(),
                "",
            )?;
            call.args = b.get("input").cloned().unwrap_or(json!({}));
            Ok(call)
        })
        .collect::<Result<Vec<_>, Error>>()?;
    if calls.is_empty() {
        let text = content
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<String>();
        return Ok(Reply::Final { text });
    }
    // Thinking blocks, signatures included, must go back unchanged with the results.
    Ok(Reply::ToolCalls {
        calls,
        native: Some(native(WIRE, Value::Array(content))),
    })
}

impl Wire for AnthropicMessages {
    fn name(&self) -> &'static str {
        WIRE
    }

    fn path(&self) -> &'static str {
        "v1/messages"
    }

    fn auth(&self, key: &SecretString) -> Result<(HeaderName, HeaderValue), Error> {
        secret_header(HeaderName::from_static("x-api-key"), key.expose_secret())
    }

    fn headers(&self) -> HeaderMap {
        self.headers.clone()
    }

    fn body(&self, request: &ChatRequest<'_>, stream: bool) -> Result<Value, Error> {
        let (system, messages) = turns(request.messages);
        let max_tokens = request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
        let mut body = json!({
            "model": request.model,
            "max_tokens": max_tokens,
            "messages": messages,
            "stream": stream,
        });
        let object = body
            .as_object_mut()
            .ok_or_else(|| Error::Invalid("body".into()))?;
        if !system.is_empty() {
            object.insert("system".into(), system.join("\n\n").into());
        }
        if !request.tools.is_empty() {
            object.insert(
                "tools".into(),
                request
                    .tools
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "input_schema": tool_schema(&t.schema),
                        })
                    })
                    .collect(),
            );
        }
        if let Some(budget) = self.thinking_budget {
            if budget >= max_tokens {
                return Err(Error::Invalid(format!(
                    "thinking budget {budget} must be below max_tokens {max_tokens}"
                )));
            }
            // Thinking takes no temperature.
            object.insert(
                "thinking".into(),
                json!({ "type": "enabled", "budget_tokens": budget }),
            );
        } else if let Some(t) = request.temperature {
            object.insert("temperature".into(), json!(t));
        }
        merge(&mut body, request.extra);
        Ok(body)
    }

    fn parse(&self, body: Value) -> Result<Reply, Error> {
        log_usage(&body["usage"]);
        let Some(Value::Array(content)) = body.get("content").cloned() else {
            return Err(malformed("no content"));
        };
        reply(content)
    }

    fn decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(Decoder::default())
    }
}

fn log_usage(usage: &Value) {
    if usage.is_object() {
        tracing::debug!(
            input = usage.get("input_tokens").and_then(|v| v.as_u64()),
            output = usage.get("output_tokens").and_then(|v| v.as_u64()),
            "llm usage"
        );
    }
}

#[derive(Debug, Default)]
struct Decoder {
    blocks: BTreeMap<u64, Value>,
    /// `input_json_delta` pieces of each tool_use block, parsed when it stops.
    json: BTreeMap<u64, String>,
}

impl Decoder {
    fn append(&mut self, index: u64, field: &str, text: &str) {
        if let Some(Value::Object(block)) = self.blocks.get_mut(&index) {
            let current = block.get(field).and_then(Value::as_str).unwrap_or_default();
            block.insert(field.into(), format!("{current}{text}").into());
        }
    }
}

impl StreamDecoder for Decoder {
    fn event(&mut self, event: &SseEvent) -> Result<Vec<Chunk>, Error> {
        let data: Value = serde_json::from_str(&event.data)?;
        let index = data.get("index").and_then(Value::as_u64).unwrap_or(0);
        let mut chunks = Vec::new();
        match data["type"].as_str().unwrap_or_default() {
            "content_block_start" => {
                let block = data["content_block"].clone();
                if block["type"] == "tool_use" {
                    chunks.push(Chunk::ToolCall {
                        name: block["name"].as_str().unwrap_or_default().into(),
                    });
                }
                self.blocks.insert(index, block);
            }
            "content_block_delta" => {
                let delta = &data["delta"];
                let text = |field: &str| delta[field].as_str().unwrap_or_default().to_owned();
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let text = text("text");
                        self.append(index, "text", &text);
                        chunks.push(Chunk::Text(text));
                    }
                    "thinking_delta" => {
                        let text = text("thinking");
                        self.append(index, "thinking", &text);
                        chunks.push(Chunk::Reasoning(text));
                    }
                    "signature_delta" => self.append(index, "signature", &text("signature")),
                    "input_json_delta" => self
                        .json
                        .entry(index)
                        .or_default()
                        .push_str(&text("partial_json")),
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let (Some(json), Some(block)) =
                    (self.json.remove(&index), self.blocks.get_mut(&index))
                {
                    block["input"] = if json.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&json).unwrap_or(Value::String(json))
                    };
                }
            }
            "message_start" => log_usage(&data["message"]["usage"]),
            "message_delta" => log_usage(&data["usage"]),
            "error" => return Err(Error::Llm(format!("stream error: {}", data["error"]))),
            _ => {}
        }
        Ok(chunks)
    }

    fn finish(self: Box<Self>) -> Result<Reply, Error> {
        reply(self.blocks.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agile_money_domain::Untrusted;
    use serde_json::Map;

    fn event(data: Value) -> SseEvent {
        SseEvent {
            event: data["type"].as_str().map(str::to_owned),
            data: data.to_string(),
        }
    }

    #[test]
    fn body_lifts_system_and_groups_tool_results() {
        let call = |id: &str| tool_call(id, "t", "{}").unwrap();
        let messages = [
            Message::System { text: "sys".into() },
            Message::User {
                text: Untrusted::new("hi".into()),
            },
            Message::ToolCalls {
                calls: vec![call("toolu_1"), call("toolu_2")],
                native: None,
            },
            Message::ToolResult {
                call_id: "toolu_1".parse().unwrap(),
                name: "t".into(),
                content: json!(1),
            },
            Message::ToolResult {
                call_id: "toolu_2".parse().unwrap(),
                name: "t".into(),
                content: json!(2),
            },
        ];
        let wire = AnthropicMessages::new(ANTHROPIC_VERSION, &[])
            .unwrap()
            .with_thinking(1024);
        let body = wire
            .body(
                &ChatRequest {
                    model: "claude-sonnet-5-5",
                    messages: &messages,
                    tools: &[],
                    max_tokens: None,
                    temperature: Some(0.2),
                    extra: &Map::new(),
                },
                false,
            )
            .unwrap();
        assert_eq!(body["system"], "sys");
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(body["thinking"]["budget_tokens"], 1024);
        assert!(body.get("temperature").is_none());
        let turns = body["messages"].as_array().unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[1]["content"][1]["id"], "toolu_2");
        assert_eq!(turns[2]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn stream_rebuilds_thinking_and_tool_input() {
        let mut decoder: Box<dyn StreamDecoder> = Box::new(Decoder::default());
        let events = [
            json!({"type":"message_start","message":{"usage":{"input_tokens":5}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"list_contacts","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"2}"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}),
            json!({"type":"message_stop"}),
        ];
        let mut chunks = Vec::new();
        for e in events {
            chunks.extend(decoder.event(&event(e)).unwrap());
        }
        assert_eq!(
            chunks,
            [
                Chunk::Reasoning("hm".into()),
                Chunk::ToolCall {
                    name: "list_contacts".into()
                }
            ]
        );
        let Reply::ToolCalls { calls, native } = decoder.finish().unwrap() else {
            panic!("expected tool calls");
        };
        assert_eq!(calls[0].args, json!({ "q": 2 }));
        let blocks = &native.unwrap()["message"];
        assert_eq!(blocks[0]["signature"], "sig");
        assert_eq!(blocks[1]["input"], json!({ "q": 2 }));
    }

    #[test]
    fn stream_error_event_fails() {
        let mut decoder = Decoder::default();
        let err = decoder
            .event(&event(
                json!({"type":"error","error":{"type":"overloaded_error"}}),
            ))
            .unwrap_err();
        assert!(err.to_string().contains("overloaded"));
    }
}
