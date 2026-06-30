# SMA-452 runtime-axum follow-ups — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the three deferred SMA-331 polish items for `paigasus-helikon-runtime-axum`: a synthetic `run_failed` terminal frame on SSE/WS streams that end without a real terminal, an O(n) batch-drain `EventLog::subscribe`, and a required `--no-default-features` CI build gate.

**Architecture:** All code changes are confined to the `paigasus-helikon-runtime-axum` crate (`event_log.rs`, `registry.rs`, `handlers/runs.rs`, `handlers/events.rs`, `tests/`). One new CI job in `.github/workflows/ci.yml` plus the required-check ruleset and its doc mirrors. Documentation touch-ups in the mdBook and crate README.

**Tech Stack:** Rust (edition/MSRV inherited from workspace), `axum` 0.8, `tokio`, `futures-util`, `tracing`; tests use `reqwest` + `tokio-tungstenite`.

## Global Constraints

- **MSRV `1.94`**, edition/license/etc. inherited from `[workspace.package]` — do not hardcode per-crate.
- **No manual version bump and no manual CHANGELOG edit.** `runtime-axum` is already-released (0.1.0); release-plz auto-patches it on merge. No `paigasus-helikon-core` API is touched, so no core bump.
- **Ring capacity stays `max_events.min(64)`** — do NOT bump it (decided at GATE 1; the `max_runs` multiplier makes a bump a net memory regression).
- **Before every commit:** `cargo fmt --all` then `cargo clippy --workspace --all-features --all-targets -- -D warnings` must be clean. The pre-push hook also runs `convco`. Commits are signed via a 1Password SSH key — if signing fails with "failed to fill whole buffer", ask the user to unlock the vault; never bypass signing.
- **Commit prefix:** `<type>(<scope>): SMA-452 <lowercase message>`. Valid types/scopes are in `.versionrc` (`perf`/`feat`/`ci`/`docs` + scope `runtime-axum` are all used here).
- **The full local gate** (must stay green): `cargo fmt --all -- --check`; `cargo clippy --workspace --all-features --all-targets -- -D warnings`; `cargo test --workspace --all-features`; `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`; `mdbook build docs/book`.
- **`AgentEvent` is `#[non_exhaustive]`** and does **not** derive `PartialEq` — assert event equality through `serde_json::to_value`, or match on the variant with `matches!`.
- Branch: `feature/sma-452-runtime-axum-follow-ups-ssews-start-error-frame-no-default` (already checked out).

---

### Task 1: O(n) batch-drain `EventLog::subscribe` (+ stale-doc cleanup)

**Files:**
- Modify: `crates/paigasus-helikon-runtime-axum/src/event_log.rs`
- Test: same file's `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: existing `EventLog::read_from(cursor) -> ReadSlice` (with `events`, `first_seq`, `next_cursor`, `terminal`), `EventLog::notify`.
- Produces: `pub(crate) fn is_terminal(&AgentEvent) -> bool` (visibility widened from private — consumed by Task 2 & 3); unchanged public signature `EventLog::subscribe(self: &Arc<Self>, from: u64) -> impl Stream<Item = AgentEvent> + Send + Unpin`.

- [ ] **Step 1: Add the failing large-replay test.** In `event_log.rs`'s `mod tests`, add:

```rust
    /// A large replay must drain in one batch, in order, with no duplicates or
    /// drops — guards the O(n) batch-drain rewrite of `subscribe`.
    #[tokio::test]
    async fn subscribe_replays_large_log_in_order() {
        let log = Arc::new(EventLog::new(8192));
        for i in 0..5000u32 {
            log.append(delta(&i.to_string()));
        }
        log.append(done());
        let got: Vec<_> = log.subscribe(0).collect().await;
        assert_eq!(got.len(), 5001);
        assert!(matches!(&got[0], AgentEvent::TokenDelta { text } if text == "0"));
        assert!(matches!(&got[4999], AgentEvent::TokenDelta { text } if text == "4999"));
        assert!(matches!(&got[5000], AgentEvent::RunCompleted { .. }));
    }
```

- [ ] **Step 2: Run it — expect PASS already.** The current `subscribe` is correct (just O(n²)); this test passes today and is a regression guard for the rewrite.

