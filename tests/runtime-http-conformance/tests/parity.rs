//! Cross-runtime HTTP wire-format conformance.
//!
//! Boots the `paigasus-helikon-runtime-axum` and `paigasus-helikon-runtime-actix`
//! servers with the SAME [`scripted_agents`] set and asserts that every
//! user-facing HTTP surface matches between the two runtimes — byte-identical
//! where the wire format is fully determined, structurally equal where a field
//! is legitimately per-run (a `run_id`) or order-nondeterministic (a HashMap of
//! agents).
//!
//! The two servers are booted with the two different runtime models on purpose:
//! axum via `tokio::spawn` on the test's own runtime; actix via a dedicated OS
//! thread driving its own `actix_web::rt::System`.

use paigasus_helikon_runtime_http_conformance::scripted_agents;

/// A fixed token substituted for the per-run UUID before a byte comparison.
const RUN_ID_TOKEN: &str = "<RUN_ID>";

/// Boot the axum runtime on an ephemeral loopback port and return its base URL.
///
/// The serve loop is spawned as a task on the caller's tokio runtime, matching
/// the axum runtime's documented embedding model.
async fn boot_axum() -> String {
    let mut builder =
        paigasus_helikon_runtime_axum::AgentServer::<()>::builder().with_default_context();
    for agent in scripted_agents() {
        builder = builder.agent(agent);
    }
    let server = builder.build().expect("axum server builds");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        server
            .serve_with_listener(listener)
            .await
            .expect("axum serve loop");
    });
    format!("http://{addr}")
}

