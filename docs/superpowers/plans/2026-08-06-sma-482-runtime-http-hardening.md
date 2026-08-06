# SMA-482 HTTP Runtime Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close four security findings (CWE-209 information disclosure, CWE-639 IDOR on session ids, CWE-770 unbounded runs, CWE-639 IDOR on the WebSocket events endpoint) in `paigasus-helikon-runtime-axum` and `paigasus-helikon-runtime-actix` simultaneously, with cross-runtime parity asserted rather than assumed.

**Architecture:** Every change lands in both runtime crates in the same task, because `tests/runtime-http-conformance` asserts the two are wire-compatible and a one-sided change breaks it. The two crates are structurally identical — `handlers/runs.rs` resolves a session, takes a per-session lock, then calls `registry.create` — so each task is the same edit twice, differing only in framework types (axum `Parts` vs actix `HttpRequest`).

**Tech Stack:** Rust 1.94, axum 0.8, actix-web 4, tokio, `async-trait`, `tracing`, `tokio-tungstenite` (test-only), `utoipa` (behind the `openapi` feature).

**Spec:** `docs/superpowers/specs/2026-08-06-sma-482-runtime-http-hardening-design.md` (revision 3).

## Global Constraints

- **Worktree:** all work happens in `/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/ab7762f8-fcc1-4a23-8e34-063c7633315b/scratchpad/sma-482`, on branch `feature/sma-482-runtime-axum-runtime-actix-harden-5xx-redaction-session`. Use worktree-absolute paths for every file edit — a bare `crates/…` path lands in the main checkout.
- **Never run `git add -A` or `git add .`** — `.env` is untracked but not gitignored, so a broad add stages secrets. Stage explicit paths only, and verify with `git show --stat`.
- **Never run any git command that moves HEAD or mutates refs** beyond committing on the current branch (no `checkout`, `switch`, `reset`, `rebase`, `stash`). The object store is shared with other worktrees.
- **Commits are signed** via a 1Password SSH key. Do not pass `--no-gpg-sign` or `-c commit.gpgsign=false`. If a commit fails with `failed to fill whole buffer`, the vault is locked — stop and ask, do not bypass.
- **Commit format:** `<type>(<scope>): SMA-482 <lowercase message>`. Allowed scopes are fixed by `.versionrc`; this plan uses only `runtime-axum`, `runtime-actix`, `runtime`, `spec`, and `plan`.
- **The breaking-change marker is load-bearing.** release-plz bumps an additive `feat` on a 0.x crate as a *patch*; only a breaking change yields the *minor* bump these crates need (`0.1.5 → 0.2.0`, `0.1.0 → 0.2.0`). Task 3's commit — the one that changes `SessionProvider::session` — MUST use a `!` in the type (`feat(runtime)!:`) and carry a `BREAKING CHANGE:` footer in the body.
- **Every new public item needs a `///` doc comment.** The workspace sets `missing_docs = "warn"` and the CI docs job runs `RUSTDOCFLAGS="-D warnings"`, so an undocumented `pub` item fails a required check.
- **Run cargo commands synchronously in the foreground.** Do not offload `cargo test` / `cargo build` to a background monitor and end your turn — finish the task or report the blocker.
- **Per-task verification is `cargo test -p <crate>`; the final gate is `cargo test --workspace --all-features`.** Per-crate runs miss the conformance suite entirely.
- **Parity is the invariant.** No task may leave `runtime-axum` and `runtime-actix` behaviourally different. If you cannot make a change work in both, stop and report rather than landing half.

## File Structure

| File | Responsibility | Tasks |
|---|---|---|
| `tests/runtime-http-conformance/src/lib.rs` | Shared agent fixtures for both runtimes | 1 |
| `tests/runtime-http-conformance/Cargo.toml` | Adds the `tokio-tungstenite` dev-dep | 1 |
| `tests/runtime-http-conformance/tests/parity.rs` | All cross-runtime assertions | 1–6 |
| `crates/paigasus-helikon-runtime-{axum,actix}/src/error.rs` | 5xx redaction + `Retry-After` | 2 |
| `…/src/registry.rs` | Synthetic frame text; run principal; in-flight counter, cap, reclamation | 2, 4, 5 |
| `…/src/handlers/runs.rs` | Principal + session resolution, 403 gate, start-error logging | 2, 3, 4, 5 |
| `…/src/handlers/events.rs` | WS principal authorisation; actix upgrade-error reclassification | 2, 4 |
| `…/src/auth.rs` | `Principal` newtype | 3 |
| `…/src/session.rs` | `SessionKey`, tuple keying, `SessionProvider` trait | 3 |
| `…/src/server.rs` | Builder knobs, `AppStateInner` fields, `build()` guards | 3, 5 |
| `…/src/lib.rs` | Public re-exports | 3 |
| `…/src/handlers/openapi.rs` | Documented response codes | 6 |
| `…/README.md`, `docs/book/src/concepts/axum-server.md` | User-facing docs + migration guide | 7 |

---

### Task 1: Conformance fixtures — failing and hanging agents

The shared fixture set is one always-succeeding agent, so neither the redaction nor the cap is reachable from the parity suite. Give `ScriptedAgent` a behaviour discriminant and add two agents.

**Files:**
- Modify: `tests/runtime-http-conformance/src/lib.rs:24-84`
- Modify: `tests/runtime-http-conformance/Cargo.toml:20-30`
- Test: `tests/runtime-http-conformance/tests/parity.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `scripted_agents() -> Vec<Arc<dyn Agent<()>>>` now returns three agents named `echo`, `boom`, `hang`. `boom` fails to start with `AgentError::MaxTurnsExceeded(1)`, which `TokioRunner::run_streamed` wraps into `RunError::Agent`, so the server's `start_error` string is exactly `"agent failed: max turns (1) exceeded"`. `hang` never terminates.

- [ ] **Step 1: Write the failing test**

Append to `tests/runtime-http-conformance/tests/parity.rs`:

```rust
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
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test -p paigasus-helikon-runtime-http-conformance fixture_set_exposes_all_three_agents`
Expected: FAIL — `axum /agents is missing 'boom'; got ["echo"]`

- [ ] **Step 3: Add the behaviour discriminant and the two agents**

Replace `tests/runtime-http-conformance/src/lib.rs:22-84` (the `ScriptedAgent` struct, its `Agent` impl, and `scripted_agents`) with:

```rust
/// What a [`ScriptedAgent`] does when run.
///
/// Three behaviours are needed because the parity suite must reach three
/// server code paths: the normal terminal path, the run-start error path
/// (redaction), and the never-terminates path (in-flight cap).
enum Behaviour {
    /// Emit a fixed event sequence, then end the stream.
    Script(Vec<AgentEvent>),
    /// Fail before emitting anything at all.
    FailToStart,
    /// Never terminate; the runner's cancel token is the only way out.
    Hang,
}

/// A deterministic [`Agent`] that replays a fixed behaviour instead of talking
/// to a real model, so every run is byte-reproducible.
struct ScriptedAgent {
    /// Agent name returned by [`Agent::name`].
    name: String,
    /// Human-readable description returned by [`Agent::description`].
    description: String,
    /// What this agent does on each [`Agent::run`] call.
    behaviour: Behaviour,
}

#[async_trait]
impl Agent<()> for ScriptedAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    async fn run(
        &self,
        _ctx: RunContext<()>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        match &self.behaviour {
            Behaviour::Script(events) => Ok(stream::iter(events.clone()).boxed()),
            // `TokioRunner::run_streamed` does `agent.run(..).await?`, and
            // `RunError: From<AgentError>`, so the server records a
            // `start_error` of exactly "agent failed: max turns (1) exceeded".
            // The redaction assertions grep for that substring's absence.
            Behaviour::FailToStart => Err(AgentError::MaxTurnsExceeded(1)),
            // `TokioRunner::controlled` selects on the cancel token, so the
            // agent itself need not handle cancellation.
            Behaviour::Hang => Ok(stream::pending().boxed()),
        }
    }
}

/// The shared agent set mounted on both runtimes by the parity suite.
///
/// - `echo` — emits one assistant [`AgentEvent::MessageOutput`] carrying the
///   text `"echo"` followed by a terminal [`AgentEvent::RunCompleted`]. The
///   events carry no per-run identifiers, so bodies differ between runtimes
///   only in the injected `run_id`, which the parity test normalizes.
/// - `boom` — fails before emitting any event, exercising the redacted 500 and
///   the redacted synthetic SSE/WebSocket terminal frames.
/// - `hang` — never produces a terminal event, so a run of it holds an
///   in-flight slot until cancelled. Used to drive the admission cap.
///
/// The return type is `Vec<Arc<dyn Agent<()>>>` so the same values can be handed
/// to both `paigasus_helikon_runtime_axum::AgentServer::<()>` and
/// `paigasus_helikon_runtime_actix::AgentServer::<()>`.
pub fn scripted_agents() -> Vec<Arc<dyn Agent<()>>> {
    vec![
        Arc::new(ScriptedAgent {
            name: "echo".to_owned(),
            description: "scripted echo agent".to_owned(),
            behaviour: Behaviour::Script(vec![
                AgentEvent::MessageOutput {
                    item: Item::AssistantMessage {
                        content: vec![ContentPart::Text {
                            text: "echo".to_owned(),
                        }],
                        agent: None,
                    },
                },
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                },
            ]),
        }),
        Arc::new(ScriptedAgent {
            name: "boom".to_owned(),
            description: "scripted agent that fails to start".to_owned(),
            behaviour: Behaviour::FailToStart,
        }),
        Arc::new(ScriptedAgent {
            name: "hang".to_owned(),
            description: "scripted agent that never terminates".to_owned(),
            behaviour: Behaviour::Hang,
        }),
    ]
}
```

- [ ] **Step 4: Add the WebSocket dev-dependency**

In `tests/runtime-http-conformance/Cargo.toml`, add to `[dev-dependencies]` (Task 4 and the §4 assertion need it; it is already in `[workspace.dependencies]` and already used by both runtimes' own `tests/ws.rs`, so this adds no new third-party pin):

```toml
tokio-tungstenite  = { workspace = true }
```

- [ ] **Step 5: Run the whole conformance suite**

Run: `cargo test -p paigasus-helikon-runtime-http-conformance`
Expected: PASS — the new test passes and every pre-existing assertion still passes. The existing `GET /agents` check is set-equality across runtimes, so adding agents to the shared set changes both sides identically and it continues to hold.

- [ ] **Step 6: Commit**

```bash
git add tests/runtime-http-conformance/src/lib.rs \
        tests/runtime-http-conformance/Cargo.toml \
        tests/runtime-http-conformance/tests/parity.rs
