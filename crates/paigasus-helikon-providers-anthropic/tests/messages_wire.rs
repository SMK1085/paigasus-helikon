//! Wire-format tests for the request side of Anthropic Messages.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelError, ModelRequest,
};
use paigasus_helikon_providers_anthropic::AnthropicModel;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn empty_stream_response() -> ResponseTemplate {
    // Minimal SSE that ends cleanly so the stream completes.
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(
            "event: message_start\n\
             data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n\
             event: message_delta\n\
             data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n\
             event: message_stop\n\
             data: {\"type\":\"message_stop\"}\n\n",
            "text/event-stream",
        )
}

fn user(s: &str) -> Item {
    Item::UserMessage {
        content: vec![ContentPart::Text { text: s.to_owned() }],
    }
}

fn req_with(messages: Vec<Item>) -> ModelRequest {
    let mut r = ModelRequest::new();
    r.messages = messages;
    r
}

#[tokio::test]
async fn request_carries_required_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header("content-type", "application/json"))
        .respond_with(empty_stream_response())
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(req_with(vec![user("hi")]), CancellationToken::new())
        .await
        .unwrap();
    while let Some(_) = s.next().await {}
}

#[tokio::test]
async fn http_429_with_retry_after_maps_to_rate_limited() {
    let server = MockServer::start().await;
    let body = serde_json::json!({"type": "error", "error": {"type": "rate_limit_error", "message": "slow down"}});
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "7")
                .set_body_json(body),
        )
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(req_with(vec![user("hi")]), CancellationToken::new())
        .await
        .unwrap();
    let first = s.next().await.expect("stream not empty");
    match first {
        Err(ModelError::RateLimited { retry_after_ms }) => {
            assert_eq!(retry_after_ms, Some(7_000));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn http_529_overloaded_maps_to_unavailable() {
    let server = MockServer::start().await;
    let body =
        serde_json::json!({"type":"error","error":{"type":"overloaded_error","message":"busy"}});
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(529).set_body_json(body))
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(req_with(vec![user("hi")]), CancellationToken::new())
        .await
        .unwrap();
    let first = s.next().await.expect("stream not empty");
    assert!(matches!(first, Err(ModelError::Unavailable)));
}

#[tokio::test]
async fn http_400_prompt_too_long_maps_to_context_length_exceeded() {
    let server = MockServer::start().await;
    let body = serde_json::json!({"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 200k > 200k tokens"}});
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(400).set_body_json(body))
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(req_with(vec![user("hi")]), CancellationToken::new())
        .await
        .unwrap();
    let first = s.next().await.expect("stream not empty");
    assert!(matches!(first, Err(ModelError::ContextLengthExceeded)));
}

#[tokio::test]
async fn http_401_auth_maps_to_refused() {
    let server = MockServer::start().await;
    let body = serde_json::json!({"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}});
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(body))
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-bad")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(req_with(vec![user("hi")]), CancellationToken::new())
        .await
        .unwrap();
    let first = s.next().await.expect("stream not empty");
    match first {
        Err(ModelError::Refused { reason }) => assert!(reason.contains("invalid")),
        other => panic!("expected Refused, got {other:?}"),
    }
}
