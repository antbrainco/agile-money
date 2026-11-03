//! `Llm` over any round tripper and wire.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderValue};
use reqwest::{Method, Request, Response};
use serde_json::{Map, Value};
use url::Url;

use super::http::RoundTripper;
use super::sse::SseParser;
use super::{ChatRequest, StreamDecoder, Wire};
use crate::{Chunk, Error, Llm, Message, Reply, ReplyStream, ToolSpec};

/// How much of an error body is kept.
const ERROR_BODY_LIMIT: usize = 1_000;

#[derive(Debug, Clone)]
pub struct LlmClient {
    transport: Arc<dyn RoundTripper>,
    wire: Arc<dyn Wire>,
    endpoint: Url,
    model: String,
    streaming: bool,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    extra_body: Map<String, Value>,
    request_timeout: Option<Duration>,
}

impl LlmClient {
    /// `base_url` should end in `/`; the wire's path is joined under it.
    pub fn new(
        transport: Arc<dyn RoundTripper>,
        wire: Arc<dyn Wire>,
        base_url: Url,
        model: impl Into<String>,
    ) -> Self {
        let endpoint = base_url.join(wire.path()).unwrap_or(base_url);
        Self {
            transport,
            wire,
            endpoint,
            model: model.into(),
            streaming: true,
            max_tokens: None,
            temperature: None,
            extra_body: Map::new(),
            request_timeout: None,
        }
    }

    pub fn with_streaming(mut self, on: bool) -> Self {
        self.streaming = on;
        self
    }

    pub fn with_max_tokens(mut self, max: Option<u32>) -> Self {
        self.max_tokens = max;
        self
    }

    pub fn with_temperature(mut self, temperature: Option<f32>) -> Self {
        self.temperature = temperature;
        self
    }

    pub fn with_extra_body(mut self, extra: Map<String, Value>) -> Self {
        self.extra_body = extra;
        self
    }

    /// Caps non-streaming calls only; streams are bounded by the read timeout.
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn request(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
        stream: bool,
    ) -> Result<Request, Error> {
        let body = self.wire.body(
            &ChatRequest {
                model: &self.model,
                messages,
                tools,
                max_tokens: self.max_tokens,
                temperature: self.temperature,
                extra: &self.extra_body,
            },
            stream,
        )?;
        let mut request = Request::new(Method::POST, self.endpoint.clone());
        let headers = request.headers_mut();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(if stream {
                "text/event-stream"
            } else {
                "application/json"
            }),
        );
        *request.body_mut() = Some(serde_json::to_vec(&body)?.into());
        if !stream {
            *request.timeout_mut() = self.request_timeout;
        }
        Ok(request)
    }

    async fn send(&self, request: Request) -> Result<Response, Error> {
        let response = self.transport.round_trip(request).await?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        Err(Error::Api {
            status: status.as_u16(),
            message: api_message(&body),
        })
    }
}

/// The provider's own error message if it sent one; both wires use `error.message`.
fn api_message(body: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.pointer("/error/message")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| body.to_owned());
    message.chars().take(ERROR_BODY_LIMIT).collect()
}

struct Streaming {
    response: Option<Response>,
    sse: SseParser,
    decoder: Option<Box<dyn StreamDecoder>>,
    queue: VecDeque<Chunk>,
}

impl Streaming {
    fn decode(&mut self, events: Vec<super::SseEvent>) -> Result<(), Error> {
        let Some(decoder) = self.decoder.as_mut() else {
            return Ok(());
        };
        for event in &events {
            self.queue.extend(decoder.event(event)?);
        }
        Ok(())
    }

    async fn next(mut self) -> Result<Option<(Chunk, Self)>, Error> {
        loop {
            if let Some(chunk) = self.queue.pop_front() {
                return Ok(Some((chunk, self)));
            }
            let Some(response) = self.response.as_mut() else {
                return Ok(None);
            };
            match response
                .chunk()
                .await
                .map_err(super::http::HttpError::from)?
            {
                Some(bytes) => {
                    let events = self.sse.feed(&bytes);
                    self.decode(events)?;
                }
                None => {
                    self.response = None;
                    let events = self.sse.finish();
                    self.decode(events)?;
                    if let Some(decoder) = self.decoder.take() {
                        self.queue.push_back(Chunk::Done(decoder.finish()?));
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Llm for LlmClient {
    #[tracing::instrument(skip_all, fields(wire = self.wire.name(), model = %self.model))]
    async fn complete(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<Reply, Error> {
        let response = self.send(self.request(messages, tools, false)?).await?;
        let body: Value = response
            .json()
            .await
            .map_err(super::http::HttpError::from)?;
        self.wire.parse(body)
    }

    #[tracing::instrument(skip_all, fields(wire = self.wire.name(), model = %self.model))]
    async fn stream(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<ReplyStream, Error> {
        if !self.streaming {
            return Ok(crate::whole(self.complete(messages, tools).await?));
        }
        let response = self.send(self.request(messages, tools, true)?).await?;
        let state = Streaming {
            response: Some(response),
            sse: SseParser::default(),
            decoder: Some(self.wire.decoder()),
            queue: VecDeque::new(),
        };
        Ok(Box::pin(stream::try_unfold(state, Streaming::next)))
    }
}