git commit -m "test(runtime): SMA-482 add failing and hanging conformance fixtures"
```

---

### Task 2: Redact every 5xx body and both synthetic terminal frames

Close CWE-209 on all three paths it escapes: the 500 response body, the synthetic SSE frame, and the synthetic WebSocket frame. The detail goes to `tracing` at `error` level, logged once at the source.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-axum/src/error.rs:85-107`
- Modify: `crates/paigasus-helikon-runtime-actix/src/error.rs:98-103`
- Modify: `crates/paigasus-helikon-runtime-axum/src/registry.rs:43-59`
- Modify: `crates/paigasus-helikon-runtime-actix/src/registry.rs:42-58`
- Modify: `crates/paigasus-helikon-runtime-axum/src/handlers/runs.rs:309-318`
- Modify: `crates/paigasus-helikon-runtime-actix/src/handlers/runs.rs` (the matching `spawn_writer` `Err` branch)
- Modify: `crates/paigasus-helikon-runtime-actix/src/handlers/events.rs:72`
- Test: `tests/runtime-http-conformance/tests/parity.rs`

**Interfaces:**
- Consumes: `scripted_agents()`'s `boom` agent from Task 1.
- Produces: the exact wire strings later tasks assert against — a 500 body of `{"error":"internal error"}`, a 503 body of `{"error":"service unavailable"}` carrying `Retry-After: 1`, and a synthetic terminal frame whose `error` field is `"run failed to start"` (start-error case) or `"run ended before producing a terminal event"` (terminal-less case).

- [ ] **Step 1: Write the failing conformance test**

Append to `tests/runtime-http-conformance/tests/parity.rs`:

```rust
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
        assert!(!body.contains(LEAK), "{name} 500 body leaked `{LEAK}`: {body}");
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
        assert!(!text.contains(LEAK), "{name} SSE body leaked `{LEAK}`: {text}");
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
        assert!(!frame.contains(LEAK), "{name} ws frame leaked `{LEAK}`: {frame}");
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
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test -p paigasus-helikon-runtime-http-conformance start_error_detail_is_redacted_on_every_transport`
Expected: FAIL — the 500 body is `{"error":"run start failed: agent failed: max turns (1) exceeded"}`, not the fixed string.

- [ ] **Step 3: Redact the axum response bodies**

In `crates/paigasus-helikon-runtime-axum/src/error.rs`, add these constants above `impl IntoResponse for ServerError`:

```rust
/// Body text returned for every HTTP 500.
///
/// Deliberately non-diagnostic: the underlying error is recorded via `tracing`
/// at `error` level instead, so an external caller learns nothing about the
/// server's internals (CWE-209).
const PUBLIC_INTERNAL_ERROR: &str = "internal error";

/// Body text returned for every HTTP 503, redacted for the same reason.
const PUBLIC_UNAVAILABLE: &str = "service unavailable";
```

Then replace the body of `into_response` (keeping the existing `status` match exactly as it is) with:

```rust
    fn into_response(self) -> Response {
        let status = /* … the existing match, unchanged … */;

        // Every 5xx renders a fixed public string; the detail goes to `tracing`.
        // 4xx variants stay detailed — they describe what the caller sent, which
        // the caller already knows.
        let public: Option<&'static str> = match &self {
            ServerError::RunStart(_) | ServerError::Internal(_) => {
                tracing::error!(error = %self, "internal server error");
                Some(PUBLIC_INTERNAL_ERROR)
            }
            ServerError::Unavailable(_) => {
                tracing::error!(error = %self, "service unavailable");
                Some(PUBLIC_UNAVAILABLE)
            }
            _ => None,
        };

        let body = ErrorBody {
            error: public.map_or_else(|| self.to_string(), str::to_owned),
        };

        let mut response = (status, Json(body)).into_response();
        if status == StatusCode::SERVICE_UNAVAILABLE {
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_static("1"),
            );
        }
        response
    }
```

- [ ] **Step 4: Redact the actix response bodies**

In `crates/paigasus-helikon-runtime-actix/src/error.rs`, add the identical two constants, then replace `error_response`:

```rust
    fn error_response(&self) -> HttpResponse {
        let status = self.status_code();
        let public: Option<&'static str> = match self {
            ServerError::RunStart(_) | ServerError::Internal(_) => {
                tracing::error!(error = %self, "internal server error");
                Some(PUBLIC_INTERNAL_ERROR)
            }
            ServerError::Unavailable(_) => {
                tracing::error!(error = %self, "service unavailable");
                Some(PUBLIC_UNAVAILABLE)
            }
            _ => None,
        };

        let body = ErrorBody {
            error: public.map_or_else(|| self.to_string(), str::to_owned),
        };

        let mut builder = HttpResponse::build(status);
        if status == StatusCode::SERVICE_UNAVAILABLE {
            builder.insert_header((actix_web::http::header::RETRY_AFTER, "1"));
        }
        builder.json(body)
    }
```

- [ ] **Step 5: Redact the synthetic terminal frame in both registries**

In **both** `crates/paigasus-helikon-runtime-axum/src/registry.rs` and `crates/paigasus-helikon-runtime-actix/src/registry.rs`, add above `impl RunHandle`:

```rust
/// Public `error` text for a run that failed before emitting any event.
///
/// The detailed cause is logged once by the writer task; putting it in the
/// frame would leak it to every SSE and WebSocket subscriber (CWE-209).
const PUBLIC_RUN_FAILED_TO_START: &str = "run failed to start";

/// Public `error` text for a stream that ended without a terminal event.
const PUBLIC_RUN_NO_TERMINAL: &str = "run ended before producing a terminal event";
```

and replace `synthetic_terminal_frame` with:

```rust
    pub(crate) fn synthetic_terminal_frame(&self, saw_terminal: bool) -> Option<AgentEvent> {
        if saw_terminal {
            return None;
        }
        let failed_to_start = self
            .start_error
            .lock()
            .expect("start_error mutex poisoned")
            .is_some();
        let error = if failed_to_start {
            PUBLIC_RUN_FAILED_TO_START
        } else {
            PUBLIC_RUN_NO_TERMINAL
        };
        // Note this logs the PUBLIC string, not the detail. The detail is logged
        // once by the writer task; this method runs once per subscriber, so
        // logging it here would duplicate it per subscriber and skip it entirely
        // for an unwatched run.
        tracing::warn!(
            agent = %self.agent_name,
            %error,
            "run ended without a real terminal event; synthesizing a RunFailed frame for the stream subscriber"
        );
        Some(AgentEvent::RunFailed {
            error: error.to_owned(),
        })
    }
```

- [ ] **Step 6: Log the detail once, at the source, in both runtimes**

In **both** `handlers/runs.rs`, in `spawn_writer`'s `Err(e)` branch, add the log above the assignment:

```rust
            Err(e) => {
                // The run failed to *start* (no events were ever emitted). Log the
                // detailed cause exactly once — the wire-facing frame and the 500
                // body are both redacted, so this is the only place it survives.
                tracing::error!(
                    agent = %handle.agent_name,
                    %run_id,
                    error = %e,
                    "run failed to start"
                );
                *handle
                    .start_error
                    .lock()
                    .expect("start_error mutex poisoned") = Some(e.to_string());
            }
```

- [ ] **Step 7: Reclassify actix's malformed-upgrade error**

At `crates/paigasus-helikon-runtime-actix/src/handlers/events.rs:72`, a failed `actix_ws::handle` is a **client** error — a malformed upgrade request. Leaving it as `Internal` means any caller can drive unbounded `error!`-level log output now that 500s are logged. Change:

```rust
    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, body)
        .map_err(|e| ServerError::BadRequest(format!("invalid websocket upgrade: {e}")))?;
```

and update the `# Errors` doc block above the handler (`events.rs:44-49`): replace the `ServerError::Internal (500) — the WebSocket upgrade handshake failed` line with:

```rust
/// - [`ServerError::BadRequest`] (400) — `id` is not a valid UUID, or the request
///   was not a valid WebSocket upgrade.
```

(axum needs no equivalent: its `WebSocketUpgrade` extractor rejects malformed upgrades with its own response before the handler body runs.)

- [ ] **Step 8: Run both crates' own suites and fix the assertions this breaks**

