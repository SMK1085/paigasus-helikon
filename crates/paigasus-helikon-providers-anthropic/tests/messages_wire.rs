//! Wire-format tests for the request side of Anthropic Messages.

mod common;

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelError, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_anthropic::AnthropicModel;
use paigasus_helikon_runtime_tokio::{RetryPolicy, RetryingModel};
use std::time::Duration;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// HAND-AUTHORED, not captured. The SMA-317 plan (PR #33) contains this body
/// (`docs/superpowers/plans/2026-05-26-sma-317-anthropic-provider.md`, line
/// 4094). No source for its shape is recorded. It sends `message_start`,
/// `message_delta` and `message_stop` only, with no content block. Its
/// `message_start` carries only `usage.input_tokens`. The tests that use it
/// check the request side, not the response.
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

    common::install_crypto_provider();
    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(req_with(vec![user("hi")]), CancellationToken::new())
        .await
        .unwrap();
    while s.next().await.is_some() {}
}

#[tokio::test]
async fn http_429_with_retry_after_maps_to_rate_limited() {
    let server = MockServer::start().await;
    // HAND-AUTHORED, not captured. The SMA-317 plan (PR #33) contains this body
    // (docs/superpowers/plans/2026-05-26-sma-317-anthropic-provider.md, line
    // 4142). The SMA-317 design states the error envelope and the error types
    // (docs/superpowers/specs/2026-05-26-sma-317-anthropic-provider-design.md,
    // lines 327-338) but names no source. No fixture in tests/fixtures/ is an
    // HTTP error body. The test checks the status, the `retry-after` header and
    // the mapped error.
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

    common::install_crypto_provider();
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
    // HAND-AUTHORED, not captured. The SMA-317 plan (PR #33) contains this body
    // (docs/superpowers/plans/2026-05-26-sma-317-anthropic-provider.md, line
    // 4176). The SMA-317 design states the error envelope and the error types
    // (docs/superpowers/specs/2026-05-26-sma-317-anthropic-provider-design.md,
    // lines 327-338) but names no source. No fixture in tests/fixtures/ is an
    // HTTP error body. The test checks the status and the mapped error.
    let body =
        serde_json::json!({"type":"error","error":{"type":"overloaded_error","message":"busy"}});
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(529).set_body_json(body))
        .mount(&server)
        .await;

    common::install_crypto_provider();
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
    // HAND-AUTHORED, not captured. The SMA-317 plan (PR #33) contains this body
    // (docs/superpowers/plans/2026-05-26-sma-317-anthropic-provider.md, line
    // 4201). The SMA-317 design states the error envelope and the error types
    // (docs/superpowers/specs/2026-05-26-sma-317-anthropic-provider-design.md,
    // lines 327-338) but names no source. No fixture in tests/fixtures/ is an
    // HTTP error body. The test checks the mapped error, which depends on the
    // message text.
    let body = serde_json::json!({"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 200k > 200k tokens"}});
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(400).set_body_json(body))
        .mount(&server)
        .await;

    common::install_crypto_provider();
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

/// HAND-AUTHORED, not captured. Shape from the paigasus-gateway Anthropic error envelope (SMA-750 §6.1, quoted in the SMA-868 design).
fn error_body(error_type: &str, message: &str) -> serde_json::Value {
    serde_json::json!({"type":"error","error":{"type":error_type,"message":message}})
}

fn model_for(server: &MockServer) -> AnthropicModel {
    common::install_crypto_provider();
    AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap()
}

/// Mount `response` on a fresh server, invoke once and return the first stream item.
async fn first_item(response: ResponseTemplate) -> Result<ModelEvent, ModelError> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(response)
        .mount(&server)
        .await;
    let model = model_for(&server);
    let mut s = model
        .invoke(req_with(vec![user("hi")]), CancellationToken::new())
        .await
        .unwrap();
    s.next().await.expect("stream not empty")
}

