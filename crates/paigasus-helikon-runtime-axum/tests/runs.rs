//! Integration tests for the run endpoints (one-shot, SSE, async).

mod support;

/// **AC1** — a one-shot `POST /agents/{name}/runs` returns the aggregated run
/// result as JSON, with an `x-run-id` response header.
#[tokio::test]
async fn oneshot_returns_aggregated_result() {
    let addr = support::spawn_echo_server().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs"))
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

/// **AC2** — the SSE stream emits exactly the agent's local event sequence,
/// event for event.
#[tokio::test]
async fn sse_stream_matches_local_events() {
    let addr = support::spawn_echo_server().await;
    let text = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs?stream=sse"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let got = support::parse_sse(&text);
    let want = support::echo_script();
    // `AgentEvent` does not derive `PartialEq` in core, so assert event-for-event
    // equality through the canonical JSON of each sequence.
    assert_eq!(
        serde_json::to_value(&got).unwrap(),
        serde_json::to_value(&want).unwrap(),
    );
}

/// `?mode=async` accepts the run and returns `202 Accepted` with a `run_id`
/// field, without waiting for the run to finish.
#[tokio::test]
async fn async_mode_returns_202() {
    let addr = support::spawn_echo_server().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs?mode=async"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(v["run_id"].is_string());
}

/// Posting to an unregistered agent name returns `404 Not Found`.
#[tokio::test]
async fn unknown_agent_404() {
    let addr = support::spawn_echo_server().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/nope/runs"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
