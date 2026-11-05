//! HTTP round trippers, after Go's `http.RoundTripper`: one request in, one
//! response out, any status. Layers wrap another round tripper to add default
//! headers, retries or tracing, and stack in any order.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{Request, Response, StatusCode};
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// Connect, TLS, timeout or body errors: the server may never have seen the request.
    #[error("http transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("http setup: {0}")]
    Setup(String),
}

impl HttpError {
    fn retryable(&self) -> bool {
        match self {
            Self::Transport(e) => e.is_connect() || e.is_timeout() || e.is_request(),
            Self::Setup(_) => false,
        }
    }
}

/// One HTTP exchange. A non-2xx status is a response, not an error: callers decide.
#[async_trait]
pub trait RoundTripper: Send + Sync + fmt::Debug {
    async fn round_trip(&self, request: Request) -> Result<Response, HttpError>;
}

#[async_trait]
impl<T: RoundTripper + ?Sized> RoundTripper for Arc<T> {
    async fn round_trip(&self, request: Request) -> Result<Response, HttpError> {
        (**self).round_trip(request).await
    }
}

/// `[agent.http]`: timeouts and retries for model calls.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HttpSettings {
    pub connect_timeout_ms: u64,
    /// Longest gap between two reads. Bounds a stalled stream without capping a long one.
    pub read_timeout_ms: u64,
    /// Whole-request cap for non-streaming calls.
    pub request_timeout_ms: u64,
    /// Retries after the first attempt, on 408, 409, 429, 5xx and connect errors.
    pub max_retries: u32,
    pub retry_base_ms: u64,
    pub retry_max_ms: u64,
}

impl Default for HttpSettings {
    fn default() -> Self {
        Self {
            connect_timeout_ms: 10_000,
            read_timeout_ms: 60_000,
            request_timeout_ms: 120_000,
            max_retries: 2,
            retry_base_ms: 500,
            retry_max_ms: 8_000,
        }
    }
}

/// The bottom of the stack: a pooled `reqwest::Client`.
#[derive(Debug, Clone)]
pub struct Reqwest(reqwest::Client);

impl Reqwest {
    pub fn new(settings: &HttpSettings) -> Result<Self, HttpError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("agile-money/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_millis(settings.connect_timeout_ms))
            .read_timeout(Duration::from_millis(settings.read_timeout_ms))
            .build()?;
        Ok(Self(client))
    }

    pub fn from_client(client: reqwest::Client) -> Self {
        Self(client)
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.0
    }
}

#[async_trait]
impl RoundTripper for Reqwest {
    async fn round_trip(&self, request: Request) -> Result<Response, HttpError> {
        Ok(self.0.execute(request).await?)
    }
}

/// Adds headers the request does not already set: auth, API versions, gateway tokens.
/// Mark secret values with `HeaderValue::set_sensitive` so `Debug` hides them.
#[derive(Debug, Clone)]
pub struct Headers<T> {
    inner: T,
    headers: HeaderMap,
}

impl<T> Headers<T> {
    pub fn new(inner: T, headers: HeaderMap) -> Self {
        Self { inner, headers }
    }
}

#[async_trait]
impl<T: RoundTripper> RoundTripper for Headers<T> {
    async fn round_trip(&self, mut request: Request) -> Result<Response, HttpError> {
        let target = request.headers_mut();
        for (name, value) in &self.headers {
            if !target.contains_key(name) {
                target.insert(name.clone(), value.clone());
            }
        }
        self.inner.round_trip(request).await
    }
}

/// Retries transient failures with capped exponential backoff and full jitter,
/// honouring `Retry-After`. A request whose body cannot be cloned is sent once.
#[derive(Debug, Clone)]
pub struct Retry<T> {
    inner: T,
    max_retries: u32,
    base: Duration,
    max: Duration,
}