Run: `cargo test -p paigasus-helikon-runtime-axum --lib subscribe_replays_large_log_in_order`
Expected: PASS (it may be perceptibly slow on the O(n²) code — that's the point of the refactor).

- [ ] **Step 3: Widen `is_terminal` visibility.** Change its signature line:

```rust
pub(crate) fn is_terminal(ev: &AgentEvent) -> bool {
```

- [ ] **Step 4: Cleanup the `next_cursor` field doc + drop the stale allow.** In `ReadSlice`, replace:

```rust
    /// The sequence number the caller should pass on the next [`EventLog::read_from`] call.
    // Consumed by the WebSocket cursor-resume transport added in a later task; the
    // replay-then-tail `subscribe` path computes its own cursor and does not read it.
    #[allow(dead_code)]
    pub next_cursor: u64,
```

with:

```rust
    /// The sequence number the caller should pass on the next [`EventLog::read_from`] call.
    ///
    /// The replay-then-tail [`EventLog::subscribe`] path advances its cursor to this
    /// value after each batch read.
    pub next_cursor: u64,
```

- [ ] **Step 5: Rewrite `subscribe` to batch-drain.** Add a private state struct just above `subscribe` (inside the `impl EventLog` block is fine, or at module scope before it — module scope shown):

```rust
/// Unfold state for [`EventLog::subscribe`]: the resume cursor, a once-cloned
/// batch buffer drained in order, and the terminal flag.
struct SubscribeState {
    cursor: u64,
    pending: std::vec::IntoIter<AgentEvent>,
    done: bool,
}
```

Replace the body of `subscribe` (keep the existing doc comment block; only the `Box::pin(...unfold...)` body changes) with:

```rust
        let log = Arc::clone(self);

        // Each `read_from` clones the available tail ONCE into `pending`; we drain
        // `pending` one event at a time without re-reading, so replaying N retained
        // events is O(n), not O(n²). The notify-before-read ordering is preserved:
        // the wakeup future is still registered (and `enable()`d) before each read
        // that may need to wait.
        Box::pin(futures_util::stream::unfold(
            SubscribeState {
                cursor: from,
                pending: Vec::new().into_iter(),
                done: false,
            },
            move |mut state| {
                let log = Arc::clone(&log);
                async move {
                    if state.done {
                        return None;
                    }
                    // Fast path: drain the previously-read batch first.
                    if let Some(ev) = state.pending.next() {
                        state.done = is_terminal(&ev);
                        return Some((ev, state));
                    }
                    loop {
                        // Register the wakeup BEFORE reading (lost-wakeup avoidance).
                        let notif = log.notify.notified();
                        tokio::pin!(notif);
                        notif.as_mut().enable();

                        let slice = log.read_from(state.cursor);
                        if !slice.events.is_empty() {
                            // Advance past the whole batch (and past any eviction gap:
                            // `read_from` clamps `next_cursor` up from `first_seq`).
                            state.cursor = slice.next_cursor;
                            state.pending = slice.events.into_iter();
                            let ev = state.pending.next().expect("slice.events non-empty");
                            state.done = is_terminal(&ev);
                            return Some((ev, state));
                        }
                        if slice.terminal {
                            // No retained events at/after the cursor and the run ended
                            // (mark_terminal with no events, or everything evicted).
                            return None;
                        }
                        notif.await;
                    }
                }
            },
        ))
```

- [ ] **Step 6: Run the full `event_log` test module.**

Run: `cargo test -p paigasus-helikon-runtime-axum --lib event_log`
Expected: PASS — all of `read_from_cursor_returns_tail_and_terminal`, `bounded_ring_truncates_head`, `subscribe_replays_then_tails_until_terminal`, `subscribe_skips_gap_on_ring_eviction`, `subscribe_does_not_lose_fast_appended_event`, `new_rejects_zero_capacity`, and the new `subscribe_replays_large_log_in_order`.

- [ ] **Step 7: fmt + clippy, then commit.**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-axum --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-axum/src/event_log.rs
git commit -m "perf(runtime-axum): SMA-452 batch-drain EventLog::subscribe replay

Drain each read_from slice from a buffer in the unfold state instead of
re-reading the whole tail per event, making replay O(n) not O(n^2). Widen
is_terminal to pub(crate) for the streaming transports' synthetic-frame
path. Preserves the lost-wakeup and eviction-gap guarantees; cleans up the
now-read next_cursor field doc.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: synthetic `run_failed` frame on the **SSE** transport

**Files:**
- Modify: `crates/paigasus-helikon-runtime-axum/src/registry.rs` (add `RunHandle::synthetic_terminal_frame` + import)
- Modify: `crates/paigasus-helikon-runtime-axum/src/handlers/runs.rs` (`sse_response`)
- Modify: `crates/paigasus-helikon-runtime-axum/tests/support/mod.rs` (add `PartialThenEndRunner`)
- Test: `crates/paigasus-helikon-runtime-axum/tests/runs.rs` (two SSE tests) + a unit test in `registry.rs`

**Interfaces:**
- Consumes: `is_terminal` (Task 1), `RunHandle { start_error: Mutex<Option<String>>, agent_name, log, cancel }`, `to_sse_event`, `EventLog::subscribe`.
- Produces: `pub(crate) fn RunHandle::synthetic_terminal_frame(&self, saw_terminal: bool) -> Option<AgentEvent>` (consumed by Task 3 too).

- [ ] **Step 1: Add the `PartialThenEndRunner` test helper.** Append to `tests/support/mod.rs`:

```rust
// ── PartialThenEndRunner ────────────────────────────────────────────────────────

/// A test [`Runner`] whose `run_streamed` succeeds and yields exactly one
/// non-terminal event (`TokenDelta { "hi" }`), then ends the stream WITHOUT a
/// terminal `RunCompleted`/`RunFailed`. Exercises the streaming transports'
/// synthetic-terminal-frame path for a run that produced real events first, so
/// `saw_terminal` must stay false and the generic message is used.
pub struct PartialThenEndRunner;

#[async_trait]
impl<Ctx: Send + Sync + 'static> Runner<Ctx> for PartialThenEndRunner {
    async fn run(
        &self,
        _agent: &(dyn Agent<Ctx> + '_),
        _ctx: RunContext<Ctx>,
        _input: AgentInput,
        _config: RunConfig,
    ) -> Result<RunResult, RunError> {
        unimplemented!("PartialThenEndRunner is only used through run_streamed")
    }

    async fn run_streamed(
        &self,
        _agent: &(dyn Agent<Ctx> + '_),
        _ctx: RunContext<Ctx>,
        _input: AgentInput,
        _config: RunConfig,
    ) -> Result<RunResultStreaming, RunError> {
        let stream = stream::iter(vec![AgentEvent::TokenDelta {
            text: "hi".to_owned(),
        }])
        .boxed();
        Ok(RunResultStreaming::new(stream))
    }
}
```

(`tests/support/mod.rs` already imports `AgentEvent`, `RunResultStreaming`, `stream`, `StreamExt as _`, `Runner`, etc., and is `#![allow(dead_code)]` module-wide — no import changes needed.)

- [ ] **Step 2: Add the `RunHandle::synthetic_terminal_frame` helper + its unit test (failing).** In `registry.rs`, add `use paigasus_helikon_core::AgentEvent;` to the imports, then add an impl block after the `RunHandle` struct:

```rust
impl RunHandle {
    /// The synthetic terminal frame a streaming transport must emit when its
    /// subscribe stream ended without delivering a real `RunCompleted`/`RunFailed`.
    ///
    /// Returns `None` when a real terminal was already delivered (`saw_terminal`
    /// is `true`). Otherwise returns an [`AgentEvent::RunFailed`], sourced from
    /// [`start_error`](RunHandle#structfield.start_error) if the run failed to
    /// start, or a generic message (e.g. a stream that panicked or ended mid-run
    /// before any terminal event).
    pub(crate) fn synthetic_terminal_frame(&self, saw_terminal: bool) -> Option<AgentEvent> {
        if saw_terminal {
            return None;
        }
        let error = self
            .start_error
            .lock()
            .expect("start_error mutex poisoned")
            .clone()
            .unwrap_or_else(|| "run ended before producing a terminal event".to_owned());
        tracing::warn!(
            agent = %self.agent_name,
            %error,
            "run ended without a real terminal event; synthesizing a RunFailed frame for the stream subscriber"
        );
        Some(AgentEvent::RunFailed { error })
    }
}
```

Add to `registry.rs`'s `#[cfg(test)] mod tests`:

```rust
    /// `synthetic_terminal_frame` returns `None` once a real terminal was seen,
    /// the captured `start_error` when present, and a generic message otherwise.
    #[test]
    fn synthetic_terminal_frame_branches() {
        let reg = RunRegistry::new(Duration::from_secs(60), 16, 16);
        let (_id, h) = reg.create("a".into(), CancellationToken::new());

        assert!(h.synthetic_terminal_frame(true).is_none());

        match h.synthetic_terminal_frame(false) {
            Some(AgentEvent::RunFailed { error }) => {
                assert_eq!(error, "run ended before producing a terminal event");
            }
            other => panic!("expected generic RunFailed, got {other:?}"),
        }

        *h.start_error.lock().unwrap() = Some("boom".to_owned());
        match h.synthetic_terminal_frame(false) {
            Some(AgentEvent::RunFailed { error }) => assert_eq!(error, "boom"),
            other => panic!("expected RunFailed(boom), got {other:?}"),
        }
    }
```

- [ ] **Step 3: Run the unit test.**

Run: `cargo test -p paigasus-helikon-runtime-axum --lib synthetic_terminal_frame_branches`
Expected: PASS.

- [ ] **Step 4: Add the failing SSE integration tests.** In `tests/runs.rs`, add `use std::sync::Arc;`, `use paigasus_helikon_core::AgentEvent;`, and `use paigasus_helikon_runtime_axum::AgentServer;` near the top (after `mod support;`), then add:

```rust
/// A run whose runner start-errors must still surface a final synthetic
/// `run_failed` frame on the SSE stream (the stream is HTTP 200; the failure
/// is in-band), then close.
#[tokio::test]
async fn sse_emits_synthetic_run_failed_on_start_error() {
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

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs?stream=sse"))
        .header("content-type", "application/json")
        .body(r#"{"input":"x"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "SSE responses are 200; failure is in-band");
    let events = support::parse_sse(&resp.text().await.unwrap());
    assert_eq!(events.len(), 1, "exactly one synthetic terminal frame");
    assert!(
        matches!(&events[0], AgentEvent::RunFailed { error } if !error.is_empty()),
        "expected a non-empty RunFailed, got {:?}",
        events[0]
    );
}

/// A run that yields real events then ends with no terminal must get a final
/// synthetic `run_failed` frame (generic message) appended AFTER the real events.
#[tokio::test]
async fn sse_emits_synthetic_run_failed_after_terminalless_stream() {
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

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs?stream=sse"))
        .header("content-type", "application/json")
        .body(r#"{"input":"x"}"#)
        .send()
        .await
        .unwrap();
    let events = support::parse_sse(&resp.text().await.unwrap());
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], AgentEvent::TokenDelta { text } if text == "hi"));
    assert!(
        matches!(&events[1], AgentEvent::RunFailed { error }
            if error == "run ended before producing a terminal event"),
        "expected generic RunFailed last, got {:?}",
        events[1]
    );
}
```

- [ ] **Step 5: Run them — expect FAIL.** They fail today because the SSE stream closes with no synthetic frame (`events.len()` is 0 / 1 respectively).

Run: `cargo test -p paigasus-helikon-runtime-axum --test runs sse_emits_synthetic`
Expected: FAIL (assertion on `events.len()`).

- [ ] **Step 6: Implement the SSE synthetic frame.** In `handlers/runs.rs`: change the event_log import to bring in `is_terminal`, and add `DropGuard`:

```rust
use crate::{
    dto::{AsyncAccepted, RunRequest, RunResponse},
    error::ServerError,
    event_log::{is_terminal, EventLog},
    registry::{RunHandle, RunRegistry},
    server::AppState,
};
```

and change `use tokio_util::sync::CancellationToken;` to `use tokio_util::sync::{CancellationToken, DropGuard};`.

Add a private state struct just above `sse_response`:

```rust
/// Unfold state for the SSE response stream: the live event stream, the cancel
/// drop-guard (held for the stream's whole lifetime so a client disconnect
/// cancels the run), a clone of the run handle (to synthesize a terminal frame
/// on a terminal-less close), and the `saw_terminal` / `done` flags.
struct SseState<S> {
    events: S,
    disconnect: DropGuard,
    handle: Arc<RunHandle>,
    saw_terminal: bool,
    done: bool,
}
```

Replace the body of `sse_response` with:

```rust
    let disconnect = handle.cancel.clone().drop_guard();
    let events = handle.log.subscribe(0);
    let handle = Arc::clone(handle);

    let stream = futures_util::stream::unfold(
        SseState {
            events,
            disconnect,
            handle,
            saw_terminal: false,
            done: false,
        },
        |mut state| async move {
            if state.done {
                return None;
            }
            match state.events.next().await {
                Some(ev) => {
                    state.saw_terminal |= is_terminal(&ev);
                    let frame = to_sse_event(&ev);
                    Some((Ok::<Event, Infallible>(frame), state))
                }
                None => {
                    // Live stream ended. If no real terminal was delivered, emit
                    // exactly one synthetic `run_failed` frame, then finish.
                    let synthetic = state.handle.synthetic_terminal_frame(state.saw_terminal)?;
                    let frame = to_sse_event(&synthetic);
                    state.done = true;
                    Some((Ok::<Event, Infallible>(frame), state))
                }
            }
        },
    );

    let mut response = Sse::new(stream).into_response();
    insert_run_id(response.headers_mut(), run_id);
    response
```

- [ ] **Step 7: Run the SSE tests + the existing happy-path SSE test.**

Run: `cargo test -p paigasus-helikon-runtime-axum --test runs`
Expected: PASS — both new tests plus `sse_stream_matches_local_events` (proves no spurious frame on a normal run) and the rest.

- [ ] **Step 8: fmt + clippy, then commit.**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-axum --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-axum/src/registry.rs \
        crates/paigasus-helikon-runtime-axum/src/handlers/runs.rs \
        crates/paigasus-helikon-runtime-axum/tests/support/mod.rs \
        crates/paigasus-helikon-runtime-axum/tests/runs.rs
git commit -m "feat(runtime-axum): SMA-452 emit synthetic run_failed frame on SSE terminal-less close

Add RunHandle::synthetic_terminal_frame and have the SSE transport emit a
final RunFailed frame (from start_error, else a generic message) whenever its
subscribe stream ends without a real terminal event — start-error and
terminal-less-stream paths both. tracing::warn at the synthesize point.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: synthetic `run_failed` frame on the **WebSocket** transport

**Files:**
- Modify: `crates/paigasus-helikon-runtime-axum/src/handlers/events.rs` (`handle_socket`)
- Test: `crates/paigasus-helikon-runtime-axum/tests/ws.rs` (two WS tests)

**Interfaces:**
- Consumes: `RunHandle::synthetic_terminal_frame` (Task 2), `is_terminal` (Task 1), `support::PartialThenEndRunner` + `support::FailingRunner` + `support::create_async_run`.

- [ ] **Step 1: Add the failing WS integration tests.** In `tests/ws.rs`, add imports near the top (it already has `use futures_util::StreamExt;` and `mod support;`):

```rust
use std::sync::Arc;

use paigasus_helikon_core::AgentEvent;
use paigasus_helikon_runtime_axum::AgentServer;
```

Add the tests:

```rust
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
```

- [ ] **Step 2: Run them — expect FAIL.** Today the WS stream closes with no synthetic frame (`got.len()` is 0).

Run: `cargo test -p paigasus-helikon-runtime-axum --test ws ws_emits_synthetic`
Expected: FAIL (assertion on `got.len()`).

- [ ] **Step 3: Implement the WS synthetic frame.** In `handlers/events.rs`, add `use crate::event_log::is_terminal;` to the imports (the `use crate::{...}` block becomes `use crate::{error::ServerError, event_log::is_terminal, registry::RunHandle, server::AppState};`). In `handle_socket`, track `saw_terminal` and emit the frame in the stream-ended arm:

```rust
async fn handle_socket(mut socket: WebSocket, handle: Arc<RunHandle>) {
    let mut sub = handle.log.subscribe(0);
    let mut saw_terminal = false;

    loop {
        tokio::select! {
            // Next event from the log (replay + live tail).
            ev = sub.next() => {
                match ev {
                    Some(ev) => {
                        if is_terminal(&ev) {
                            saw_terminal = true;
                        }
                        let text = match serde_json::to_string(&ev) {
                            Ok(t) => t,
                            Err(_) => break,
                        };
                        if socket.send(Message::text(text)).await.is_err() {
                            break;
                        }
                    }
                    // Log stream ended. If no real terminal was delivered (start
                    // error / terminal-less stream), send a final synthetic
                    // `RunFailed` frame before the Close so the client always sees
                    // a terminal frame.
                    None => {
                        if let Some(frame) = handle.synthetic_terminal_frame(saw_terminal) {
                            if let Ok(text) = serde_json::to_string(&frame) {
                                let _ = socket.send(Message::text(text)).await;
                            }
                        }
                        let _ = socket.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
            // Inbound frames from the client (drain to observe close/disconnect).
            msg = socket.recv() => {
                match msg {
                    None | Some(Err(_)) => break,
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run the WS tests + the existing happy-path WS test.**

Run: `cargo test -p paigasus-helikon-runtime-axum --test ws`
Expected: PASS — both new tests plus `ws_replays_completed_run_then_closes` (no spurious frame on a normal run) and `ws_unknown_id_404_before_upgrade`.

- [ ] **Step 5: fmt + clippy, then commit.**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-axum --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-axum/src/handlers/events.rs \
        crates/paigasus-helikon-runtime-axum/tests/ws.rs
git commit -m "feat(runtime-axum): SMA-452 emit synthetic run_failed frame on WebSocket terminal-less close

Track saw_terminal in handle_socket and send a final RunFailed text frame
before the Close whenever the subscribe stream ends without a real terminal
event. Every SSE/WS subscriber stream now ends with a terminal frame.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: required `--no-default-features` CI build gate

**Files:**
- Modify: `.github/workflows/ci.yml` (new `build-no-default-features` job)
- Modify: `.github/rulesets/main-protection-checks.json` (add the required context)
- Modify: `CONTRIBUTING.md:276` (required-checks list)
- Modify: `CLAUDE.md:103` (required-checks list — add `build-no-default-features` **and** the already-missing `sessions-it`)

**Interfaces:** none (CI/config only).

- [ ] **Step 1: Confirm the gate is green locally first.**

Run: `cargo build -p paigasus-helikon-runtime-axum --no-default-features`
Expected: `Finished` (the gating is already correct; this job prevents future regressions).

- [ ] **Step 2: Add the CI job.** In `.github/workflows/ci.yml`, add this job (reuse the SHAs already pinned in the file — do not re-resolve). Place it after the `test:` job block:

```yaml
  build-no-default-features:
    runs-on: ubuntu-latest
    steps:
      # actions/checkout v6.0.2
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
        with:
          persist-credentials: false
      # dtolnay/rust-toolchain master (no tagged releases)
      - uses: dtolnay/rust-toolchain@67ef31d5b988238dd797d409d6f9574278e20537
        with:
          toolchain: stable
      # Swatinem/rust-cache v2.9.1
      - uses: Swatinem/rust-cache@c19371144df3bb44fab255c43d04cbc2ab54d1c4
      - run: cargo build -p paigasus-helikon-runtime-axum --no-default-features
```

- [ ] **Step 3: Add the required-check context to the ruleset.** In `.github/rulesets/main-protection-checks.json`, add a line to the `required_status_checks` array after the `sessions-it` entry:

```json
          { "context": "sessions-it" },
          { "context": "build-no-default-features" }
```

(Add the comma after `sessions-it` and the new line; keep valid JSON.)

- [ ] **Step 4: Update `CONTRIBUTING.md:276`.** Append `build-no-default-features` to the required-contexts sentence, with a parenthetical noting why it's required:

```
…, `deny`, `sessions-it` (required because it is the only gate that runs the Postgres/Redis session backends against live servers), `build-no-default-features` (required because it is the only gate that compiles the crate with `--no-default-features`, catching `openapi`-feature-gating regressions). |
```

- [ ] **Step 5: Update `CLAUDE.md:103`.** That list is missing `sessions-it`; add both it and the new job. Change the contexts list to end:

```
…, `commits`, `pr-title`, `audit`, `deny`, `sessions-it`, `build-no-default-features`. The macOS job is required because it is the only gate that compiles and runs the Seatbelt backend; `sessions-it` because it is the only gate that runs the live Postgres/Redis session backends; `build-no-default-features` because it is the only gate that compiles the crate with default features off.
```

- [ ] **Step 6: Validate the YAML + JSON parse.**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml'))" && python3 -c "import json; json.load(open('.github/rulesets/main-protection-checks.json'))" && echo OK`
Expected: `OK`.

- [ ] **Step 7: Commit.**

```bash
git add .github/workflows/ci.yml .github/rulesets/main-protection-checks.json CONTRIBUTING.md CLAUDE.md
git commit -m "ci(runtime-axum): SMA-452 add required --no-default-features build gate

New build-no-default-features job compiling runtime-axum with default features
off, added to the required-check ruleset + CONTRIBUTING/CLAUDE.md (also fixes
the pre-existing sessions-it omission in CLAUDE.md). Applying the ruleset is a
post-merge maintainer step (scripts/apply-repo-config.sh).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

> **Post-merge (maintainer, surfaced at GATE 2):** run `scripts/apply-repo-config.sh` to make the new context actually required, then rebase/re-run any open PRs (notably the release-plz `chore: release` PR) so they report the new context. The ruleset JSON edit is inert until this is applied.

---

### Task 5: documentation (mdBook + crate README)

**Files:**
- Modify: `docs/book/src/concepts/axum-server.md` (add the streaming error-frame behavior)
- Modify: `crates/paigasus-helikon-runtime-axum/README.md` (add a streaming error-semantics line)

**Interfaces:** none.

- [ ] **Step 1: Add the behavior to the mdBook.** In `docs/book/src/concepts/axum-server.md`, in the "Replayable runs" bullet list (after the "Cancellation" bullet), add:

```markdown
- **Stream error frames**: if a run ends without a real terminal event — e.g. its session backend fails to start, or its stream ends early — the SSE and WebSocket transports emit a final synthetic `run_failed` event before closing, so a streaming client always observes a terminal frame. (One-shot mode instead returns HTTP `500`.)
```

- [ ] **Step 2: Add a line to the crate README.** In `crates/paigasus-helikon-runtime-axum/README.md`, immediately after the "Routes" table, add:

```markdown
On a start error — or any run that ends without a terminal event — the streaming transports (SSE and WebSocket) emit a final synthetic `run_failed` event before closing, so a streaming client always sees a terminal frame; one-shot runs instead return `500`.
```

- [ ] **Step 3: Build the book (linkcheck = error).**

Run: `mdbook build docs/book`
Expected: builds cleanly, no link warnings. (If `mdbook` isn't installed: `cargo install mdbook mdbook-linkcheck` — match the versions CI uses if pinned.)

- [ ] **Step 4: Commit.**

```bash
git add docs/book/src/concepts/axum-server.md crates/paigasus-helikon-runtime-axum/README.md
git commit -m "docs(runtime-axum): SMA-452 document streaming synthetic run_failed frame

Note the new SSE/WebSocket terminal-frame-on-terminal-less-close behavior in
the mdBook axum-server concept page and the crate README.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Final verification (run before opening the PR)

- [ ] **Full workspace gate:**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo build -p paigasus-helikon-runtime-axum --no-default-features
mdbook build docs/book
```

Expected: all green. Then confirm the diff matches this plan (no stray debug code, no `dbg!`/`println!`), and that the ring capacity in `event_log.rs` is still `max_events.min(64)`.

---

## Self-review notes

- **Spec coverage:** Task 1 ⇒ spec "Task 3" (batch-drain + capacity-left-at-min(64) + read_from cleanup); Tasks 2-3 ⇒ spec "Task 1" (SSE/WS synthetic frame, broad invariant, helper, `PartialThenEndRunner`, all test cases, tracing::warn); Task 4 ⇒ spec "Task 2" (required CI gate + ruleset/CONTRIBUTING/CLAUDE.md + sessions-it drift + rollout note); Task 5 ⇒ spec "Documentation & release" (book ADD + README). The "no manual version bump" constraint is honored (no version/CHANGELOG edits anywhere).
- **Type consistency:** `synthetic_terminal_frame(&self, saw_terminal: bool) -> Option<AgentEvent>` is defined in Task 2 and consumed identically in Tasks 2-3; `is_terminal` widened in Task 1 and consumed in Tasks 2-3; `PartialThenEndRunner` defined in Task 2 step 1 and reused in Task 3.
- **No placeholders:** every code step shows full code; every run step gives the command + expected result.