/// Boot the actix runtime on an ephemeral loopback port and return its base URL.
///
/// actix-web owns a non-`Send` per-worker runtime, so — unlike axum — the serve
/// loop cannot be a task on the test's runtime. The listener is bound on the
/// calling thread, then the serve loop is driven from an `actix_web::rt::System`
/// created on a dedicated OS thread. A brief readiness wait lets the accept loop
/// come up before the first connection attempt.
fn boot_actix() -> String {
    let mut builder =
        paigasus_helikon_runtime_actix::AgentServer::<()>::builder().with_default_context();
    for agent in scripted_agents() {
        builder = builder.agent(agent);
    }
    let server = builder.build().expect("actix server builds");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    std::thread::spawn(move || {
        actix_web::rt::System::new().block_on(async move {
            server
                .serve_with_listener(listener)
                .await
                .expect("actix serve loop");
        });
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    format!("http://{addr}")
}

/// An `AuthLayer` for the parity suite that maps the `X-Test-Principal` header
/// to a `Principal`. A request with no such header is admitted but establishes
/// no principal — which is exactly the fail-closed row under test.
mod principal_auth {
    use async_trait::async_trait;

    /// axum flavour of the header→principal auth layer.
    pub struct HeaderPrincipalAuth;

    #[async_trait]
    impl paigasus_helikon_runtime_axum::AuthLayer for HeaderPrincipalAuth {
        async fn authenticate(
            &self,
            parts: &mut axum::http::request::Parts,
        ) -> Result<(), paigasus_helikon_runtime_axum::AuthRejection> {
            if let Some(v) = parts.headers.get("x-test-principal") {
                if let Ok(s) = v.to_str() {
                    parts
                        .extensions
                        .insert(paigasus_helikon_runtime_axum::Principal(s.to_owned()));
                }
            }
            Ok(())
        }
    }

    /// actix flavour of the same layer.
    pub struct ActixHeaderPrincipalAuth;

    #[async_trait(?Send)]
    impl paigasus_helikon_runtime_actix::AuthLayer for ActixHeaderPrincipalAuth {
        async fn authenticate(
            &self,
            req: &actix_web::HttpRequest,
        ) -> Result<(), paigasus_helikon_runtime_actix::AuthRejection> {
            use actix_web::HttpMessage as _;
            let found = req
                .headers()
                .get("x-test-principal")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            if let Some(s) = found {
                req.extensions_mut()
                    .insert(paigasus_helikon_runtime_actix::Principal(s));
            }
            Ok(())
        }
    }
}

/// A fourth agent, mounted only on the authenticated servers, that echoes the
/// **merged conversation** (the session history the runner loaded plus this
/// turn's input) back as its assistant message.
///
/// The shared `scripted_agents()` set cannot prove session isolation over HTTP:
/// `echo` emits a fixed string, so its response body is identical whether or
/// not two principals collided. This agent makes the loaded history *observable
/// in the response body*, which is what turns the isolation assertion below
/// from a status-code check into a real one.
mod history_echo {
    use async_trait::async_trait;
    use futures_util::stream::{self, BoxStream, StreamExt as _};
    use paigasus_helikon_core::{
        Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
    };

    /// Echoes the merged conversation as one assistant message.
    pub struct HistoryEchoAgent;

    #[async_trait]
    impl Agent<()> for HistoryEchoAgent {
        fn name(&self) -> &str {
            "history"
        }

        fn description(&self) -> &str {
            "echoes the merged conversation (session history + this turn)"
        }

        async fn run(
            &self,
            _ctx: RunContext<()>,
            input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            let mut parts: Vec<String> = Vec::new();
            for item in &input.messages {
                let content = match item {
                    Item::UserMessage { content } => content,
                    Item::AssistantMessage { content, .. } => content,
                    _ => continue,
                };
                for part in content {
                    if let ContentPart::Text { text } = part {
                        parts.push(text.clone());
                    }
                }
            }
            Ok(stream::iter(vec![
                AgentEvent::MessageOutput {
                    item: Item::AssistantMessage {
                        content: vec![ContentPart::Text {
                            text: parts.join("|"),
                        }],
                        agent: None,
                    },
                },
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                },
            ])
            .boxed())
        }
    }
}

/// Boot an axum server with the header-principal auth layer. Mirrors
/// `boot_axum` otherwise, plus the `history` agent.
async fn boot_axum_authed() -> String {
    let mut builder = paigasus_helikon_runtime_axum::AgentServer::<()>::builder()
        .with_default_context()
        .auth(std::sync::Arc::new(principal_auth::HeaderPrincipalAuth))
        .agent(std::sync::Arc::new(history_echo::HistoryEchoAgent));
    for agent in scripted_agents() {
        builder = builder.agent(agent);
    }
    let server = builder.build().expect("axum authed server builds");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        server.serve_with_listener(listener).await.expect("serve");
    });
    format!("http://{addr}")
}

/// Boot an actix server with the header-principal auth layer. Mirrors
/// `boot_actix` otherwise, including its dedicated-thread `System`, plus the
/// `history` agent.
fn boot_actix_authed() -> String {
    let mut builder = paigasus_helikon_runtime_actix::AgentServer::<()>::builder()
        .with_default_context()
        .auth(std::sync::Arc::new(
            principal_auth::ActixHeaderPrincipalAuth,
        ))
        .agent(std::sync::Arc::new(history_echo::HistoryEchoAgent));
    for agent in scripted_agents() {
        builder = builder.agent(agent);
    }
    let server = builder.build().expect("actix authed server builds");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    std::thread::spawn(move || {
        actix_web::rt::System::new().block_on(async move {
            server.serve_with_listener(listener).await.expect("serve");
        });
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    format!("http://{addr}")
}

/// Replace the run's UUID (read from the body's `run_id` field) with a fixed
/// token so two otherwise-identical bodies can be byte-compared. Parsing only
/// locates the UUID; the substitution is a raw-text replace, so field order and
/// spacing are preserved (a parse-and-reserialize would hide framing drift).
fn normalize_run_id(body: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(body).expect("run response is JSON");
    let run_id = value["run_id"].as_str().expect("run_id is a string");
    body.replace(run_id, RUN_ID_TOKEN)
}

/// Decode the `data:` payloads of an SSE body into a `Vec` of JSON values, in
/// order. `event:` tag lines and blank separators are ignored.
fn sse_data_values(body: &str) -> Vec<serde_json::Value> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|data| !data.is_empty())
        .map(|data| serde_json::from_str::<serde_json::Value>(data).expect("SSE data is JSON"))
        .collect()
}

