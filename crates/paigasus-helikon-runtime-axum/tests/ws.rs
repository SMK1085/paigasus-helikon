//! Integration tests for the WebSocket run-events endpoint.

mod support;

use std::sync::Arc;

use futures_util::StreamExt;
use paigasus_helikon_core::AgentEvent;
use paigasus_helikon_runtime_axum::{AgentServer, AuthLayer, AuthRejection, Principal};

/// **AC1** — connecting to an existing, already-completed run replays the full
/// event sequence and then closes the stream (server sends a Close frame once
/// the terminal event has been delivered).
#[tokio::test]
async fn ws_replays_completed_run_then_closes() {
    let addr = support::spawn_echo_server().await;
    let run_id = support::create_async_run(addr, "echo").await;

    // Small yield so the scripted agent (which completes synchronously) has time
    // to finish before we subscribe. The subscribe stream handles both in-progress
    // and completed runs, so this is a belt-and-suspenders courtesy.
    tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

    let url = format!("ws://{addr}/agents/echo/runs/{run_id}/events");
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("WS handshake should succeed for a known run");

    // Bound the collection so a regression (e.g. a missing Close frame) fails
    // fast instead of hanging the test indefinitely.
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut got = Vec::new();
        while let Some(Ok(msg)) = ws.next().await {
            if msg.is_text() {
                got.push(support::parse_event(msg.to_text().unwrap()));
            }
        }
        got
    })
    .await
    .expect("WS stream must complete within 5s, not hang");

    // Full event sequence must be replayed, event-for-event.
    assert_eq!(
        serde_json::to_value(&got).unwrap(),
        serde_json::to_value(support::echo_script()).unwrap(),
    );
}

/// **AC2** — connecting to an unknown run id must fail the WebSocket handshake
/// (the server returns 404, not 101, so `connect_async` returns an error).
#[tokio::test]
async fn ws_unknown_id_404_before_upgrade() {
    let addr = support::spawn_echo_server().await;
    let url = format!("ws://{addr}/agents/echo/runs/{}/events", uuid::Uuid::nil());
    let err = tokio_tungstenite::connect_async(url)
        .await
        .expect_err("handshake should fail: server returns 404, not 101");
    assert_handshake_404(err);
}

/// Assert a failed WebSocket handshake was specifically an HTTP 404, not some
/// other transport-level failure.
fn assert_handshake_404(err: tokio_tungstenite::tungstenite::Error) {
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), 404, "handshake must fail with HTTP 404");
        }
        other => panic!("expected an HTTP 404 handshake failure, got: {other:?}"),
    }
}

/// Extract the status and (lossily-decoded) body of a failed WebSocket
/// handshake, without completing the upgrade.
///
/// `tungstenite::Error::Http`'s body comes from whatever was left in the
/// handshake read-buffer tail, so it is not guaranteed non-empty in general
/// (headers and body could in principle arrive in separate reads). Callers
/// that compare bodies for equality should assert the body carries the
/// expected shape first, so an empty tail on both sides can't make the
/// comparison silently vacuous.
fn handshake_failure_status_and_body(err: tokio_tungstenite::tungstenite::Error) -> (u16, String) {
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            let status = resp.status().as_u16();
            let body = resp
                .body()
                .as_ref()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            (status, body)
        }
        other => panic!("expected an HTTP handshake failure, got: {other:?}"),
    }
}

/// A WebSocket connection that targets the correct run id but the wrong agent
/// name must fail the upgrade (server returns 404 before the 101 handshake).
#[tokio::test]
async fn ws_name_mismatch_404_before_upgrade() {
    let addr = support::spawn_echo_server().await;
    let run_id = support::create_async_run(addr, "echo").await;
    // The run exists (agent "echo"), but the URL references a different agent.
    let url = format!("ws://{addr}/agents/other/runs/{run_id}/events");
    let err = tokio_tungstenite::connect_async(url)
        .await
        .expect_err("agent-name mismatch should fail the WS handshake (404, not 101)");
    assert_handshake_404(err);
}

/// A start-erroring run, reached over WebSocket, must surface a final synthetic
/// `RunFailed` frame, then a Close.
#[tokio::test]
async fn ws_emits_synthetic_run_failed_on_start_error() {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .runner(Arc::new(support::FailingRunner))
        .agent(Arc::new(support::ScriptedAgent {
            name: "echo".into(),
            events: support::echo_script(),
        }))
        .build()
        .expect("server builds");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { server.serve_with_listener(listener).await.unwrap() });

    // Create the (start-erroring) run via async mode to obtain a run id; it stays
    // registered (TTL 300s) so the WS handshake's registry check passes.
    let run_id = support::create_async_run(addr, "echo").await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let url = format!("ws://{addr}/agents/echo/runs/{run_id}/events");
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("WS handshake should succeed for a registered run");
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut got = Vec::new();
        while let Some(Ok(msg)) = ws.next().await {
            if msg.is_text() {
                got.push(support::parse_event(msg.to_text().unwrap()));
            }
        }
        got
    })
    .await
    .expect("WS drain must complete within 5s, not hang");

    assert_eq!(got.len(), 1, "exactly one synthetic terminal frame");
    assert!(
        matches!(&got[0], AgentEvent::RunFailed { error } if !error.is_empty()),
        "expected a non-empty RunFailed, got {:?}",
        got[0]
    );
}