#[tokio::test]
async fn http_4xx_statuses_map_to_rejected() {
    for (status, error_type) in [
        (400, "invalid_request_error"),
        (401, "authentication_error"),
        (403, "permission_error"),
        (404, "not_found_error"),
        (413, "request_too_large"),
    ] {
        let item =
            first_item(ResponseTemplate::new(status).set_body_json(error_body(error_type, "m")))
                .await;
        match item {
            Err(ModelError::Rejected {
                status: got_status,
                error_type: got_type,
                message,
            }) => {
                assert_eq!(got_status, status);
                assert_eq!(got_type, error_type);
                assert_eq!(message, "m");
            }
            other => panic!("status {status}: expected Rejected, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn http_401_html_body_maps_to_rejected_with_empty_error_type() {
    // A proxy page, not the Anthropic envelope. No JSON, so no `error.type`.
    let item = first_item(
        ResponseTemplate::new(401)
            .insert_header("content-type", "text/html")
            .set_body_raw("<html>denied</html>", "text/html"),
    )
    .await;
    match item {
        Err(ModelError::Rejected {
            status,
            error_type,
            message,
        }) => {
            assert_eq!(status, 401);
            assert_eq!(error_type, "");
            assert_eq!(message, "<html>denied</html>");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn http_403_empty_body_maps_to_rejected_with_empty_error_type() {
    let item = first_item(ResponseTemplate::new(403)).await;
    match item {
        Err(ModelError::Rejected {
            status, error_type, ..
        }) => {
            assert_eq!(status, 403);
            assert_eq!(error_type, "");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn http_429_billing_error_keeps_retry_after() {
    let item = first_item(
        ResponseTemplate::new(429)
            .insert_header("retry-after", "4")
            .set_body_json(error_body("billing_error", "budget")),
    )
    .await;
    match item {
        Err(ModelError::RateLimited { retry_after_ms }) => {
            assert_eq!(retry_after_ms, Some(4_000));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

fn retry_policy() -> RetryPolicy {
    RetryPolicy::new()
        .max_attempts(3)
        .base_delay(Duration::from_millis(1))
        .jitter(false)
}

#[tokio::test]
async fn rejected_is_not_retried_by_retrying_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(error_body("authentication_error", "invalid x-api-key")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let model = RetryingModel::new(model_for(&server), retry_policy());
    let mut s = model
        .invoke(req_with(vec![user("hi")]), CancellationToken::new())
        .await
        .unwrap();
    let first = s.next().await.expect("stream not empty");
    assert!(
        matches!(first, Err(ModelError::Rejected { status: 401, .. })),
        "got {first:?}",
    );
    // The `MockServer` drop checks the call count.
}

/// Control for the test above: a retryable error makes three HTTP calls, so
/// the `expect(1)` there can fail.
#[tokio::test]
async fn unavailable_is_retried_by_retrying_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(529).set_body_json(error_body("overloaded_error", "busy")),
        )
        .expect(3)
        .mount(&server)
        .await;

    let model = RetryingModel::new(model_for(&server), retry_policy());
    let mut s = model
        .invoke(req_with(vec![user("hi")]), CancellationToken::new())
        .await
        .unwrap();
    let first = s.next().await.expect("stream not empty");
    assert!(
        matches!(first, Err(ModelError::Unavailable)),
        "got {first:?}"
    );
}

#[tokio::test]
async fn blank_only_request_fails_without_http_call() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(empty_stream_response())
        .expect(0)
        .mount(&server)
        .await;

    common::install_crypto_provider();
    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let res = model
        .invoke(req_with(vec![user("  ")]), CancellationToken::new())
        .await;
    assert!(matches!(res, Err(ModelError::Other(_))));
}

#[tokio::test]
async fn typed_tool_entry_is_sent_in_request_body() {
    use paigasus_helikon_core::{ProviderToolType, ToolDef};
    use paigasus_helikon_providers_anthropic::PROVIDER_ID;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(empty_stream_response())
        .mount(&server)
        .await;

    common::install_crypto_provider();
    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let def = ToolDef::new(
        "memory",
        "custom description",
        serde_json::json!({"type": "object"}),
    )
    .with_provider_types(vec![ProviderToolType::new(PROVIDER_ID, "memory_20250818")]);
    let mut req = req_with(vec![user("hi")]);
    req.tools = vec![def];
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    while s.next().await.is_some() {}

    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(
        body["tools"][0],
        serde_json::json!({"type": "memory_20250818", "name": "memory"})
    );
    assert!(received[0].headers.get("anthropic-beta").is_none());
}