impl<T> Retry<T> {
    pub fn new(inner: T, settings: &HttpSettings) -> Self {
        Self {
            inner,
            max_retries: settings.max_retries,
            base: Duration::from_millis(settings.retry_base_ms),
            max: Duration::from_millis(settings.retry_max_ms),
        }
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let ceiling = self
            .base
            .saturating_mul(2u32.saturating_pow(attempt))
            .min(self.max);
        let millis = u64::try_from(ceiling.as_millis()).unwrap_or(u64::MAX);
        Duration::from_millis(jitter() % millis.saturating_add(1))
    }
}

/// Statuses worth another try. 529 is Anthropic's "overloaded".
pub fn retryable_status(status: StatusCode) -> bool {
    matches!(
        status.as_u16(),
        408 | 409 | 429 | 500 | 502 | 503 | 504 | 529
    )
}

fn retry_after(response: &Response) -> Option<Duration> {
    let secs: u64 = response
        .headers()
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs))
}

fn jitter() -> u64 {
    RandomState::new().build_hasher().finish()
}

#[async_trait]
impl<T: RoundTripper> RoundTripper for Retry<T> {
    async fn round_trip(&self, request: Request) -> Result<Response, HttpError> {
        let mut attempt = 0;
        let mut next = request;
        loop {
            let spare = (attempt < self.max_retries)
                .then(|| next.try_clone())
                .flatten();
            let result = self.inner.round_trip(next).await;
            let Some(retry) = spare else {
                return result;
            };
            let delay = match &result {
                Ok(response) if retryable_status(response.status()) => {
                    retry_after(response).map_or_else(|| self.backoff(attempt), |d| d.min(self.max))
                }
                Err(e) if e.retryable() => self.backoff(attempt),
                _ => return result,
            };
            tracing::warn!(
                attempt = attempt + 1,
                delay_ms = delay.as_millis() as u64,
                status = result.as_ref().ok().map(|r| r.status().as_u16()),
                error = result.as_ref().err().map(tracing::field::display),
                "retrying model call"
            );
            tokio::time::sleep(delay).await;
            attempt += 1;
            next = retry;
        }
    }
}

/// Logs method, host, path, status and latency of each exchange. Never headers,
/// query strings or bodies, which hold keys and prompts.
#[derive(Debug, Clone)]
pub struct Trace<T> {
    inner: T,
}

impl<T> Trace<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl<T: RoundTripper> RoundTripper for Trace<T> {
    async fn round_trip(&self, request: Request) -> Result<Response, HttpError> {
        let method = request.method().clone();
        let host = request.url().host_str().unwrap_or_default().to_owned();
        let path = request.url().path().to_owned();
        let started = Instant::now();
        let result = self.inner.round_trip(request).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match &result {
            Ok(response) => tracing::debug!(
                %method, %host, %path, status = response.status().as_u16(), elapsed_ms,
                "llm http"
            ),
            Err(e) => tracing::warn!(%method, %host, %path, error = %e, elapsed_ms, "llm http"),
        }
        result
    }
}

/// The default stack: `Trace(Retry(Headers(Reqwest)))`. Tracing sits outside
/// retries, so one log line covers the whole exchange.
pub fn stack(
    settings: &HttpSettings,
    headers: HeaderMap,
) -> Result<Arc<dyn RoundTripper>, HttpError> {
    let base = Reqwest::new(settings)?;
    Ok(Arc::new(Trace::new(Retry::new(
        Headers::new(base, headers),
        settings,
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_stays_under_the_cap() {
        let retry = Retry::new(
            (),
            &HttpSettings {
                retry_base_ms: 100,
                retry_max_ms: 1_000,
                ..HttpSettings::default()
            },
        );
        for attempt in 0..40 {
            assert!(retry.backoff(attempt) <= Duration::from_millis(1_000));
        }
    }

    #[test]
    fn only_transient_statuses_retry() {
        assert!(retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(retryable_status(StatusCode::from_u16(529).unwrap()));
        assert!(!retryable_status(StatusCode::BAD_REQUEST));
        assert!(!retryable_status(StatusCode::UNAUTHORIZED));
    }
}