Run: `cargo test -p paigasus-helikon-runtime-axum -p paigasus-helikon-runtime-actix`
Expected: FAIL in a small number of pre-existing tests that assert on the old detailed bodies or the old synthetic-frame text — in particular `synthetic_terminal_frame_branches` in both `registry.rs` (it asserts the frame carries the raw `"boom"` text) and any body assertion in `tests/runs.rs` / `tests/ws.rs`.

Update each to the new expectation. For `synthetic_terminal_frame_branches`, the start-error arm becomes:

```rust
        *h.start_error.lock().unwrap() = Some("boom".to_owned());
        match h.synthetic_terminal_frame(false) {
            // Redacted: the detail lives in the log, not the frame.
            Some(AgentEvent::RunFailed { error }) => {
                assert_eq!(error, "run failed to start");
                assert!(!error.contains("boom"));
            }
            other => panic!("expected redacted RunFailed, got {other:?}"),
        }
```

Do **not** weaken an assertion to make it pass — if a test now fails for a reason other than the intended redaction, that is a real regression.

- [ ] **Step 9: Add per-crate redaction unit tests**

In **both** `src/error.rs` test modules, add:

```rust
    /// Every 5xx body is a fixed public string; every 4xx body keeps its detail.
    #[test]
    fn five_hundreds_are_redacted_four_hundreds_are_not() {
        let body_of = |e: ServerError| -> String {
            // axum: read the serialized body. actix: use `error_response()`.
            // See the sibling runtime for the framework-specific form.
            serde_json::to_string(&ErrorBody {
                error: e.to_string(),
            })
            .unwrap()
        };
        let _ = body_of; // replaced below by the framework-specific assertion

        assert_eq!(
            render_body(ServerError::Internal("secret detail".into())),
            r#"{"error":"internal error"}"#
        );
        assert_eq!(
            render_body(ServerError::RunStart("secret detail".into())),
            r#"{"error":"internal error"}"#
        );
        assert_eq!(
            render_body(ServerError::Unavailable("pool at postgres://u:pw@h".into())),
            r#"{"error":"service unavailable"}"#
        );
        assert_eq!(
            render_body(ServerError::BadRequest("bad selector `x`".into())),
            r#"{"error":"bad request: bad selector `x`"}"#
        );
        assert_eq!(
            render_body(ServerError::UnknownAgent("nope".into())),
            r#"{"error":"unknown agent: nope"}"#
        );
    }
```

Write the `render_body` helper for each framework in that crate's test module — axum via `into_response()` plus `axum::body::to_bytes`, actix via `error_response()` plus `actix_web::body::to_bytes`. Add one more assertion per crate that the 503 response carries `Retry-After: 1` and the 500 does not.

- [ ] **Step 10: Run everything**

Run: `cargo test -p paigasus-helikon-runtime-axum -p paigasus-helikon-runtime-actix -p paigasus-helikon-runtime-http-conformance`
Expected: PASS, including `start_error_detail_is_redacted_on_every_transport`.

- [ ] **Step 11: Commit**

```bash
git add crates/paigasus-helikon-runtime-axum/src \
        crates/paigasus-helikon-runtime-actix/src \
        tests/runtime-http-conformance/tests/parity.rs
git commit -m "fix(runtime): SMA-482 redact internal detail from 5xx bodies and synthetic frames"
```

---

### Task 3: Bind the session id to the authenticated principal

