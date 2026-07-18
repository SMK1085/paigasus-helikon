//! Integration tests for the one-shot `POST /agents/{name}/runs` endpoint.
//!
//! The SSE (`?stream=sse`) and async (`?mode=async`) transports are added by
//! Task 8; this file exercises the one-shot transport plus the selector
//! validation that guards all three.

mod support;

/// **AC1** — a one-shot `POST /agents/{name}/runs` returns the aggregated run
/// result as JSON, with an `x-run-id` response header.
#[tokio::test]
async fn oneshot_returns_aggregated_result() {
    let base = support::spawn_echo_server();
    let resp = reqwest::Client::new()
        .post(format!("{base}/agents/echo/runs"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hello"}"#)
        .send()
        .await
        .unwrap();
    assert!(resp.headers().contains_key("x-run-id"));
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["status"], "completed");
    assert_eq!(v["output"], "echo");
}

/// Posting to an unregistered agent name returns `404 Not Found`.
#[tokio::test]
async fn unknown_agent_404() {
    let base = support::spawn_echo_server();
    let resp = reqwest::Client::new()
        .post(format!("{base}/agents/nope/runs"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// An unrecognised `?mode=` selector is rejected with `400 Bad Request` instead
/// of silently falling back to one-shot.
#[tokio::test]
async fn invalid_mode_selector_is_400() {
    let base = support::spawn_echo_server();
    let resp = reqwest::Client::new()
        .post(format!("{base}/agents/echo/runs?mode=bogus"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// An unrecognised `?stream=` selector is rejected with `400 Bad Request`.
#[tokio::test]
async fn invalid_stream_selector_is_400() {
    let base = support::spawn_echo_server();
    let resp = reqwest::Client::new()
        .post(format!("{base}/agents/echo/runs?stream=foo"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// Requesting the async and SSE transports together is rejected with `400 Bad
/// Request` rather than silently preferring one.
#[tokio::test]
async fn conflicting_async_and_sse_is_400() {
    let base = support::spawn_echo_server();
    let resp = reqwest::Client::new()
        .post(format!("{base}/agents/echo/runs?mode=async&stream=sse"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