/// Order-insensitive canonical form of a JSON array body: each element rendered
/// as compact JSON, then sorted. Two `GET /agents` responses are set-equal iff
/// their canonical forms match, regardless of HashMap iteration order.
fn json_array_set(body: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(body).expect("body is JSON");
    let mut items: Vec<String> = value
        .as_array()
        .expect("body is a JSON array")
        .iter()
        .map(|v| serde_json::to_string(v).expect("element re-serializes"))
        .collect();
    items.sort();
    items
}

/// The full cross-runtime parity sweep. Both servers are booted once with the
/// same agent set, then every endpoint in the parity scope is checked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn axum_and_actix_are_wire_compatible() {
    let axum_base = boot_axum().await;
    let actix_base = boot_actix();
    let client = reqwest::Client::new();

    // ── one-shot RunResponse — byte-identical after run_id normalization ──────
    {
        let axum = client
            .post(format!("{axum_base}/agents/echo/runs"))
            .header("content-type", "application/json")
            .body(r#"{"input":"hi"}"#)
            .send()
            .await
            .expect("axum one-shot request");
        let actix = client
            .post(format!("{actix_base}/agents/echo/runs"))
            .header("content-type", "application/json")
            .body(r#"{"input":"hi"}"#)
            .send()
            .await
            .expect("actix one-shot request");

        assert_eq!(axum.status(), 200, "axum one-shot status");
        assert_eq!(actix.status(), 200, "actix one-shot status");
        assert!(
            axum.headers().contains_key("x-run-id"),
            "axum one-shot sets x-run-id"
        );
        assert!(
            actix.headers().contains_key("x-run-id"),
            "actix one-shot sets x-run-id"
        );

        let axum_body = axum.text().await.expect("axum one-shot body");
        let actix_body = actix.text().await.expect("actix one-shot body");

        // Content assertions on each runtime independently.
        for (name, body) in [("axum", &axum_body), ("actix", &actix_body)] {
            let v: serde_json::Value = serde_json::from_str(body).expect("one-shot body is JSON");
            assert_eq!(v["status"], "completed", "{name} one-shot status field");
            assert_eq!(v["output"], "echo", "{name} one-shot output field");
        }

        assert_eq!(
            normalize_run_id(&axum_body),
            normalize_run_id(&actix_body),
            "one-shot RunResponse bodies must be byte-identical after run_id normalization\n\
             axum : {axum_body}\nactix: {actix_body}"
        );
    }

    // ── SSE — Content-Type + decoded events + RAW BYTE parity ────────────────
    {
        let axum = client
            .post(format!("{axum_base}/agents/echo/runs?stream=sse"))
            .header("content-type", "application/json")
            .body(r#"{"input":"hi"}"#)
            .send()
            .await
            .expect("axum sse request");
        let actix = client
            .post(format!("{actix_base}/agents/echo/runs?stream=sse"))
            .header("content-type", "application/json")
            .body(r#"{"input":"hi"}"#)
            .send()
            .await
            .expect("actix sse request");

        assert_eq!(axum.status(), 200, "axum sse status");
        assert_eq!(actix.status(), 200, "actix sse status");
        let content_type = |resp: &reqwest::Response| -> String {
            resp.headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .expect("sse content-type header")
                .to_owned()
        };
        assert_eq!(
            content_type(&axum),
            "text/event-stream",
            "axum sse content-type"
        );
        assert_eq!(
            content_type(&actix),
            "text/event-stream",
            "actix sse content-type"
        );

        let axum_bytes = axum.bytes().await.expect("axum sse body");
        let actix_bytes = actix.bytes().await.expect("actix sse body");

        // Decoded event-sequence parity (semantic).
        let axum_text = String::from_utf8(axum_bytes.to_vec()).expect("axum sse is utf-8");
        let actix_text = String::from_utf8(actix_bytes.to_vec()).expect("actix sse is utf-8");
        assert_eq!(
            sse_data_values(&axum_text),
            sse_data_values(&actix_text),
            "SSE decoded event sequences must match"
        );

        // The key check: SSE frames carry no run_id, so the raw bytes must match
        // exactly. axum's `to_sse_event` and actix's hand-rolled `sse_frame` are
        // meant to produce identical `event: <tag>\ndata: <json>\n\n` frames.
        assert_eq!(
            axum_bytes, actix_bytes,
            "SSE raw bodies must be byte-identical\naxum : {axum_text:?}\nactix: {actix_text:?}"
        );
    }

    // ── GET /agents — set-equal (order-insensitive) ──────────────────────────
    {
        let axum_body = client
            .get(format!("{axum_base}/agents"))
            .send()
            .await
            .expect("axum GET /agents")
            .text()
            .await
            .expect("axum /agents body");
        let actix_body = client
            .get(format!("{actix_base}/agents"))
            .send()
            .await
            .expect("actix GET /agents")
            .text()
            .await
            .expect("actix /agents body");

        assert_eq!(
            json_array_set(&axum_body),
            json_array_set(&actix_body),
            "GET /agents must be set-equal\naxum : {axum_body}\nactix: {actix_body}"
        );
    }

    // ── async ?mode=async — 202 + string run_id ──────────────────────────────
    {
        for (name, base) in [("axum", &axum_base), ("actix", &actix_base)] {
            let resp = client
                .post(format!("{base}/agents/echo/runs?mode=async"))
                .header("content-type", "application/json")
                .body(r#"{"input":"hi"}"#)
                .send()
                .await
                .unwrap_or_else(|e| panic!("{name} async request: {e}"));
            assert_eq!(resp.status(), 202, "{name} async status");
            let v: serde_json::Value = resp.json().await.expect("async body is JSON");
            assert!(
                v["run_id"].as_str().is_some(),
                "{name} async response carries a string run_id: {v}"
            );
        }
    }

    // ── content-type is case-insensitive (RFC 9110 §8.3.1) ───────────────────
    {
        for (name, base) in [("axum", &axum_base), ("actix", &actix_base)] {
            for ct in ["Application/JSON", "APPLICATION/JSON; charset=UTF-8"] {
                let resp = client
                    .post(format!("{base}/agents/echo/runs?mode=async"))
                    .header("content-type", ct)
                    .body(r#"{"input":"hi"}"#)
                    .send()
                    .await
                    .unwrap_or_else(|e| panic!("{name} request with `{ct}`: {e}"));
                assert_eq!(
                    resp.status(),
                    202,
                    "{name} must accept `{ct}` — media types are case-insensitive"
                );
            }
        }
    }

    // ── error body — 404 + byte-identical {"error":...} ──────────────────────
    {
        let axum_resp = client
            .post(format!("{axum_base}/agents/does-not-exist/runs"))
            .header("content-type", "application/json")
            .body(r#"{"input":"hi"}"#)
            .send()
            .await
            .expect("axum unknown-agent request");
        let actix_resp = client
            .post(format!("{actix_base}/agents/does-not-exist/runs"))
            .header("content-type", "application/json")
            .body(r#"{"input":"hi"}"#)
            .send()
            .await
            .expect("actix unknown-agent request");

        assert_eq!(axum_resp.status(), 404, "axum unknown-agent status");
        assert_eq!(actix_resp.status(), 404, "actix unknown-agent status");

        let axum_body = axum_resp.text().await.expect("axum error body");
        let actix_body = actix_resp.text().await.expect("actix error body");
        assert_eq!(
            axum_body, actix_body,
            "404 error bodies must be byte-identical\naxum : {axum_body}\nactix: {actix_body}"
        );
    }

    // ── /openapi.json — structural: 200 + the three documented paths ─────────
    {
        for (name, base) in [("axum", &axum_base), ("actix", &actix_base)] {
            let resp = client
                .get(format!("{base}/openapi.json"))
                .send()
                .await
                .unwrap_or_else(|e| panic!("{name} openapi request: {e}"));
            assert_eq!(resp.status(), 200, "{name} openapi status");
            let spec: serde_json::Value = resp.json().await.expect("openapi body is JSON");
            let paths = spec["paths"]
                .as_object()
                .unwrap_or_else(|| panic!("{name} openapi document has a `paths` object: {spec}"));
            for expected in [
                "/agents",
                "/agents/{name}/runs",
                "/agents/{name}/runs/{id}/events",
            ] {
                assert!(
                    paths.contains_key(expected),
                    "{name} openapi paths missing `{expected}`; got keys {:?}",
                    paths.keys().collect::<Vec<_>>()
                );
            }
        }
    }
}

/// The shared fixture set must expose all three agents on both runtimes. `boom`
/// and `hang` are what make the redaction and in-flight-cap assertions
/// reachable; without them those behaviours cannot be triggered from the
/// parity suite at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_set_exposes_all_three_agents() {
    let axum_base = boot_axum().await;
    let actix_base = boot_actix();
    let client = reqwest::Client::new();

    for (name, base) in [("axum", &axum_base), ("actix", &actix_base)] {
        let body = client
            .get(format!("{base}/agents"))
            .send()
            .await
            .unwrap_or_else(|e| panic!("{name} GET /agents: {e}"))
            .text()
            .await
            .expect("agents body");
        let value: serde_json::Value = serde_json::from_str(&body).expect("agents body is JSON");
        let names: Vec<&str> = value
            .as_array()
            .expect("agents body is an array")
            .iter()
            .filter_map(|a| a["name"].as_str())
            .collect();
        for expected in ["echo", "boom", "hang"] {
            assert!(
                names.contains(&expected),
                "{name} /agents is missing `{expected}`; got {names:?}"
            );
        }
    }
}

/// The runner's error text must not reach the caller on ANY transport. A 500
/// body carrying it is CWE-209; so is the synthetic `run_failed` frame the SSE
/// and WebSocket transports emit, which is a 200 response and would otherwise
/// let `?stream=sse` walk straight around the redaction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_error_detail_is_redacted_on_every_transport() {
    /// Substring of the underlying `boom` failure. Its presence anywhere on the
    /// wire is the bug this test exists to catch.
    const LEAK: &str = "max turns";

    let axum_base = boot_axum().await;
    let actix_base = boot_actix();
    let client = reqwest::Client::new();

    // ── one-shot: 500 with a fixed, non-diagnostic body ──────────────────────
    let mut oneshot_bodies = Vec::new();
    for (name, base) in [("axum", &axum_base), ("actix", &actix_base)] {
        let resp = client
            .post(format!("{base}/agents/boom/runs"))
            .header("content-type", "application/json")
            .body(r#"{"input":"hi"}"#)
            .send()
            .await
            .unwrap_or_else(|e| panic!("{name} boom one-shot: {e}"));
        assert_eq!(resp.status(), 500, "{name} boom one-shot status");
        let body = resp.text().await.expect("boom one-shot body");
        assert_eq!(
            body, r#"{"error":"internal error"}"#,
            "{name} 500 body must be the fixed public string"
        );
        assert!(
            !body.contains(LEAK),
            "{name} 500 body leaked `{LEAK}`: {body}"
        );
        oneshot_bodies.push(body);
    }
    assert_eq!(
        oneshot_bodies[0], oneshot_bodies[1],
        "500 bodies must be byte-identical across runtimes"
    );

    // ── SSE: the synthetic run_failed frame is redacted and byte-identical ───
    let mut sse_bytes = Vec::new();
    for (name, base) in [("axum", &axum_base), ("actix", &actix_base)] {
        let resp = client
            .post(format!("{base}/agents/boom/runs?stream=sse"))
            .header("content-type", "application/json")
            .body(r#"{"input":"hi"}"#)
            .send()
            .await
            .unwrap_or_else(|e| panic!("{name} boom sse: {e}"));
        assert_eq!(resp.status(), 200, "{name} boom sse status");
        let text = resp.text().await.expect("boom sse body");
        assert!(
            !text.contains(LEAK),
            "{name} SSE body leaked `{LEAK}`: {text}"
        );
        let values = sse_data_values(&text);
        let terminal = values.last().expect("sse stream has at least one frame");
        assert_eq!(terminal["type"], "run_failed", "{name} sse terminal type");
        assert_eq!(
            terminal["error"], "run failed to start",
            "{name} sse terminal carries the fixed public string"
        );
        sse_bytes.push(text);
    }
    assert_eq!(
        sse_bytes[0], sse_bytes[1],
        "SSE bodies for a failed run must be byte-identical across runtimes"
    );

    // ── WebSocket: same redacted frame ───────────────────────────────────────
    let mut ws_frames = Vec::new();
    for (name, base) in [("axum", &axum_base), ("actix", &actix_base)] {
        let run_id = {
            let resp = client
                .post(format!("{base}/agents/boom/runs?mode=async"))
                .header("content-type", "application/json")
                .body(r#"{"input":"hi"}"#)
                .send()
                .await
                .unwrap_or_else(|e| panic!("{name} boom async: {e}"));
            assert_eq!(resp.status(), 202, "{name} boom async status");
            let v: serde_json::Value = resp.json().await.expect("async body is JSON");
            v["run_id"].as_str().expect("run_id string").to_owned()
        };

        let ws_url = format!("{base}/agents/boom/runs/{run_id}/events").replacen("http", "ws", 1);
        let (mut socket, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .unwrap_or_else(|e| panic!("{name} ws connect: {e}"));

        let mut last_text: Option<String> = None;
        while let Some(Ok(msg)) = futures_util::StreamExt::next(&mut socket).await {
            if let tokio_tungstenite::tungstenite::Message::Text(t) = msg {
                last_text = Some(t.to_string());
            }
        }
        let frame = last_text.expect("ws delivered at least one text frame");
        assert!(
            !frame.contains(LEAK),
            "{name} ws frame leaked `{LEAK}`: {frame}"
        );
        let v: serde_json::Value = serde_json::from_str(&frame).expect("ws frame is JSON");
        assert_eq!(v["type"], "run_failed", "{name} ws terminal type");
        assert_eq!(
            v["error"], "run failed to start",
            "{name} ws terminal carries the fixed public string"
        );
        ws_frames.push(frame);
    }
    assert_eq!(
        ws_frames[0], ws_frames[1],
        "WebSocket terminal frames must be byte-identical across runtimes"
    );
}

/// A named session with no established principal must be refused identically on
/// both runtimes. This is the most security-critical new response AND the one
/// whose implementations diverge most — axum gates via `from_fn_with_state`
/// plus a `Request::from_parts` reassembly, actix via a hand-rolled
/// `AuthGuard` short-circuit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_session_without_principal_is_refused_identically() {
    let axum_base = boot_axum_authed().await;
    let actix_base = boot_actix_authed();
    let client = reqwest::Client::new();

    let mut bodies = Vec::new();
    for (name, base) in [("axum", &axum_base), ("actix", &actix_base)] {
        let resp = client
            .post(format!("{base}/agents/echo/runs"))
            .header("content-type", "application/json")
            .header("x-session-id", "victim-session")
            .body(r#"{"input":"hi"}"#)
            .send()
            .await
            .unwrap_or_else(|e| panic!("{name} unbound-session request: {e}"));
        assert_eq!(
            resp.status(),
            403,
            "{name} unbound named session must be 403"
        );
        bodies.push(resp.text().await.expect("403 body"));
    }
    assert_eq!(
        bodies[0], bodies[1],
        "403 bodies must be byte-identical\naxum : {}\nactix: {}",
        bodies[0], bodies[1]
    );
    assert_eq!(
        bodies[0],
        r#"{"error":"unauthorized: session id requires an authenticated principal (403 Forbidden)"}"#,
        "the 403 body is pinned so the two runtimes cannot drift"
    );
}

/// Two principals using the SAME `X-Session-Id` must not share conversation
/// history. This is the IDOR itself, asserted end to end on both runtimes.
///
/// The `history` agent (not `echo`) is what gives this teeth: its response body
/// carries the conversation the runner loaded, so a collision shows up as
/// alice's literal text inside mallory's response.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_session_id_different_principals_are_isolated() {
    let axum_base = boot_axum_authed().await;
    let actix_base = boot_actix_authed();
    let client = reqwest::Client::new();

    for (name, base) in [("axum", &axum_base), ("actix", &actix_base)] {
        // Principal "alice" runs once under session id "shared".
        let alice = client
            .post(format!("{base}/agents/history/runs"))
            .header("content-type", "application/json")
            .header("x-session-id", "shared")
            .header("x-test-principal", "alice")
            .body(r#"{"input":"alice-secret"}"#)
            .send()
            .await
            .unwrap_or_else(|e| panic!("{name} alice run: {e}"));
        assert_eq!(alice.status(), 200, "{name} alice run status");
        let alice_run: serde_json::Value = alice.json().await.expect("alice body");

        // Principal "mallory" reuses the id. If the two collide, mallory's run
        // resumes alice's session and her text is echoed back to him.
        let mallory = client
            .post(format!("{base}/agents/history/runs"))
            .header("content-type", "application/json")
            .header("x-session-id", "shared")
            .header("x-test-principal", "mallory")
            .body(r#"{"input":"mallory-probe"}"#)
            .send()
            .await
            .unwrap_or_else(|e| panic!("{name} mallory run: {e}"));
        assert_eq!(mallory.status(), 200, "{name} mallory run status");
        let mallory_run: serde_json::Value = mallory.json().await.expect("mallory body");

        assert_ne!(
            alice_run["run_id"], mallory_run["run_id"],
            "{name} runs must be distinct"
        );
        assert_eq!(
            mallory_run["output"], "mallory-probe",
            "{name} mallory must see only his own turn, not alice's history"
        );
        assert!(
            !serde_json::to_string(&mallory_run)
                .expect("re-serialize")
                .contains("alice-secret"),
            "{name}: mallory's response leaked alice's input — sessions collided"
        );

        // Positive control: alice still resumes her OWN conversation, so the
        // isolation above is not just "session affinity is broken for everyone".
        let alice_again = client
            .post(format!("{base}/agents/history/runs"))
            .header("content-type", "application/json")
            .header("x-session-id", "shared")
            .header("x-test-principal", "alice")
            .body(r#"{"input":"alice-again"}"#)
            .send()
            .await
            .unwrap_or_else(|e| panic!("{name} alice second run: {e}"));
        assert_eq!(alice_again.status(), 200, "{name} alice second run status");
        let alice_again_run: serde_json::Value = alice_again.json().await.expect("alice body");
        let output = alice_again_run["output"].as_str().expect("output string");
        assert!(
            output.contains("alice-secret"),
            "{name}: alice lost her own conversation; got {output:?}"
        );
        assert!(
            !output.contains("mallory-probe"),
            "{name}: alice read mallory's conversation; got {output:?}"
        );
    }
}

/// Extract the status and (lossily-decoded) body of a failed WebSocket
/// handshake — used below to assert a denial's exact wire shape without
/// completing the upgrade.
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

/// A run's event stream must be readable only by the principal that started it.
/// The denial is folded into the EXISTING 404 rather than a new 403: a distinct
/// status would confirm the run id exists and belongs to someone else, turning
/// the endpoint into an existence oracle for harvested ids.
///
/// The negative (mallory) check goes through a genuine WebSocket handshake
/// request (`tokio_tungstenite::connect_async`) rather than a bare
/// `reqwest::Client::get()`. axum's `WebSocketUpgrade` extractor validates the
/// `Connection` / `Upgrade` / `Sec-WebSocket-*` handshake headers *before* the
/// handler body runs, independently of the registry — so a plain non-upgrade
/// GET always fails with 400 on axum regardless of whether the run exists or
/// who owns it (a pre-existing axum/actix ordering asymmetry: actix checks the
/// registry before attempting the upgrade, axum validates the upgrade before
/// the handler body executes at all). A bare GET therefore cannot distinguish
/// "denied by the principal gate" from "rejected before the gate was ever
/// reached" on axum, and would pass even against the unfixed handler. A real
/// handshake request reaches the same registry `.filter()` chain the positive
/// control below exercises, which is what actually proves the fix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_events_are_scoped_to_the_owning_principal() {
    let axum_base = boot_axum_authed().await;
    let actix_base = boot_actix_authed();
    let client = reqwest::Client::new();

    let mut denial_bodies = Vec::new();
    for (name, base) in [("axum", &axum_base), ("actix", &actix_base)] {
        // alice starts a run.
        let resp = client
            .post(format!("{base}/agents/echo/runs?mode=async"))
            .header("content-type", "application/json")
            .header("x-test-principal", "alice")
            .body(r#"{"input":"hi"}"#)
            .send()
            .await
            .unwrap_or_else(|e| panic!("{name} alice async run: {e}"));
        assert_eq!(resp.status(), 202, "{name} alice async status");
        let v: serde_json::Value = resp.json().await.expect("async body");
        let run_id = v["run_id"].as_str().expect("run_id string").to_owned();

        let ws_url = format!("{base}/agents/echo/runs/{run_id}/events").replacen("http", "ws", 1);

        // mallory must not reach it — 404, and NOT an upgrade.
        let mallory_request = {
            use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
            let mut r = ws_url.clone().into_client_request().expect("ws request");
            r.headers_mut()
                .insert("x-test-principal", "mallory".parse().expect("header value"));
            r
        };
        let err = tokio_tungstenite::connect_async(mallory_request)
            .await
            .err()
            .unwrap_or_else(|| {
                panic!("{name}: mallory's handshake unexpectedly succeeded — cross-principal isolation broken")
            });
        let (status, body) = handshake_failure_status_and_body(err);
        assert_eq!(
            status, 404,
            "{name}: another principal's run must 404, not 403 (no existence oracle)"
        );
        // `tungstenite::Error::Http`'s body comes from whatever was left in the
        // handshake read-buffer tail (tungstenite's client handshake reads
        // headers and body opportunistically off the same buffer). If headers
        // and body ever arrived in separate reads, that tail — and therefore
        // `body` — would be empty on BOTH runtimes, and the byte-equality
        // check below would silently degrade to `"" == ""`, passing without
        // asserting anything. Pin down what we can actually rely on first: a
        // non-empty body carrying the expected error shape.
        assert!(
            body.contains("unknown agent"),
            "{name}: cross-principal denial body must carry the `unknown agent` shape, got {body:?}"
        );
        // The body also embeds the per-run UUID (`unknown agent: echo/<run_id>`);
        // normalize it to a fixed token before the cross-runtime byte compare,
        // exactly as `normalize_run_id` does for the JSON run responses above.
        denial_bodies.push(body.replace(&run_id, RUN_ID_TOKEN));

        // Positive control: alice CAN reach her own run, so the test cannot pass
        // by denying everyone.
        let request = {
            use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
            let mut r = ws_url.into_client_request().expect("ws request");
            r.headers_mut()
                .insert("x-test-principal", "alice".parse().expect("header value"));
            r
        };
        let (socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .unwrap_or_else(|e| panic!("{name} alice ws connect (positive control): {e}"));
        drop(socket);
    }
    assert_eq!(
        denial_bodies[0], denial_bodies[1],
        "cross-principal denial bodies must be byte-identical"
    );
}
