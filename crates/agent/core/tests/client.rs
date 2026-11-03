//! `LlmClient` over the real round-tripper stack, against a mock HTTP server.

#![allow(clippy::unwrap_used)] // helpers outside #[test] fns, which clippy.toml does not cover

use futures_util::StreamExt;
use secrecy::SecretString;
use serde_json::json;
use warden_agent_core::{Chunk, Error, Llm, LlmSettings, Message, Provider, Reply};
use warden_domain::Untrusted;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn settings(provider: Provider, server: &MockServer, prefix: &str) -> LlmSettings {
    let mut settings = LlmSettings::new(provider);
    settings.base_url = Some(format!("{}{prefix}", server.uri()).parse().unwrap());
    settings.api_key = Some(SecretString::from("sk-test".to_owned()));
    settings.http.retry_base_ms = 1;
    settings
}

fn hello() -> Vec<Message> {
    vec![Message::User {
        text: Untrusted::new("hi".into()),
    }]
}

#[tokio::test]
async fn deepseek_streams_text_through_a_gateway_path() {
    let server = MockServer::start().await;
    let sse = [
        r#"{"choices":[{"delta":{"content":"Hel"}}]}"#,
        r#"{"choices":[{"delta":{"content":"lo"}}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#,
        "[DONE]",
    ]
    .map(|d| format!("data: {d}\n\n"))
    .concat();
    Mock::given(method("POST"))
        .and(path("/v1/acct/gw/deepseek/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .and(header("cf-aig-collect-log-payload", "false"))
        .and(header("accept", "text/event-stream"))
        .and(body_partial_json(json!({
            "model": "deepseek-flash",
            "stream": true,
            "stream_options": { "include_usage": true },
        })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let mut settings = settings(Provider::Deepseek, &server, "/v1/acct/gw/deepseek");
    settings
        .headers
        .insert("cf-aig-collect-log-payload".into(), "false".into());
    let client = settings.client().unwrap();
    let chunks: Vec<Chunk> = client
        .stream(&hello(), &[])
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;

    assert_eq!(
        chunks,
        [
            Chunk::Text("Hel".into()),
            Chunk::Text("lo".into()),
            Chunk::Done(Reply::Final {
                text: "Hello".into()
            }),
        ]
    );
}

#[tokio::test]
async fn rate_limits_are_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(
            json!({ "stream": false, "max_completion_tokens": 64 }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "content": "ok" } }]
        })))
        .mount(&server)
        .await;

    let mut settings = settings(Provider::Openai, &server, "/v1");
    settings.max_tokens = Some(64);
    let reply = settings
        .client()
        .unwrap()
        .complete(&hello(), &[])
        .await
        .unwrap();

    assert_eq!(reply, Reply::Final { text: "ok".into() });
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn client_errors_surface_the_provider_message_without_retrying() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": { "message": "model not found", "type": "invalid_request_error" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let err = settings(Provider::Openai, &server, "/v1")
        .client()
        .unwrap()
        .complete(&hello(), &[])
        .await
        .unwrap_err();

    assert!(
        matches!(&err, Error::Api { status: 400, message } if message == "model not found"),
        "got {err:?}"
    );
}

#[tokio::test]
async fn claude_tool_calls_round_trip_with_its_own_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [
                { "type": "text", "text": "Checking." },
                { "type": "tool_use", "id": "toolu_1", "name": "list_contacts", "input": {} }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        })))
        .mount(&server)
        .await;

    let mut settings = settings(Provider::Claude, &server, "");
    settings.stream = false;
    settings.anthropic.betas = vec!["a".into(), "b".into()];
    let mut stream = settings
        .client()
        .unwrap()
        .stream(&hello(), &[])
        .await
        .unwrap();
    let Some(Ok(Chunk::Done(Reply::ToolCalls { calls, native }))) = stream.next().await else {
        panic!("expected tool calls");
    };

    assert_eq!(calls[0].id.as_str(), "toolu_1");
    assert_eq!(native.unwrap()["message"][0]["text"], "Checking.");
    let sent = &server.received_requests().await.unwrap()[0];
    assert_eq!(sent.headers["anthropic-beta"], "a,b");
}