/// A run that yields real events then ends with no terminal must get a final
/// synthetic `RunFailed` frame (generic message) over WebSocket, then a Close.
#[tokio::test]
async fn ws_emits_synthetic_run_failed_after_terminalless_stream() {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .runner(Arc::new(support::PartialThenEndRunner))
        .agent(Arc::new(support::ScriptedAgent {
            name: "echo".into(),
            events: vec![],
        }))
        .build()
        .expect("server builds");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { server.serve_with_listener(listener).await.unwrap() });

    let run_id = support::create_async_run(addr, "echo").await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let url = format!("ws://{addr}/agents/echo/runs/{run_id}/events");
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("WS handshake");
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut got = Vec::new();
        while let Some(Ok(msg)) = ws.next().await {
            if msg.is_text() {
                got.push(support::parse_event(msg.to_text().unwrap()));
            }
        }
        got
    })
    .await
    .expect("WS drain must complete within 5s, not hang");

    assert_eq!(got.len(), 2);
    assert!(matches!(&got[0], AgentEvent::TokenDelta { text } if text == "hi"));
    assert!(
        matches!(&got[1], AgentEvent::RunFailed { error }
            if error == "run ended before producing a terminal event"),
        "expected generic RunFailed last, got {:?}",
        got[1]
    );
}

// ── principal scoping ───────────────────────────────────────────────────────

/// Admits every request, and establishes a [`Principal`] only when the
/// `X-Test-Principal` header is present. Mirrors `tests/principal.rs`.
struct HeaderPrincipalAuth;

#[async_trait::async_trait]
impl AuthLayer for HeaderPrincipalAuth {
    async fn authenticate(
        &self,
        parts: &mut axum::http::request::Parts,
    ) -> Result<(), AuthRejection> {
        if let Some(value) = parts.headers.get("x-test-principal") {
            if let Ok(s) = value.to_str() {
                parts.extensions.insert(Principal(s.to_owned()));
            }
        }
        Ok(())
    }
}

/// Build an [`AgentServer`] mounting the `echo` [`support::ScriptedAgent`]
/// behind [`HeaderPrincipalAuth`], bind it to an ephemeral loopback port, spawn
/// the serve loop, and return the bound address.
async fn spawn_authed_echo_server() -> std::net::SocketAddr {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .auth(Arc::new(HeaderPrincipalAuth))
        .agent(Arc::new(support::ScriptedAgent {
            name: "echo".into(),
            events: support::echo_script(),
        }))
        .build()
        .expect("server builds");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { server.serve_with_listener(listener).await.unwrap() });
    addr
}

/// Create an async run as `principal` via `POST /agents/{name}/runs?mode=async`
/// and return the run id.
async fn create_async_run_as(
    addr: std::net::SocketAddr,
    agent_name: &str,
    principal: &str,
) -> String {
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/{agent_name}/runs?mode=async"))
        .header("content-type", "application/json")
        .header("x-test-principal", principal)
        .body(r#"{"input":"test"}"#)
        .send()
        .await
        .expect("async run request");
    assert_eq!(resp.status(), 202, "expected 202 Accepted");
    let v: serde_json::Value = resp.json().await.expect("async run response body");
    v["run_id"]
        .as_str()
        .expect("run_id field in response")
        .to_owned()
}

/// Build a WebSocket client request for `url`, attaching
/// `X-Test-Principal: {principal}` when given.
fn ws_request_as(
    url: &str,
    principal: Option<&str>,
) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    let mut request = url.into_client_request().expect("ws request");
    if let Some(p) = principal {
        request
            .headers_mut()
            .insert("x-test-principal", p.parse().expect("header value"));
    }
    request
}

/// Drain a connected WebSocket to completion, collecting every text frame as a
/// decoded [`AgentEvent`]. Bounded by a 5s timeout so a regression (e.g. a
/// missing Close frame) fails fast instead of hanging the test.
async fn drain_ws_events(
    mut ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Vec<AgentEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut got = Vec::new();
        while let Some(Ok(msg)) = ws.next().await {
            if msg.is_text() {
                got.push(support::parse_event(msg.to_text().unwrap()));
            }
        }
        got
    })
    .await
    .expect("WS stream must complete within 5s, not hang")
}