Close CWE-639. This is the breaking task: `SessionProvider::session` changes signature.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-{axum,actix}/src/auth.rs` (add `Principal`)
- Modify: `crates/paigasus-helikon-runtime-{axum,actix}/src/session.rs` (`SessionKey`, tuple keys, trait)
- Modify: `crates/paigasus-helikon-runtime-{axum,actix}/src/server.rs` (builder + state)
- Modify: `crates/paigasus-helikon-runtime-{axum,actix}/src/handlers/runs.rs` (resolution + 403)
- Modify: `crates/paigasus-helikon-runtime-{axum,actix}/src/lib.rs` (exports)
- Test: `tests/runtime-http-conformance/tests/parity.rs`, both crates' `tests/`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `pub struct Principal(pub String)`; `pub struct SessionKey<'a>` with `new(Option<&'a str>, Option<&'a str>) -> Self` and `storage_key(&self) -> Option<String>`; `SessionProvider::session(&self, key: SessionKey<'_>)`; `AgentServerBuilder::require_principal(bool)` and `::allow_unbound_sessions()`; `AppStateInner::require_principal: bool`. Task 4 consumes the `principal: Option<String>` local that `create_run` now resolves.

- [ ] **Step 1: Write the failing conformance test**

Append to `tests/runtime-http-conformance/tests/parity.rs`. Note this needs an auth-configured server pair, so add these two boot helpers alongside the existing ones:

```rust
/// An `AuthLayer` for the parity suite that maps the `X-Test-Principal` header
/// to a `Principal`. A request with no such header is admitted but establishes
/// no principal — which is exactly the fail-closed row under test.
mod principal_auth {
    use async_trait::async_trait;

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

/// Boot an axum server with the header-principal auth layer. Mirrors
/// `boot_axum` otherwise.
async fn boot_axum_authed() -> String {
    let mut builder = paigasus_helikon_runtime_axum::AgentServer::<()>::builder()
        .with_default_context()
        .auth(std::sync::Arc::new(principal_auth::HeaderPrincipalAuth));
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
/// `boot_actix` otherwise, including its dedicated-thread `System`.
fn boot_actix_authed() -> String {
    let mut builder = paigasus_helikon_runtime_actix::AgentServer::<()>::builder()
        .with_default_context()
        .auth(std::sync::Arc::new(principal_auth::ActixHeaderPrincipalAuth));
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
        assert_eq!(resp.status(), 403, "{name} unbound named session must be 403");
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_session_id_different_principals_are_isolated() {
    let axum_base = boot_axum_authed().await;
    let actix_base = boot_actix_authed();
    let client = reqwest::Client::new();

    for (name, base) in [("axum", &axum_base), ("actix", &actix_base)] {
        // Principal "alice" runs once under session id "shared".
        let alice = client
            .post(format!("{base}/agents/echo/runs"))
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
        // resumes alice's session and the histories are the same object.
        let mallory = client
            .post(format!("{base}/agents/echo/runs"))
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
        assert!(
            !serde_json::to_string(&mallory_run)
                .expect("re-serialize")
                .contains("alice-secret"),
            "{name}: mallory's response leaked alice's input — sessions collided"
        );
    }
}
```

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-http-conformance named_session_without_principal_is_refused_identically same_session_id_different_principals_are_isolated`
Expected: FAIL to **compile** — `Principal` does not exist yet. That is the correct failure for this step.

- [ ] **Step 3: Add `Principal` to both `auth.rs` files**

Append to **both** `crates/paigasus-helikon-runtime-{axum,actix}/src/auth.rs`:

```rust
/// A stable identity for the authenticated caller.
///
/// An [`AuthLayer`] establishes it by inserting the value into the request's
/// extensions. The server then scopes every session the caller reaches to that
/// identity, so two callers can no longer collide on one `X-Session-Id`
/// (CWE-639).
///
/// A server built with an [`AuthLayer`] but whose layer never inserts a
/// `Principal` refuses any request carrying `X-Session-Id` with `403 Forbidden`
/// — see [`AgentServerBuilder::require_principal`](crate::AgentServerBuilder::require_principal).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Principal(pub String);
```

Also extend the `# Identity handoff` doc section in both files to name `Principal` as the specific type the server looks for.

- [ ] **Step 4: Add `SessionKey` and re-key the session store, in both `session.rs` files**

Replace the `SessionProvider` trait and `InMemorySessionProvider`/`SessionLocks` internals. The code below is identical in both crates:

```rust
/// The compound identity a session is resolved under.
///
/// # Security
///
/// A [`SessionProvider`] that keys on [`id`](SessionKey::id) **alone** remains
/// vulnerable to CWE-639: any admitted caller who learns or guesses another
/// caller's id reaches their conversation. Key on
/// [`storage_key`](SessionKey::storage_key), or on both fields together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct SessionKey<'a> {
    /// The authenticated principal, when one was established.
    pub principal: Option<&'a str>,
    /// The caller-supplied `X-Session-Id`, when present.
    pub id: Option<&'a str>,
}

impl<'a> SessionKey<'a> {
    /// Construct a key.
    ///
    /// Required because the struct is `#[non_exhaustive]`, so external crates
    /// cannot build one with a struct literal. Being `#[non_exhaustive]` is what
    /// lets a future third component be added without another breaking change.
    pub fn new(principal: Option<&'a str>, id: Option<&'a str>) -> Self {
        Self { principal, id }
    }

    /// A collision-free single-string key, for providers whose backend needs one
    /// (Postgres, Redis, a filesystem path).
    ///
    /// Returns `None` for an anonymous request (`id` is `None`), which must not
    /// be stored at all.
    ///
    /// The principal is length-prefixed so that no two distinct
    /// `(principal, id)` pairs can produce the same string. A plain
    /// `format!("{principal}:{id}")` would let `("a:b", "c")` and
    /// `("a", "b:c")` collide, reintroducing the very IDOR this type exists to
    /// close.
    pub fn storage_key(&self) -> Option<String> {
        let id = self.id?;
        let principal = self.principal.unwrap_or("");
        Some(format!("{}:{}:{}", principal.len(), principal, id))
    }
}

/// Maps a [`SessionKey`] to a [`Session`] object.
///
/// - `key.id == Some(_)` — return the existing session for that key, creating
///   one on the first call. Two calls with an equal key must return `Arc`s that
///   are pointer-equal (`Arc::ptr_eq`).
/// - `key.id == None` — return a fresh, anonymous session that is *not* stored
///   and is never pointer-equal to any other session.
///
/// # Security — key on the principal, not just the id
///
/// `key.id` comes straight from the request's `X-Session-Id` header, so it is
/// attacker-chosen. **A provider that uses it as its sole lookup key lets any
/// admitted caller who learns or guesses another caller's id read and append to
/// that conversation (CWE-639).** Use
/// [`SessionKey::storage_key`](SessionKey::storage_key), which combines both
/// components unambiguously.
#[async_trait]
pub trait SessionProvider: Send + Sync {
    /// Look up or create the session for `key`.
    async fn session(&self, key: SessionKey<'_>) -> Result<Arc<dyn Session>, ServerError>;
}

/// Owned form of the compound key: `(principal, id)`.
///
/// A tuple, deliberately. Concatenating the two components into one string
/// would let `("a:b", "c")` and `("a", "b:c")` collide; a tuple has no encoding
/// to get wrong.
type OwnedKey = (Option<String>, String);
```

Update `InMemoryInner` to `map: HashMap<OwnedKey, Arc<dyn Session>>` / `order: VecDeque<OwnedKey>`, and replace the `session` impl:

```rust
#[async_trait]
impl SessionProvider for InMemorySessionProvider {
    async fn session(&self, key: SessionKey<'_>) -> Result<Arc<dyn Session>, ServerError> {
        let Some(id) = key.id else {
            // Anonymous: fresh session, never stored, regardless of principal.
            return Ok(Arc::new(MemorySession::new()) as Arc<dyn Session>);
        };
        let owned: OwnedKey = (key.principal.map(str::to_owned), id.to_owned());

        // Fast path: read lock.
        {
            let inner = self.inner.read().await;
            if let Some(arc) = inner.map.get(&owned) {
                return Ok(Arc::clone(arc));
            }
        }

        // Slow path: write lock — insert and possibly evict.
        let mut inner = self.inner.write().await;
        if let Some(arc) = inner.map.get(&owned) {
            return Ok(Arc::clone(arc));
        }

        let session: Arc<dyn Session> = Arc::new(MemorySession::new());
        inner.map.insert(owned.clone(), Arc::clone(&session));
        inner.order.push_back(owned);

        if inner.map.len() > self.max_sessions {
            if let Some(oldest) = inner.order.pop_front() {
                inner.map.remove(&oldest);
            }
        }

        Ok(session)
    }
}
```

Add to the `InMemorySessionProvider` struct docs, under `# Security`, the known limitation:

```rust
/// **Known limitation.** `max_sessions` is a single global FIFO bound, so one
/// principal creating `max_sessions` distinct ids evicts every other
/// principal's session, silently resetting their conversations. This is a
/// cross-tenant availability concern, not a disclosure one — the compound key
/// still prevents any caller from *reading* another's session.
```

Re-key `SessionLocks` the same way:

```rust
    /// Return the per-session lock for `key`.
    ///
    /// Keyed on the SAME compound identity as the session store, and not
    /// optionally so: if the lock map kept keying on the bare id, two principals
    /// using one id would serialise against each other — a cross-tenant stall
    /// and a timing oracle on the other principal's traffic.
    pub(crate) fn lock_for(&self, key: SessionKey<'_>) -> Arc<tokio::sync::Mutex<()>> {
        let Some(id) = key.id else {
            return Arc::new(tokio::sync::Mutex::new(()));
        };
        let owned: OwnedKey = (key.principal.map(str::to_owned), id.to_owned());

        let mut map = self.map.lock().expect("SessionLocks mutex poisoned");
        map.retain(|_, lock| Arc::strong_count(lock) > 1);
        Arc::clone(
            map.entry(owned)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }
```

with `map: std::sync::Mutex<HashMap<OwnedKey, Arc<tokio::sync::Mutex<()>>>>`.

- [ ] **Step 5: Add the builder knobs and state field, in both `server.rs` files**

Add to `AgentServerBuilder`: `require_principal: Option<bool>,` initialised to `None` in `new()`. Add to `AppStateInner`: `pub require_principal: bool,`. Add the two setters:

```rust
    /// Require an authenticated [`Principal`](crate::Principal) before honouring
    /// an `X-Session-Id` header.
    ///
    /// When enabled, a request that carries `X-Session-Id` but for which no
    /// `Principal` was established is rejected with `403 Forbidden`, because it
    /// would otherwise land in a namespace shared with every other
    /// principal-less caller (CWE-639).
    ///
    /// **Default:** enabled exactly when an [`AuthLayer`] is configured. Set it
    /// explicitly to `true` when the server is *embedded* in a host application
    /// that authenticates for it — via [`AgentServer::router`] — since no
    /// `AuthLayer` is configured on this builder in that topology and the
    /// default would leave the check off.
    pub fn require_principal(mut self, required: bool) -> Self {
        self.require_principal = Some(required);
        self
    }

    /// Permit `X-Session-Id` from callers with no established principal.
    ///
    /// Equivalent to `require_principal(false)`. Appropriate for a single-tenant
    /// service or a shared-API-key deployment that genuinely wants one shared
    /// session namespace.
    ///
    /// This suppresses the 403 **and nothing else**: the session key stays
    /// compound, so a caller that *does* carry a `Principal` is still isolated
    /// to it.
    pub fn allow_unbound_sessions(mut self) -> Self {
        self.require_principal = Some(false);
        self
    }
```

In `build()`, before constructing `AppState`:

```rust
        // Default the gate to "on whenever this builder authenticates". An
        // embedded deployment whose host authenticates must opt in explicitly.
        let require_principal = self.require_principal.unwrap_or(self.auth.is_some());
```

and pass it into `AppStateInner`.

- [ ] **Step 6: Resolve the principal and gate, in the axum handler**

In `crates/paigasus-helikon-runtime-axum/src/handlers/runs.rs`, replace step 3 (`crates/…/runs.rs:163-178`) with:

```rust
    // 3. Resolve the principal the auth layer established, and the session id.
    //    A present-but-non-UTF-8 header is a 400 rather than a silent `None`:
    //    coercing it to `None` would skip the fail-closed gate below.
    let principal: Option<String> = parts.extensions.get::<Principal>().map(|p| p.0.clone());

    let session_id: Option<String> = match parts.headers.get("x-session-id") {
        None => None,
        Some(value) => Some(
            value
                .to_str()
                .map_err(|_| {
                    ServerError::BadRequest(
                        "invalid `X-Session-Id` header: not valid UTF-8".to_owned(),
                    )
                })?
                .to_owned(),
        ),
    };

    // Fail closed: a named session with no principal would join the shared
    // principal-less namespace, which is exactly the IDOR this gate prevents.
    if state.require_principal && principal.is_none() && session_id.is_some() {
        return Err(ServerError::Unauthorized(AuthRejection {
            status: StatusCode::FORBIDDEN,
            message: "session id requires an authenticated principal".to_owned(),
        }));
    }

    let key = SessionKey::new(principal.as_deref(), session_id.as_deref());
    let session = state.sessions.session(key).await?;

    // 4. Acquire the per-session serialization lock BEFORE creating/spawning the
    //    run so that same-session requests queue. `SessionKey` is `Copy`, so the
    //    same value feeds both calls without a clone.
    let guard: OwnedMutexGuard<()> = state.locks.lock_for(key).lock_owned().await;
```

Add `AuthRejection`, `Principal`, and `SessionKey` to the file's `use crate::{…}` list.

- [ ] **Step 7: Do the same in the actix handler, with the `RefCell` scope**

In `crates/paigasus-helikon-runtime-actix/src/handlers/runs.rs`, the same change, except the principal read **must** be in an explicit scope:

```rust
    // 3. Resolve the principal the auth layer established, and the session id.
    //
    //    The `Ref` from `extensions()` MUST be dropped before any `.await`.
    //    actix request extensions are `RefCell`-backed and handler futures carry
    //    no `Send` bound, so holding it across an await compiles and then panics
    //    with "already mutably borrowed" the first time a `ContextProvider` or
    //    `AuthLayer` calls `extensions_mut()`.
    let principal: Option<String> = {
        use actix_web::HttpMessage as _;
        req.extensions().get::<Principal>().map(|p| p.0.clone())
    };
```

with the `session_id`, gate, `SessionKey::new`, and `lock_for` lines identical to axum's (reading the header from `req.headers()` rather than `parts.headers`).

- [ ] **Step 8: Export the new types from both `lib.rs` files**

```rust
mod session;
pub use session::{InMemorySessionProvider, SessionKey, SessionProvider};

mod auth;
pub use auth::{AuthLayer, Principal};
```

- [ ] **Step 9: Add per-crate unit and integration tests**

In **both** `src/session.rs` test modules, replace the existing session/lock tests with these (the old ones call the removed one-argument signature):

```rust
    /// The IDOR itself: one id, two principals, two different sessions.
    #[tokio::test]
    async fn same_id_different_principals_are_isolated() {
        let p = InMemorySessionProvider::new(16);
        let alice = p.session(SessionKey::new(Some("alice"), Some("s1"))).await.unwrap();
        let mallory = p.session(SessionKey::new(Some("mallory"), Some("s1"))).await.unwrap();
        assert!(!Arc::ptr_eq(&alice, &mallory));

        // Positive control: the affinity guarantee still holds within a principal.
        let alice_again = p.session(SessionKey::new(Some("alice"), Some("s1"))).await.unwrap();
        assert!(Arc::ptr_eq(&alice, &alice_again));
    }

    /// A naive `"{principal}:{id}"` key would make these two collide. A tuple
    /// key cannot, and neither can the length-prefixed `storage_key`.
    #[tokio::test]
    async fn concatenation_collision_is_impossible() {
        let p = InMemorySessionProvider::new(16);
        let a = p.session(SessionKey::new(Some("a:b"), Some("c"))).await.unwrap();
        let b = p.session(SessionKey::new(Some("a"), Some("b:c"))).await.unwrap();
        assert!(!Arc::ptr_eq(&a, &b), "keys collided");

        assert_ne!(
            SessionKey::new(Some("a:b"), Some("c")).storage_key(),
            SessionKey::new(Some("a"), Some("b:c")).storage_key(),
        );
        assert_eq!(SessionKey::new(Some("a"), None).storage_key(), None);
    }

    /// Anonymous requests are never stored and never shared, whatever the
    /// principal is.
    #[tokio::test]
    async fn anonymous_is_always_fresh() {
        let p = InMemorySessionProvider::new(16);
        let a = p.session(SessionKey::new(Some("alice"), None)).await.unwrap();
        let b = p.session(SessionKey::new(Some("alice"), None)).await.unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
    }

    /// Locks must be keyed on the same compound identity, or one principal can
    /// stall another by squatting a guessed id.
    ///
    /// Both `Arc`s are held simultaneously ON PURPOSE: `lock_for` prunes entries
    /// whose `Arc::strong_count` is 1, so dropping the first before taking the
    /// second would make this assertion hold even against a buggy bare-id
    /// implementation.
    #[test]
    fn locks_are_isolated_by_principal() {
        let locks = SessionLocks::new();
        let alice = locks.lock_for(SessionKey::new(Some("alice"), Some("s1")));
        let mallory = locks.lock_for(SessionKey::new(Some("mallory"), Some("s1")));
        assert!(!Arc::ptr_eq(&alice, &mallory));

        // Positive control, also with both held.
        let alice_again = locks.lock_for(SessionKey::new(Some("alice"), Some("s1")));
        assert!(Arc::ptr_eq(&alice, &alice_again));
        drop((alice, mallory, alice_again));
    }

    /// Eviction still respects `max_sessions` with compound keys.
    #[tokio::test]
    async fn bounded_map_evicts() {
        let p = InMemorySessionProvider::new(1);
        let _a = p.session(SessionKey::new(Some("alice"), Some("s1"))).await.unwrap();
        let _b = p.session(SessionKey::new(Some("alice"), Some("s2"))).await.unwrap();
        assert_eq!(p.len(), 1);
    }
```

Then add a new integration test file to **both** crates, `tests/principal.rs`, covering every row of the fail-closed matrix end to end: no auth configured → shared namespace, no 403; auth + principal + id → isolated; auth + no principal + id → 403; auth + no principal + no id → 200 with a fresh session; `allow_unbound_sessions()` → the same request is 200; **`allow_unbound_sessions()` with a principal present still isolates**; `require_principal(true)` with **no** `AuthLayer` → still 403; non-UTF-8 `X-Session-Id` → 400.

For **actix only**, add to `tests/principal.rs` a case whose `ContextProvider::build` calls `req.extensions_mut()`, proving the scoped `Ref` in Step 7 does not panic:

```rust
/// Guards the `RefCell` hazard: the handler reads `extensions()` to resolve the
/// principal, and a `ContextProvider` may legitimately call `extensions_mut()`.
/// If the handler's `Ref` were held across the await, this panics.
struct MutatingContextProvider;

#[async_trait(?Send)]
impl paigasus_helikon_runtime_actix::ContextProvider<()> for MutatingContextProvider {
    async fn build(
        &self,
        req: &actix_web::HttpRequest,
        session: std::sync::Arc<dyn paigasus_helikon_core::Session>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<paigasus_helikon_core::RunContext<()>, paigasus_helikon_actix_error_alias> {
        use actix_web::HttpMessage as _;
        req.extensions_mut().insert(MarkerInsertedByContext);
        Ok(paigasus_helikon_core::RunContext::ephemeral(())
            .with_session(session)
            .with_cancel(cancel))
    }
}
```

(Use the crate's real `ServerError` path for the error type and a local unit struct for the marker; the shape above is the point, not the exact names.)

- [ ] **Step 10: Fix every existing call site the signature change breaks**

Run: `cargo test -p paigasus-helikon-runtime-axum -p paigasus-helikon-runtime-actix 2>&1 | head -60`
Expected: compile errors at each `session(…)` / `lock_for(…)` call in the crates' own `tests/`.

Update each to `SessionKey::new(None, Some("…"))`. Then, in **both** crates' `tests/auth.rs`, any test that configures an `AuthLayer` **and** sends `X-Session-Id` will now get a 403 — that is correct new behaviour, not a regression. Fix each by either inserting a `Principal` in the mock auth layer or adding `.allow_unbound_sessions()` to that server's builder, whichever matches what the test is actually asserting.

- [ ] **Step 11: Run everything**

Run: `cargo test -p paigasus-helikon-runtime-axum -p paigasus-helikon-runtime-actix -p paigasus-helikon-runtime-http-conformance`
Expected: PASS.

- [ ] **Step 12: Commit — this is the breaking commit**

The `!` and the `BREAKING CHANGE:` footer are what make release-plz cut a *minor* bump. Without them these crates publish as `0.1.6` / `0.1.1` with a silently incompatible trait.

```bash
git add crates/paigasus-helikon-runtime-axum/src crates/paigasus-helikon-runtime-axum/tests \
        crates/paigasus-helikon-runtime-actix/src crates/paigasus-helikon-runtime-actix/tests \
        tests/runtime-http-conformance/tests/parity.rs
git commit -m "feat(runtime)!: SMA-482 bind session ids to the authenticated principal" -m "\
SessionProvider::session now takes a SessionKey<'_> carrying both the
authenticated principal and the caller-supplied X-Session-Id, closing an IDOR
(CWE-639) where any admitted caller who guessed another caller's session id
could read and append to that conversation.

BREAKING CHANGE: SessionProvider::session takes SessionKey<'_> instead of
Option<&str>. Use SessionKey::storage_key() for a single-string backend key;
reading key.id alone preserves the old behaviour AND the vulnerability. A
server with an AuthLayer now rejects X-Session-Id from callers with no
Principal (403); call allow_unbound_sessions() to opt out."
```

---

### Task 4: Authorise the WebSocket events endpoint against the principal

A run's event stream is readable by anyone holding its id. Now that `Principal` exists, gate it.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-{axum,actix}/src/registry.rs` (`RunHandle.principal`, `create` signature)
- Modify: `crates/paigasus-helikon-runtime-{axum,actix}/src/handlers/runs.rs` (pass the principal)
- Modify: `crates/paigasus-helikon-runtime-{axum,actix}/src/handlers/events.rs` (resolve + compare)
- Test: `tests/runtime-http-conformance/tests/parity.rs`

**Interfaces:**
- Consumes: `Principal` and the `principal: Option<String>` local from Task 3.
- Produces: `RunRegistry::create(&self, agent_name: String, principal: Option<String>, cancel: CancellationToken) -> (Uuid, Arc<RunHandle>)` (still infallible; Task 5 makes it fallible) and `RunHandle.principal: Option<String>`.

- [ ] **Step 1: Write the failing conformance test**

Append to `tests/runtime-http-conformance/tests/parity.rs`:

```rust
/// A run's event stream must be readable only by the principal that started it.
/// The denial is folded into the EXISTING 404 rather than a new 403: a distinct
/// status would confirm the run id exists and belongs to someone else, turning
/// the endpoint into an existence oracle for harvested ids.
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

        // mallory must not reach it — 404, and NOT an upgrade.
        let denied = client
            .get(format!("{base}/agents/echo/runs/{run_id}/events"))
            .header("x-test-principal", "mallory")
            .send()
            .await
            .unwrap_or_else(|e| panic!("{name} mallory events: {e}"));
        assert_eq!(
            denied.status(),
            404,
            "{name}: another principal's run must 404, not 403 (no existence oracle)"
        );
        denial_bodies.push(denied.text().await.expect("denial body"));

        // Positive control: alice CAN reach her own run, so the test cannot pass
        // by denying everyone.
        let ws_url =
            format!("{base}/agents/echo/runs/{run_id}/events").replacen("http", "ws", 1);
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
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test -p paigasus-helikon-runtime-http-conformance ws_events_are_scoped_to_the_owning_principal`
Expected: FAIL — `axum: another principal's run must 404, not 403` reported as `left: 101, right: 404`; mallory currently completes the upgrade.

- [ ] **Step 3: Store the owning principal on the run handle, in both registries**

Add the field to `RunHandle`:

```rust
    /// Principal that started this run; `None` for an unbound run.
    ///
    /// The WebSocket events endpoint compares against this so a run's stream is
    /// readable only by its owner.
    pub principal: Option<String>,
```

and thread it through `create`:

```rust
    /// Mint a new run id, build its handle, insert it into the registry, and
    /// return both.
    ///
    /// `principal` is the identity that started the run; the events endpoint
    /// uses it to scope subscriptions.
    pub fn create(
        &self,
        agent_name: String,
        principal: Option<String>,
        cancel: CancellationToken,
    ) -> (Uuid, Arc<RunHandle>) {
        let id = Uuid::new_v4();
        let handle = Arc::new(RunHandle {
            agent_name,
            principal,
            log: Arc::new(EventLog::new(self.max_events_per_run)),
            cancel,
            start_error: Mutex::new(None),
            terminal_at: Mutex::new(None),
        });
        let mut inner = self.inner.write().expect("RunRegistry RwLock poisoned");
        inner.runs.insert(id, Arc::clone(&handle));
        (id, handle)
    }
```

Update every `create(` call in each registry's own `#[cfg(test)] mod tests` to pass `None` as the new second argument.

- [ ] **Step 4: Pass the principal at the call site, in both `handlers/runs.rs`**

```rust
    let (run_id, handle) = state.registry.create(name, principal.clone(), cancel);
```

- [ ] **Step 5: Gate the axum events handler**

In `crates/paigasus-helikon-runtime-axum/src/handlers/events.rs`, add the extractor and the filter (keep `WebSocketUpgrade` last):

```rust
pub(crate) async fn events<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
    Path((name, id)): Path<(String, String)>,
    principal: Option<axum::Extension<Principal>>,
    ws: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    let run_id = Uuid::parse_str(&id)
        .map_err(|_| ServerError::BadRequest(format!("invalid run id: {id}")))?;

    let principal: Option<String> = principal.map(|axum::Extension(p)| p.0);

    // Absence, an agent-name mismatch, and a principal mismatch all return the
    // SAME 404. A distinct status for the last case would confirm the run exists
    // and belongs to someone else — an existence oracle for harvested run ids.
    let handle = state
        .registry
        .get(run_id)
        .filter(|h| h.agent_name == name)
        .filter(|h| h.principal.as_deref() == principal.as_deref())
        .ok_or_else(|| ServerError::UnknownAgent(format!("{name}/{id}")))?;

    Ok(ws.on_upgrade(move |socket| handle_socket(socket, handle)))
}
```

Add `crate::auth::Principal` to the imports, and extend the handler's `# Errors` doc to note that a run owned by a different principal is reported as `UnknownAgent` (404).

- [ ] **Step 6: Gate the actix events handler**

Same filter, with the scoped `Ref` (this read happens before the `actix_ws::handle` call, and the `Ref` must not survive into it):

```rust
    let principal: Option<String> = {
        use actix_web::HttpMessage as _;
        req.extensions().get::<Principal>().map(|p| p.0.clone())
    };

    let handle = state
        .registry
        .get(run_id)
        .filter(|h| h.agent_name == name)
        .filter(|h| h.principal.as_deref() == principal.as_deref())
        .ok_or_else(|| ServerError::UnknownAgent(format!("{name}/{id}")))?;
```

- [ ] **Step 7: Add per-crate tests**

Append to **both** crates' `tests/ws.rs`:

```rust
/// A run started by one principal is invisible to another — reported as a plain
/// 404, indistinguishable from a run id that never existed.
#[tokio::test]
async fn cross_principal_subscription_is_404() { /* … */ }

/// The owning principal can still subscribe, so the gate is not "deny all".
#[tokio::test]
async fn owning_principal_can_subscribe() { /* … */ }

/// With no principals anywhere (`None == None`), subscription still succeeds —
/// the single-tenant and development-server path is unchanged.
#[tokio::test]
async fn unbound_run_is_subscribable_without_a_principal() { /* … */ }

/// The agent-name mismatch check still returns 404 independently of principals.
#[tokio::test]
async fn agent_name_mismatch_is_still_404() { /* … */ }
```

Write each body following the existing `tests/ws.rs` patterns in that crate for spawning a server and connecting; the assertions are the ones named in each doc comment.

- [ ] **Step 8: Run everything**

Run: `cargo test -p paigasus-helikon-runtime-axum -p paigasus-helikon-runtime-actix -p paigasus-helikon-runtime-http-conformance`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/paigasus-helikon-runtime-axum/src crates/paigasus-helikon-runtime-axum/tests \
        crates/paigasus-helikon-runtime-actix/src crates/paigasus-helikon-runtime-actix/tests \
        tests/runtime-http-conformance/tests/parity.rs
git commit -m "fix(runtime): SMA-482 scope websocket event subscriptions to the owning principal"
```

---

### Task 5: Bound in-flight runs, with reclamation

Close CWE-770. The cap ships **with** `max_run_duration`, because a cap alone converts a memory-growth bug into a permanent outage: `sweep` never evicts non-terminal runs, `?mode=async` attaches no cancel guard, and `RunConfig::default().timeout` is `None`, so a wedged run holds its slot forever.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-{axum,actix}/src/registry.rs`
- Modify: `crates/paigasus-helikon-runtime-{axum,actix}/src/server.rs`
- Modify: `crates/paigasus-helikon-runtime-{axum,actix}/src/handlers/runs.rs` (one `?`)
- Test: `tests/runtime-http-conformance/tests/parity.rs`

**Interfaces:**
- Consumes: `RunRegistry::create` from Task 4; the `hang` fixture from Task 1.
- Produces: `RunRegistry::create(…) -> Result<(Uuid, Arc<RunHandle>), ServerError>`; `RunRegistry::new(ttl, max_runs, max_events_per_run, max_in_flight, max_run_duration)`; `AgentServerBuilder::max_in_flight(usize)` and `::max_run_duration(Duration)`.

- [ ] **Step 1: Write the failing conformance test**

Append to `tests/runtime-http-conformance/tests/parity.rs`. Note the dedicated single-purpose server pair:

```rust
/// Boot an axum server with a one-run admission cap. Single-purpose: the `hang`
/// run it admits holds its slot until `max_run_duration` elapses, so nothing
/// else should be asserted against this pair.
async fn boot_axum_capped() -> String {
    let mut builder = paigasus_helikon_runtime_axum::AgentServer::<()>::builder()
        .with_default_context()
        .max_in_flight(1);
    for agent in scripted_agents() {
        builder = builder.agent(agent);
    }
    let server = builder.build().expect("axum capped server builds");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        server.serve_with_listener(listener).await.expect("serve");
    });
    format!("http://{addr}")
}

/// Boot an actix server with a one-run admission cap. Same single-purpose
/// caveat as `boot_axum_capped`.
fn boot_actix_capped() -> String {
    let mut builder = paigasus_helikon_runtime_actix::AgentServer::<()>::builder()
        .with_default_context()
        .max_in_flight(1);
    for agent in scripted_agents() {
        builder = builder.agent(agent);
    }
    let server = builder.build().expect("actix capped server builds");
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

/// Once the in-flight cap is reached, further runs are refused with an
/// identical 503 on both runtimes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_flight_cap_rejects_identically() {
    let axum_base = boot_axum_capped().await;
    let actix_base = boot_actix_capped();
    let client = reqwest::Client::new();

    let mut bodies = Vec::new();
    for (name, base) in [("axum", &axum_base), ("actix", &actix_base)] {
        // Fill the single slot. `?mode=async` returns only AFTER `registry.create`
        // has run, so the sequencing below is race-free.
        let first = client
            .post(format!("{base}/agents/hang/runs?mode=async"))
            .header("content-type", "application/json")
            .header("x-session-id", "slot-holder")
            .body(r#"{"input":"hi"}"#)
            .send()
            .await
            .unwrap_or_else(|e| panic!("{name} first hang run: {e}"));
        assert_eq!(first.status(), 202, "{name} first run must be admitted");

        // A DIFFERENT session id, deliberately: same-session requests queue on
        // the per-session lock and would never reach the admission check, so the
        // test would pass for the wrong reason.
        let second = client
            .post(format!("{base}/agents/echo/runs?mode=async"))
            .header("content-type", "application/json")
            .header("x-session-id", "other-caller")
            .body(r#"{"input":"hi"}"#)
            .send()
            .await
            .unwrap_or_else(|e| panic!("{name} second run: {e}"));
        assert_eq!(second.status(), 503, "{name} second run must be refused");
        assert_eq!(
            second
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("1"),
            "{name} 503 must carry Retry-After"
        );
        let body = second.text().await.expect("503 body");
        assert_eq!(
            body, r#"{"error":"service unavailable"}"#,
            "{name} 503 body must be redacted — the reason goes to tracing, not \
             the wire, so it does not confirm to an attacker that the cap is finite"
        );
        bodies.push(body);
    }
    assert_eq!(bodies[0], bodies[1], "503 bodies must be byte-identical");
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test -p paigasus-helikon-runtime-http-conformance in_flight_cap_rejects_identically`
Expected: FAIL to compile — `max_in_flight` does not exist.

- [ ] **Step 3: Add the counter, the cap, and reclamation to both registries**

`RegistryInner` gains the counter, `RunRegistry` the two limits, `RunHandle` a creation stamp:

```rust
struct RegistryInner {
    /// All live and recently-completed runs, keyed by run id.
    runs: HashMap<Uuid, Arc<RunHandle>>,
    /// Insertion order of terminal runs (oldest → newest). Used for FIFO eviction.
    completion_order: VecDeque<Uuid>,
    /// Count of entries in `runs` whose `terminal_at` is `None`.
    ///
    /// Maintained rather than recomputed. Every mutation happens under the one
    /// `inner` write lock (`create` +1, `note_terminal` −1, `sweep` pass 0 −1)
    /// and `sweep` never removes a non-terminal run, so it cannot drift from the
    /// map. Scanning instead would hold the write lock while taking up to
    /// `max_runs + max_in_flight` mutexes, serialising against every concurrent
    /// `get`.
    live: usize,
}
```

Add to `RunHandle`:

```rust
    /// When the run was created. Used by the sweeper to reclaim a run that never
    /// reaches a terminal state.
    pub created_at: Instant,
```

`RunRegistry` gains `max_in_flight: usize` and `max_run_duration: Duration`; `new` takes both as trailing parameters and its doc comment gains a line for each. `create` becomes:

```rust
    /// Mint a new run id, build its handle, insert it into the registry, and
    /// return both.
    ///
    /// # Errors
    ///
    /// [`ServerError::Unavailable`] when admitting the run would exceed
    /// `max_in_flight`. The check and the insert share one critical section, so
    /// there is no window in which two callers both see room for the last slot.
    pub fn create(
        &self,
        agent_name: String,
        principal: Option<String>,
        cancel: CancellationToken,
    ) -> Result<(Uuid, Arc<RunHandle>), ServerError> {
        let mut inner = self.inner.write().expect("RunRegistry RwLock poisoned");
        if inner.live >= self.max_in_flight {
            // The only server-side signal that the cap is biting; the caller's
            // 503 body is redacted.
            tracing::warn!(
                live = inner.live,
                cap = self.max_in_flight,
                "rejecting run: in-flight limit reached"
            );
            return Err(ServerError::Unavailable(
                "in-flight run limit reached".to_owned(),
            ));
        }
        let id = Uuid::new_v4();
        let handle = Arc::new(RunHandle {
            agent_name,
            principal,
            created_at: Instant::now(),
            log: Arc::new(EventLog::new(self.max_events_per_run)),
            cancel,
            start_error: Mutex::new(None),
            terminal_at: Mutex::new(None),
        });
        inner.runs.insert(id, Arc::clone(&handle));
        inner.live += 1;
        Ok((id, handle))
    }
```

In `note_terminal`, decrement inside the existing `if t.is_none()` branch, right after `*t = Some(now)`:

```rust
        if t.is_none() {
            *t = Some(now);
            drop(t);
            inner.completion_order.push_back(id);
            inner.live -= 1;
        }
```

(the `drop(t)` releases the `terminal_at` guard before the `inner` mutations, keeping the borrow checker happy while preserving the `inner` → `terminal_at` lock order).

Add **pass 0** at the top of `sweep`, before the existing TTL pass:

```rust
        // Pass 0: reclaim runs that never terminated. Without this the in-flight
        // cap is a permanent-outage vector — `?mode=async` attaches no cancel
        // guard and `RunConfig::default().timeout` is `None`, so a wedged run
        // would hold its slot for the process lifetime.
        let overdue: Vec<Uuid> = inner
            .runs
            .iter()
            .filter(|(_, h)| {
                h.terminal_at
                    .lock()
                    .expect("terminal_at mutex poisoned")
                    .is_none()
                    && h.created_at
                        .checked_add(self.max_run_duration)
                        .is_some_and(|deadline| deadline <= now)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in overdue {
            let Some(handle) = inner.runs.get(&id).cloned() else {
                continue;
            };
            handle.cancel.cancel();
            let mut t = handle
                .terminal_at
                .lock()
                .expect("terminal_at mutex poisoned");
            if t.is_none() {
                *t = Some(now);
                drop(t);
                inner.completion_order.push_back(id);
                inner.live -= 1;
                tracing::warn!(%id, agent = %handle.agent_name,
                               "reclaiming run that exceeded max_run_duration");
            }
        }
```

Cancelling drives the writer task to finish, whose `TerminalGuard` calls `note_terminal` — already idempotent, so the second stamp is a no-op and `live` is decremented exactly once.

- [ ] **Step 4: Add the builder knobs, in both `server.rs` files**

Add `max_in_flight: usize` (default `1024`) and `max_run_duration: Duration` (default `Duration::from_secs(3600)`) to `AgentServerBuilder` and its `new()`, plus:

```rust
    /// Cap the number of simultaneously in-flight (non-terminal) runs.
    ///
    /// Once this many runs are live, further run creation is rejected with
    /// `503 Service Unavailable` until a run reaches a terminal state.
    ///
    /// Default: 1 024, matching
    /// [`max_retained_runs`](AgentServerBuilder::max_retained_runs).
    pub fn max_in_flight(mut self, max: usize) -> Self {
        self.max_in_flight = max;
        self
    }

    /// Maximum wall-clock lifetime of a single run.
    ///
    /// A run still live after this long is cancelled and marked terminal by the
    /// registry sweeper, releasing its in-flight slot. Without this a run that
    /// never terminates — a hung agent on a detached `?mode=async` request —
    /// would hold its slot for the process lifetime and eventually exhaust
    /// [`max_in_flight`](AgentServerBuilder::max_in_flight) permanently.
    ///
    /// Default: 1 hour.
    pub fn max_run_duration(mut self, duration: Duration) -> Self {
        self.max_run_duration = duration;
        self
    }
```

In `build()`, add the guard — **unconditional**, unlike the `max_sessions` guard above it, which is conditional only because a custom `SessionProvider` makes that field moot:

```rust
        // Unconditional: a zero cap would reject every run, and no custom
        // component can override it the way a session provider overrides
        // `max_sessions`.
        if self.max_in_flight == 0 {
            return Err(ServerError::BadRequest(
                "max_in_flight must be greater than 0".to_owned(),
            ));
        }
```

and pass both new values to `RunRegistry::new`.

- [ ] **Step 5: Propagate the fallible `create`, in both `handlers/runs.rs`**

```rust
    let (run_id, handle) = state.registry.create(name, principal.clone(), cancel)?;
```

Add a `ServerError::Unavailable` (503) line to the handler's `# Errors` doc block.

- [ ] **Step 6: Add per-crate registry tests**

Append to **both** registries' test modules:

```rust
    /// The cap admits exactly `max_in_flight` live runs and refuses the next.
    #[test]
    fn cap_admits_then_rejects() {
        let reg = RunRegistry::new(
            Duration::from_secs(60), 1024, 1024, 2, Duration::from_secs(3600),
        );
        let (_a, _ha) = reg.create("a".into(), None, CancellationToken::new()).unwrap();
        let (_b, _hb) = reg.create("a".into(), None, CancellationToken::new()).unwrap();
        assert!(reg.create("a".into(), None, CancellationToken::new()).is_err());
    }

    /// A terminal run frees its slot; a terminal-but-RETAINED run must not keep
    /// consuming one — that distinction is the entire point of the fix.
    #[test]
    fn terminal_runs_do_not_consume_slots() {
        let reg = RunRegistry::new(
            Duration::from_secs(3600), 1024, 1024, 1, Duration::from_secs(3600),
        );
        let (id, _h) = reg.create("a".into(), None, CancellationToken::new()).unwrap();
        assert!(reg.create("a".into(), None, CancellationToken::new()).is_err());

        reg.note_terminal(id, Instant::now());
        // Still retained (TTL is an hour), but no longer in flight.
        assert!(reg.get(id).is_some());
        assert!(reg.create("a".into(), None, CancellationToken::new()).is_ok());
    }

    /// A run that never terminates is reclaimed once it exceeds
    /// `max_run_duration`, and its slot is reusable.
    #[test]
    fn sweep_reclaims_overdue_runs() {
        let reg = RunRegistry::new(
            Duration::from_secs(3600), 1024, 1024, 1, Duration::from_secs(60),
        );
        let t0 = Instant::now();
        let (_id, handle) = reg.create("a".into(), None, CancellationToken::new()).unwrap();
        assert!(reg.create("a".into(), None, CancellationToken::new()).is_err());

        reg.sweep(t0 + Duration::from_secs(59));
        assert!(reg.create("a".into(), None, CancellationToken::new()).is_err());

        reg.sweep(t0 + Duration::from_secs(61));
        assert!(handle.cancel.is_cancelled(), "overdue run must be cancelled");
        assert!(reg.create("a".into(), None, CancellationToken::new()).is_ok());
    }
```

Add to **both** `server.rs` test modules (or the crates' `tests/server.rs`) a check that `build()` rejects `max_in_flight(0)` with `ServerError::BadRequest`.

- [ ] **Step 7: Run everything**

Run: `cargo test -p paigasus-helikon-runtime-axum -p paigasus-helikon-runtime-actix -p paigasus-helikon-runtime-http-conformance`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/paigasus-helikon-runtime-axum/src crates/paigasus-helikon-runtime-axum/tests \
        crates/paigasus-helikon-runtime-actix/src crates/paigasus-helikon-runtime-actix/tests \
        tests/runtime-http-conformance/tests/parity.rs
git commit -m "feat(runtime): SMA-482 bound in-flight runs with a reclaiming sweeper"
```

---

### Task 6: Document the new responses in OpenAPI

`/openapi.json` is a first-class, default-on surface. A cap whose failure mode is undocumented breaks client codegen, and the parity suite only checks path keys today.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-{axum,actix}/src/handlers/openapi.rs:51-61` and `:76-78`
- Test: `tests/runtime-http-conformance/tests/parity.rs`

**Interfaces:**
- Consumes: the 503 from Task 5 and the 403/404 semantics from Tasks 3 and 4.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Write the failing test**

Append to `tests/runtime-http-conformance/tests/parity.rs`:

```rust
/// The two runtimes must document the SAME response codes, not merely the same
/// paths. Path-only parity let the 503 go undocumented on both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openapi_response_sets_match() {
    let axum_base = boot_axum().await;
    let actix_base = boot_actix();
    let client = reqwest::Client::new();

    let fetch = |base: String| async move {
        let spec: serde_json::Value = reqwest::Client::new()
            .get(format!("{base}/openapi.json"))
            .send()
            .await
            .expect("openapi request")
            .json()
            .await
            .expect("openapi body");
        spec
    };
    let _ = &client;
    let axum_spec = fetch(axum_base).await;
    let actix_spec = fetch(actix_base).await;

    /// Collect `path -> method -> sorted status codes`.
    fn response_sets(spec: &serde_json::Value) -> Vec<(String, String, Vec<String>)> {
        let mut out = Vec::new();
        for (path, item) in spec["paths"].as_object().expect("paths object") {
            for (method, op) in item.as_object().expect("path item object") {
                let mut codes: Vec<String> = op["responses"]
                    .as_object()
                    .map(|r| r.keys().cloned().collect())
                    .unwrap_or_default();
                codes.sort();
                out.push((path.clone(), method.clone(), codes));
            }
        }
        out.sort();
        out
    }

    assert_eq!(
        response_sets(&axum_spec),
        response_sets(&actix_spec),
        "documented response codes must match between runtimes"
    );

    let run_codes = response_sets(&axum_spec)
        .into_iter()
        .find(|(p, m, _)| p == "/agents/{name}/runs" && m == "post")
        .map(|(_, _, c)| c)
        .expect("POST /agents/{name}/runs is documented");
    assert!(
        run_codes.contains(&"503".to_owned()),
        "the in-flight cap's 503 must be documented; got {run_codes:?}"
    );
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test -p paigasus-helikon-runtime-http-conformance openapi_response_sets_match`
Expected: FAIL — `the in-flight cap's 503 must be documented; got ["200", "202", "400", "401", "403", "404", "500"]`

- [ ] **Step 3: Update both `openapi.rs` files identically**

In the `POST /agents/{name}/runs` operation's `responses(...)` list, add the 503 and widen the 403:

```rust
        (status = 403, description = "Authenticated but not permitted, or an `X-Session-Id` was supplied without an authenticated principal"),
        …
        (status = 503, description = "In-flight run limit reached; retry after the `Retry-After` interval"),
```

In the events operation, widen the 404 (per Task 4 a cross-principal denial deliberately reuses 404 rather than adding a status code):

```rust
        (status = 404, description = "Run not found, owned by a different agent, or owned by a different principal"),
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p paigasus-helikon-runtime-http-conformance openapi_response_sets_match`
Expected: PASS.

- [ ] **Step 5: Verify the `openapi` feature is genuinely optional**

Run: `cargo build -p paigasus-helikon-runtime-axum --no-default-features`
Expected: builds clean. This is a required CI gate (`build-no-default-features`) and exists because `openapi.rs` is feature-gated.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-runtime-axum/src/handlers/openapi.rs \
        crates/paigasus-helikon-runtime-actix/src/handlers/openapi.rs \
        tests/runtime-http-conformance/tests/parity.rs
git commit -m "docs(runtime): SMA-482 document the 503 and widened 403/404 in openapi"
```

---

### Task 7: User-facing documentation and the migration guide

Both crate READMEs still carry the interim "the session id is caller-controlled" wording PR #173 added, which this change makes obsolete. The migration guide lives here, **not** in the CHANGELOGs — those are git-cliff output with a bare `## [Unreleased]` heading, and prose placed there is orphaned when release-plz inserts `## [0.2.0]` beneath it.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-axum/README.md:81-91`
- Modify: `crates/paigasus-helikon-runtime-actix/README.md:101-111`
- Modify: `docs/book/src/concepts/axum-server.md:76-78,95-96,104-113`

**Interfaces:** none — documentation only.

- [ ] **Step 1: Replace the interim security section in both READMEs**

Retitle `## Security: the session id is caller-controlled` to `## Security: sessions are scoped to the authenticated principal` and replace the body with: how to insert `Principal` from an `AuthLayer`; that a named session with no principal is refused with 403 by default; that `allow_unbound_sessions()` opts out and `require_principal(true)` opts *in* for embedded deployments; and that a run's WebSocket event stream is readable only by the principal that started it, with no administrative override.

- [ ] **Step 2: Add a migration section to both READMEs**

Add `## Migrating to 0.2` with these five points, using the exact wording for the first because it is the one that carries security weight:

- `SessionProvider::session` now takes `SessionKey<'_>`. Use `key.storage_key()` for a single-string backend key. **Reading `key.id` alone preserves the old behaviour *and* the CWE-639 vulnerability.**
- An `AuthLayer` used with `X-Session-Id` must now insert `Principal`, or the server must be built with `allow_unbound_sessions()`.
- Embedded deployments with host-supplied auth should insert `Principal` and set `require_principal(true)`.
- 5xx response bodies are no longer diagnostic; the detail is logged via `tracing` at `error` level.
- In-flight runs are capped at 1 024 by default, and a run still live after 1 hour is cancelled.

- [ ] **Step 3: Update the mdBook**

In `docs/book/src/concepts/axum-server.md`: rewrite the session-affinity paragraph at lines 76-78 to describe compound keying and the 403; add `.max_in_flight(usize)` (default 1 024) and `.max_run_duration(Duration)` (default 1 hour) to the builder table at lines 95-96; update the `SessionProvider` signature at lines 104-113 to the new one and carry the "key on `storage_key`, not `id` alone" warning.

Check `docs/book/src/concepts/runtimes.md` — it carries no session-security wording today, so it needs an edit only if its builder-knob summary mentions the caps.

- [ ] **Step 4: Verify the book builds**

Run: `mdbook build docs/book`
Expected: clean. `[output.linkcheck] warning-policy = "error"`, so a broken internal link fails the required `book-build` gate.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-runtime-axum/README.md \
        crates/paigasus-helikon-runtime-actix/README.md \
        docs/book/src/concepts/axum-server.md
git commit -m "docs(runtime): SMA-482 document principal-scoped sessions and the run caps"
```

---

### Task 8: Full CI gate

Every prior task verified a slice. This runs the gates exactly as CI does, because several of them catch nothing until the whole workspace is compiled together.

**Files:** none — verification only, plus any fixes the gates demand.

- [ ] **Step 1: Formatting**

Run: `cargo fmt --all -- --check`
Expected: clean. If not, run `cargo fmt --all` and re-check.

- [ ] **Step 2: Lints**

Run: `cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 3: The full test gate**

Run: `cargo test --workspace --all-features`
Expected: PASS. This exact invocation matters — per-crate runs miss the conformance suite and can mask feature-unification problems.

- [ ] **Step 4: Docs**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
Expected: clean. A missing `///` on any new public item fails here, as does an intra-doc link from a `pub` item to a `pub(crate)` one.

- [ ] **Step 5: Doc coverage**

Run: `DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh`
Expected: at or above threshold.

- [ ] **Step 6: Default-features-off build**

Run: `cargo build -p paigasus-helikon-runtime-axum --no-default-features`
Expected: clean.

- [ ] **Step 7: Book**

Run: `mdbook build docs/book`
Expected: clean.

- [ ] **Step 8: Verify the breaking-change marker survived**

Run: `git log --format='%s%n%b' origin/main..HEAD | grep -c 'BREAKING CHANGE:'`
Expected: at least `1`. If it is `0`, Task 3's footer was lost (most likely to an amend or a squash) and release-plz will cut a patch bump instead of the minor these crates need. Fix it before opening the PR.

- [ ] **Step 9: Confirm nothing unintended is staged or committed**

Run: `git status --short && git diff --stat origin/main..HEAD`
Expected: a clean working tree, and a diff touching only the files this plan names. Confirm `.env` appears nowhere.

- [ ] **Step 10: Commit any gate fixes**

If steps 1–7 required changes:

```bash
git add <the specific files you changed>
git commit -m "fix(runtime): SMA-482 satisfy fmt, clippy, and doc gates"
```

---

## Self-Review

**Spec coverage.** §1 redaction → Task 2 (all three transports, plus the actix `Internal`→`BadRequest` reclassification). §2 principal binding → Task 3 (`Principal`, `SessionKey` + `#[non_exhaustive]` + `new` + `storage_key`, tuple keys for both the session map and the lock map, the `require_principal`/`allow_unbound_sessions` pair defaulting to `auth.is_some()`, the pinned 403 body, the non-UTF-8 400, the actix `RefCell` scope, the documented `max_sessions` limitation). §3 in-flight cap → Task 5 (maintained counter, atomic check-and-insert, `max_run_duration` reclamation, `warn!` on rejection, unconditional zero guard). §4 WebSocket authorisation → Task 4 (principal on `RunHandle`, 404-not-403, `None == None` compatibility). OpenAPI → Task 6. Documentation and migration → Task 7. Verification → Task 8. The `BREAKING CHANGE:` requirement appears in Global Constraints, in Task 3 Step 12, and is re-checked in Task 8 Step 8.

**Type consistency.** `RunRegistry::create` is deliberately edited twice — Task 4 adds the `principal` parameter and keeps it infallible; Task 5 changes the return type to `Result`. Both tasks state the signature they produce, and Task 5's tests use the final three-argument fallible form. `SessionKey::new(principal, id)` has the same argument order everywhere it appears. `AppStateInner::require_principal` is a resolved `bool`; the builder field is `Option<bool>`. The public strings — `"internal error"`, `"service unavailable"`, `"run failed to start"` — are defined in Task 2 and asserted verbatim in Tasks 2 and 5.

**Known soft spots, called out rather than papered over.** Task 3 Step 9's actix `MutatingContextProvider` sketch uses a placeholder error path (`paigasus_helikon_actix_error_alias`) — substitute the crate's real `ServerError`; the shape is what matters. Task 4 Step 7 and Task 7 Steps 1–3 describe test bodies and prose rather than spelling them out, because both follow patterns already established in the files they extend. Task 2 Step 9's `render_body` helper must be written per framework, as noted inline.
