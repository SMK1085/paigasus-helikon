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