/// A run started by one principal is invisible to another — reported as a plain
/// 404, byte-identical (once the run id itself is normalized out) to a run id
/// that never existed. That equality — not just the 404 status — is the actual
/// security property: a principal-mismatch branch that grew a distinguishable
/// message (e.g. to help debugging) would reopen the existence oracle while
/// leaving a status-only assertion green.
#[tokio::test]
async fn cross_principal_subscription_is_404() {
    let addr = spawn_authed_echo_server().await;
    let run_id = create_async_run_as(addr, "echo", "alice").await;
    tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

    // mallory reaches alice's real run — denied.
    let url = format!("ws://{addr}/agents/echo/runs/{run_id}/events");
    let request = ws_request_as(&url, Some("mallory"));
    let err = tokio_tungstenite::connect_async(request)
        .await
        .expect_err("cross-principal subscription must fail the handshake (404, not 101)");
    let (cross_principal_status, cross_principal_body) = handshake_failure_status_and_body(err);
    assert_eq!(
        cross_principal_status, 404,
        "cross-principal denial must be 404, not 403"
    );

    // mallory reaches a run id that never existed — same agent name, same
    // principal, only the id differs.
    let never_existed_id = uuid::Uuid::new_v4();
    let never_existed_url = format!("ws://{addr}/agents/echo/runs/{never_existed_id}/events");
    let never_existed_request = ws_request_as(&never_existed_url, Some("mallory"));
    let err = tokio_tungstenite::connect_async(never_existed_request)
        .await
        .expect_err("an unknown run id must also fail the handshake (404, not 101)");
    let (never_existed_status, never_existed_body) = handshake_failure_status_and_body(err);
    assert_eq!(never_existed_status, 404, "unknown-run denial must be 404");

    // Pin down what we can actually rely on before comparing: a non-empty
    // body carrying the expected error shape (see `handshake_failure_status_and_body`
    // for why the tail is not guaranteed non-empty in general).
    assert!(
        cross_principal_body.contains("unknown agent"),
        "cross-principal denial body must carry the `unknown agent` shape, got {cross_principal_body:?}"
    );
    assert!(
        never_existed_body.contains("unknown agent"),
        "unknown-run denial body must carry the `unknown agent` shape, got {never_existed_body:?}"
    );

    // Both bodies embed their own (necessarily different) run id
    // (`unknown agent: echo/<id>`); normalize each out to a fixed token before
    // comparing, so the equality check is over everything EXCEPT the one piece
    // of data that must legitimately differ.
    let normalize = |body: &str, id: &str| body.replace(id, "<RUN_ID>");
    assert_eq!(
        normalize(&cross_principal_body, &run_id),
        normalize(&never_existed_body, &never_existed_id.to_string()),
        "a cross-principal denial must be indistinguishable from an unknown-run denial — \
         any difference would reveal that the run id exists and belongs to someone else"
    );
}

/// The owning principal can still subscribe, so the gate is not "deny all".
#[tokio::test]
async fn owning_principal_can_subscribe() {
    let addr = spawn_authed_echo_server().await;
    let run_id = create_async_run_as(addr, "echo", "alice").await;
    tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

    let url = format!("ws://{addr}/agents/echo/runs/{run_id}/events");
    let request = ws_request_as(&url, Some("alice"));
    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("owning principal's handshake should succeed");

    let got = drain_ws_events(ws).await;
    assert_eq!(
        serde_json::to_value(&got).unwrap(),
        serde_json::to_value(support::echo_script()).unwrap(),
    );
}

/// With no principals anywhere (`None == None`), subscription still succeeds —
/// the single-tenant and development-server path is unchanged.
#[tokio::test]
async fn unbound_run_is_subscribable_without_a_principal() {
    // No `AuthLayer` configured at all: `principal` resolves to `None` on both
    // the create and the subscribe side, matching the pre-existing
    // single-tenant behaviour exactly.
    let addr = support::spawn_echo_server().await;
    let run_id = support::create_async_run(addr, "echo").await;
    tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

    let url = format!("ws://{addr}/agents/echo/runs/{run_id}/events");
    let (ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("unbound run must remain subscribable with no principal established");

    let got = drain_ws_events(ws).await;
    assert_eq!(
        serde_json::to_value(&got).unwrap(),
        serde_json::to_value(support::echo_script()).unwrap(),
    );
}

/// The agent-name mismatch check still returns 404 independently of principals.
#[tokio::test]
async fn agent_name_mismatch_is_still_404() {
    let addr = spawn_authed_echo_server().await;
    let run_id = create_async_run_as(addr, "echo", "alice").await;
    tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

    // The SAME principal that owns the run, but the URL names a different
    // agent — proves the agent-name filter still fires on its own.
    let url = format!("ws://{addr}/agents/other/runs/{run_id}/events");
    let request = ws_request_as(&url, Some("alice"));
    let err = tokio_tungstenite::connect_async(request)
        .await
        .expect_err("agent-name mismatch should fail the WS handshake (404, not 101)");
    assert_handshake_404(err);
}
