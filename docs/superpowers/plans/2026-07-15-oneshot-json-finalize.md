# SMA-456 One-Shot JSON Finalize Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `runtime-agentcore`'s buffered JSON invocation path finalize the turn's session write when a client disconnects mid-run, instead of silently losing it.

**Architecture:** `run_json` currently awaits `runner.run(...)` inside the axum handler future, so a client disconnect drops the future and `TokioRunner::run`'s inline `finalize` never executes. Move the run onto a detached `tokio::spawn` task and await its result over a `oneshot` channel, holding a `DropGuard` over the run's `CancellationToken` so a disconnect cancels the run while the detached task still drives it to a terminal and finalizes. This mirrors `run_sse`, fixed the same way in SMA-332, and `runtime-axum`'s `spawn_writer`.

**Tech Stack:** Rust 2024, tokio, axum 0.8, `tokio_util::sync::{CancellationToken, DropGuard}`, `async_trait`, `futures_util`.

**Spec:** `docs/superpowers/specs/2026-07-15-oneshot-json-finalize-design.md`

## Global Constraints

- **Worktree:** `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-456-oneshot-finalize`. Run every command from there. Never `cd` to the main checkout.
- **Branch:** `feature/sma-456-runtime-agentcore-runtime-axum-finalize-one-shot-json-runs`. Never change branches, never move HEAD, never stash.
- **No version bumps.** `runtime-agentcore` is already released and this adds no new core API — release-plz's pure-auto path. Do NOT edit any `version =` field, any `[workspace.dependencies]` pin, or any CHANGELOG.
- **Commit format:** `<type>(<scope>): SMA-456 <lowercase subject>`. Allowed scopes here (from `.versionrc`): `runtime-agentcore`, `runtime-axum`, `core`, `docs`, `spec`, `plan`. Commits are signed via a 1Password SSH key — if a commit fails with "failed to fill whole buffer", the vault is locked: stop and ask the user to unlock it. Never bypass signing.
- **Formatting/lints:** run `cargo fmt --all` before every commit. The pre-commit hook is a deliberate no-op, so nothing catches formatting for you until push time.
- **Do not "simplify" the 30s agent hang** in either new test down to a smaller value. See Task 1 Step 1 for why it is load-bearing.
- **Never use `let _ = cancel.drop_guard()`.** That drops the guard immediately and cancels the run instantly. It must bind to a named `_disconnect`.

---

### Task 1: Detach `run_json` so a disconnect still finalizes

**Files:**
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/invoke.rs` (imports at `:43`, `invocations` at `:134-162`, `run_json` at `:179-197`, tests module at `:308+`)

**Interfaces:**
- Consumes: `AppState<Ctx>` fields `runner: Arc<dyn Runner<Ctx>>`, `agent: Arc<dyn Agent<Ctx>>`, `run_config: RunConfig` (`server.rs:37-50`); `paigasus_helikon_core::CancellationToken`; `paigasus_helikon_runtime_axum::{InMemorySessionProvider, SessionProvider}`.
- Produces: `run_json(state, ctx, cancel, input)` — note the **new third parameter** `cancel: CancellationToken`, inserted before `input`. No other task consumes this.

- [ ] **Step 1: Write the failing regression test**

Add to the tests module in `crates/paigasus-helikon-runtime-agentcore/src/invoke.rs`, immediately after the existing `sse_client_disconnect_still_finalizes_the_session` test (end of the `mod tests` block).

Why a start-signal instead of reading a frame: a JSON-mode client receives **nothing** until the run ends, so unlike the SSE test there is no frame to key the disconnect off, and a fixed `sleep` would be flaky.

Why 30s hang vs 10s poll: with a hang *shorter* than the poll window, a non-cancelling implementation would still pass, because the run would finalize naturally before the poll gave up. With hang > poll window, the test passes only if the disconnect **both** cancels within the window **and** still finalizes.

```rust
    // ── (g) JSON client disconnect cancels and finalizes the run ────────────

    /// Signals on `started` from its FIRST stream element, then hangs for 30s
    /// before it would emit `RunCompleted`.
    ///
    /// The signal exists because a JSON-mode client receives nothing until the
    /// run ends — unlike SSE there is no frame to key a disconnect off, and a
    /// fixed sleep would race the run's start.
    struct SignallingSlowAgent {
        started: mpsc::UnboundedSender<()>,
    }

    #[async_trait]
    impl Agent<()> for SignallingSlowAgent {
        fn name(&self) -> &str {
            "signalling-slow"
        }

        fn description(&self) -> &str {
            "test-only agent that signals run start then hangs"
        }

        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            let started = self.started.clone();
            let first = stream::once(async move {
                let _ = started.send(());
                AgentEvent::RunStarted {
                    agent: "signalling-slow".to_owned(),
                }
            });
            let hangs = stream::once(async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                }
            });
            Ok(first.chain(hangs).boxed())
        }
    }

    /// A JSON-mode client that disconnects mid-run must still get its turn
    /// persisted: the detached run task (see [`run_json`]'s doc comment) drives
    /// the runner to its terminal — guaranteeing `TokioRunner::run`'s inline
    /// finalize step executes — while the retained cancel token aborts the
    /// now-orphaned run instead of leaking it for the agent's full 30s hang.
    ///
    /// The 30s hang against a 10s poll window is deliberate: a shorter hang
    /// would let a NON-cancelling implementation pass, because the run would
    /// finalize naturally inside the window. Do not shorten it.
    ///
    /// Drives a real TCP disconnect (rather than `Router::oneshot`, which
    /// buffers the whole response and cannot model a client walking away).
    #[tokio::test]
    async fn json_client_disconnect_still_finalizes_the_session() {
        use paigasus_helikon_runtime_axum::{InMemorySessionProvider, SessionProvider};
        use tokio::io::AsyncWriteExt as _;

        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let sessions = Arc::new(InMemorySessionProvider::new(16));
        let server = AgentCoreServer::<()>::builder()
            .agent(Arc::new(SignallingSlowAgent { started: started_tx }))
            .with_default_context()
            .session_provider(Arc::clone(&sessions) as Arc<dyn SessionProvider>)
            .build()
            .expect("server builds");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = server.router();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let session_id = "e".repeat(40);
        let body = r#"{"prompt":"hi"}"#;
        let len = body.len();
        let request = format!(
            "POST /invocations HTTP/1.1\r\n\
             Host: {addr}\r\n\
             Content-Type: application/json\r\n\
             Accept: application/json\r\n\
             X-Amzn-Bedrock-AgentCore-Runtime-Session-Id: {session_id}\r\n\
             Content-Length: {len}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}"
        );

        {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            client.write_all(request.as_bytes()).await.unwrap();

            // Wait until the run has demonstrably started server-side, then let
            // `client` drop at the end of this block — a real mid-run
            // disconnect, not a graceful close.
            tokio::time::timeout(std::time::Duration::from_secs(10), started_rx.recv())
                .await
                .expect("timed out waiting for the run to start")
                .expect("agent signalled run start");
        }

        // The dropped connection must cancel the run, and the detached task must
        // still run `finalize`, persisting the turn's input message. (`Runner::run`
        // aborts on cancel without synthesizing a terminal — that behavior belongs
        // to `run_streamed` — so the assertion below is on the persisted user
        // message, not on a terminal event.)
        let session = sessions.session(Some(&session_id)).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let snapshot = session.snapshot().await.unwrap();
                if !snapshot.messages.is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("session was never finalized after the JSON client disconnected");

        let snapshot = session.snapshot().await.unwrap();
        assert!(
            matches!(&snapshot.messages[0], Item::UserMessage { .. }),
            "expected the turn's user message to be persisted, got {:?}",
            snapshot.messages[0]
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails RED**

Run:
```bash
cargo test -p paigasus-helikon-runtime-agentcore --lib json_client_disconnect_still_finalizes_the_session
```

Expected: **FAIL**, panicking with `session was never finalized after the JSON client disconnected: Elapsed(())` after ~10s.

This RED is the whole point — it reproduces SMA-456. If it PASSES here, stop and report: the premise no longer holds and the rest of the plan is invalid.

- [ ] **Step 3: Widen the tokio::sync import**

In `crates/paigasus-helikon-runtime-agentcore/src/invoke.rs:43`, replace:

```rust
use tokio::sync::mpsc;
```

with:

```rust
use tokio::sync::{mpsc, oneshot};
```

- [ ] **Step 4: Update the retained-token comment and rename it**

In `invocations` (`invoke.rs:151-161`), replace:

```rust
    let cancel = CancellationToken::new();
    // Retain a clone before it is moved into `ctx`: `run_sse` needs its own handle
    // on the token to cancel the run on client disconnect (see its doc comment).
    let cancel_for_sse = cancel.clone();
    let ctx = state.context.build(&parts, session, cancel).await?;

    if json_mode {
        run_json(&state, ctx, input).await
    } else {
        Ok(run_sse(&state, ctx, cancel_for_sse, input).await)
    }
```

with:

```rust
    let cancel = CancellationToken::new();
    // Retain a clone before it is moved into `ctx`: both transports need their own
    // handle on the token to cancel the run on client disconnect (see each one's
    // doc comment).
    let cancel_for_run = cancel.clone();
    let ctx = state.context.build(&parts, session, cancel).await?;

    if json_mode {
        run_json(&state, ctx, cancel_for_run, input).await
    } else {
        Ok(run_sse(&state, ctx, cancel_for_run, input).await)
    }
```

- [ ] **Step 5: Add the new error case to the `invocations` doc**

In `invocations`' `# Errors` list (`invoke.rs:127-133`), replace the `Internal` bullet:

```rust
/// - [`AgentCoreError::Internal`] (500) — session resolution, context construction, or
///   (JSON mode only) the run itself failed. In SSE mode a run failure is instead
///   surfaced as the stream's terminal `RunFailed` frame — the response itself stays
///   `200`, per SSE semantics.
```

with:

```rust
/// - [`AgentCoreError::Internal`] (500) — session resolution, context construction, or
///   (JSON mode only) the run itself failed, or (JSON mode only) the detached run task
///   ended without reporting a result because it panicked or the runtime shut down. In
///   SSE mode a run failure is instead surfaced as the stream's terminal `RunFailed`
///   frame — the response itself stays `200`, per SSE semantics.
```

- [ ] **Step 6: Replace `run_json` with the detached implementation**

Replace the whole of `run_json` (`invoke.rs:179-197`, from its `/// Buffered JSON-mode response:` doc line through its closing brace) with:

```rust
/// Buffered JSON-mode response: run to completion, then aggregate into an
/// [`InvocationResponse`].
///
/// # Disconnect semantics
///
/// The run is driven by a **detached** [`tokio::spawn`] task rather than awaited
/// inline in the handler future, mirroring [`run_sse`] (and
/// `paigasus-helikon-runtime-axum`'s `spawn_writer`):
///
/// - [`paigasus_helikon_core::Runner::run`] performs its finalize step — which
///   persists the turn to the session — inside the future it returns. Awaiting that
///   future *in the handler* would mean a client disconnect drops it mid-run and the
///   turn's session write is silently lost (SMA-456). Owning it in a detached task
///   decouples the run's lifetime from the HTTP response's, so finalize always runs.
/// - `cancel` (a clone of the token also embedded in `ctx`, retained by the caller —
///   see [`invocations`]) is wrapped in a [`DropGuard`] bound for the handler
///   future's lifetime. When that future is dropped — a client disconnecting mid-run
///   — the guard fires [`CancellationToken::cancel`], so the runner aborts the
///   in-flight run instead of running to its natural end. Dropping the guard after a
///   clean completion is harmless (cancelling a finished run is a no-op).
/// - Net effect: a disconnect cancels the run; the runner's stream ends, `finalize`
///   persists the recorder's events (the turn's user message plus any assistant/tool
///   items observed before the cancel), and `run` returns `Err(RunError::Cancelled)`
///   — which nobody is left to receive. The turn is persisted; nothing is leaked.
///   Unlike [`run_sse`], no synthetic terminal event is produced: terminal synthesis
///   lives in `run_streamed`, not in `Runner::run`.
///
/// Because finalize runs *before* `Runner::run`'s future resolves, a received result
/// implies the session write already landed — so the `200` is never returned ahead of
/// the persisted turn.
async fn run_json<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    ctx: RunContext<Ctx>,
    cancel: CancellationToken,
    input: AgentInput,
) -> Result<Response, AgentCoreError> {
    let runner = Arc::clone(&state.runner);
    let agent = Arc::clone(&state.agent);
    let run_config = state.run_config.clone();

    // Detached: its lifetime is independent of the handler future's, which is
    // exactly why the runner's finalize step always runs — see the doc above.
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let result = runner.run(agent.as_ref(), ctx, input, run_config).await;
        if tx.send(result).is_err() {
            // The client disconnected, so nobody is left to receive the outcome.
            // The session write has already happened (finalize runs before `run`
            // resolves), so this is bookkeeping, not a lost turn.
            tracing::debug!("invocation client disconnected; run outcome discarded");
        }
    });

    // MUST bind to a name: `let _ = cancel.drop_guard()` would drop the guard
    // immediately and cancel every run the instant it started.
    let _disconnect = cancel.drop_guard();

    let result = rx
        .await
        .map_err(|_| {
            tracing::error!("run task ended without reporting a result (panicked or runtime shut down)");
            AgentCoreError::Internal("run task ended without a result".to_owned())
        })?
        .map_err(|e| AgentCoreError::Internal(format!("run failed: {e}")))?;

    Ok(Json(InvocationResponse {
        final_output: result.final_output,
        usage: result.usage,
    })
    .into_response())
}
```

Note `agent.as_ref()` borrows an `Arc` owned *by the async block*, which is what makes the non-`'static` `&dyn Agent<Ctx>` legal inside `tokio::spawn`. `run_sse` relies on the same construction at `invoke.rs:247-249`.

- [ ] **Step 7: Run the new test to verify it passes GREEN**

Run:
```bash
cargo test -p paigasus-helikon-runtime-agentcore --lib json_client_disconnect_still_finalizes_the_session
```

Expected: **PASS** in roughly 1s (not 10s — the cancel fires quickly, so finalize lands on the first or second poll).

- [ ] **Step 8: Run the crate's whole suite for regressions**

Run:
```bash
cargo test -p paigasus-helikon-runtime-agentcore --all-features
```

Expected: PASS, including the pre-existing `sse_client_disconnect_still_finalizes_the_session`, `json_mode_returns_final_output_and_usage`, and `same_session_id_continues_the_conversation`.

- [ ] **Step 9: Format and lint**

Run:
```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-agentcore --all-features --all-targets -- -D warnings
```

Expected: clean. If clippy flags the long `tracing::error!` line, let `cargo fmt` wrap it rather than shortening the message.

- [ ] **Step 10: Commit**

```bash
git add crates/paigasus-helikon-runtime-agentcore/src/invoke.rs
git commit -m "fix(runtime-agentcore): SMA-456 finalize one-shot json runs on client disconnect

run_json awaited runner.run() inside the handler future, so a client
disconnect dropped the future and TokioRunner::run's inline finalize never
executed -- silently losing the turn's session write. Drive the run on a
detached task and await the result over a oneshot, holding a DropGuard over
the cancel token so a disconnect cancels the run while the detached task
still finalizes. Mirrors run_sse (SMA-332) and runtime-axum's spawn_writer.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Lock in `runtime-axum`'s already-correct one-shot

**Files:**
- Modify: `crates/paigasus-helikon-runtime-axum/tests/support/mod.rs` (add fixtures after `spawn_echo_server`, which ends at `:93`)
- Modify: `crates/paigasus-helikon-runtime-axum/tests/concurrency.rs` (add test at end)

**Interfaces:**
- Consumes: `AgentServer::<()>::builder()` with `.agent(Arc<dyn Agent<Ctx>>)` (`server.rs:116`), `.session_provider(Arc<dyn SessionProvider>)` (`server.rs:138`), `.with_default_context()` (`server.rs:250`), `.serve_with_listener(listener)`.
- Produces: `support::SignallingHangingAgent { started: mpsc::UnboundedSender<()> }` and `support::spawn_hanging_server(started_tx) -> (SocketAddr, Arc<InMemorySessionProvider>)`. Nothing downstream consumes these.

**Context:** `create_run` calls `spawn_writer` at `runs.rs:188` — for *every* transport — before branching on the response shape at `runs.rs:201-207`, and `oneshot_response` holds its own `DropGuard` at `runs.rs:393`. Axum is therefore **already correct**; this task adds no source change. The test exists so a refactor sinking `spawn_writer` below the transport branch fails loudly instead of silently reintroducing SMA-456's class.

- [ ] **Step 1: Add the hanging-agent and server fixtures**

The existing fixtures cannot express this test: `ScriptedAgent` (`support/mod.rs:23`) completes immediately, `OrderingAgent` (`support/mod.rs:250`) sleeps only 20ms, and `spawn_echo_server` (`support/mod.rs:70`) returns only a `SocketAddr` — leaving the builder's default `InMemorySessionProvider` unreachable from the test.

Append to `crates/paigasus-helikon-runtime-axum/tests/support/mod.rs`:

```rust
/// A test [`Agent`] that signals on `started` from its FIRST stream element,
/// then hangs for 30s before it would emit `RunCompleted`.
///
/// Used to model a client that walks away mid-run. The signal exists because a
/// one-shot client receives nothing until the run ends, so there is no frame to
/// key a disconnect off and a fixed sleep would race the run's start.
pub struct SignallingHangingAgent {
    /// Fires once, from the first stream element, when the run has started.
    pub started: tokio::sync::mpsc::UnboundedSender<()>,
}

#[async_trait]
impl<Ctx: Send + Sync + 'static> Agent<Ctx> for SignallingHangingAgent {
    fn name(&self) -> &str {
        "hanging"
    }

    fn description(&self) -> &str {
        "test agent that signals run start then hangs"
    }

    async fn run(
        &self,
        _ctx: RunContext<Ctx>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let started = self.started.clone();
        let first = stream::once(async move {
            let _ = started.send(());
            AgentEvent::RunStarted {
                agent: "hanging".to_owned(),
            }
        });
        let hangs = stream::once(async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            }
        });
        Ok(first.chain(hangs).boxed())
    }
}

/// Spawn an [`AgentServer`] mounting a single `hanging` [`SignallingHangingAgent`]
/// and return both the bound address and the injected session provider.
///
/// Unlike [`spawn_echo_server`], this hands back the [`InMemorySessionProvider`] so
/// a test can assert on what the run actually persisted — the builder's default
/// provider is otherwise unreachable from outside the server.
pub async fn spawn_hanging_server(
    started: tokio::sync::mpsc::UnboundedSender<()>,
) -> (SocketAddr, Arc<InMemorySessionProvider>) {
    let sessions = Arc::new(InMemorySessionProvider::new(16));
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(SignallingHangingAgent { started }))
        .session_provider(Arc::clone(&sessions) as Arc<dyn SessionProvider>)
        .build()
        .expect("server builds");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        server
            .serve_with_listener(listener)
            .await
            .expect("serve loop");
    });

    (addr, sessions)
}
```

Then extend the `paigasus_helikon_runtime_axum` import at `support/mod.rs:19` from:

```rust
use paigasus_helikon_runtime_axum::AgentServer;
```

to:

```rust
use paigasus_helikon_runtime_axum::{AgentServer, InMemorySessionProvider, SessionProvider};
```

- [ ] **Step 2: Add the lock-in test**

Append to `crates/paigasus-helikon-runtime-axum/tests/concurrency.rs`. Note it uses a raw `TcpStream`, not `reqwest` (which every other test in this file uses at `:48, :92, :180`) — dropping a `reqwest::send()` future is not a reliable mid-run TCP disconnect.

```rust
/// A one-shot client that disconnects mid-run must still get its turn persisted.
///
/// `runtime-axum` already satisfies this: `create_run` calls `spawn_writer` for
/// EVERY transport before it branches on the response shape, so the run is always
/// driven by a detached task that drains to a terminal. This test is a lock-in —
/// it fails loudly if a refactor ever sinks `spawn_writer` below the transport
/// branch, which would reintroduce the class SMA-456 fixed in `runtime-agentcore`.
///
/// The 30s agent hang against a 10s poll window is deliberate: a shorter hang
/// would let a NON-cancelling implementation pass, because the run would finalize
/// naturally inside the window. Do not shorten it.
#[tokio::test]
async fn oneshot_client_disconnect_still_finalizes_the_session() {
    use paigasus_helikon_runtime_axum::SessionProvider;
    use tokio::io::AsyncWriteExt as _;

    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let (addr, sessions) = support::spawn_hanging_server(started_tx).await;

    let session_id = "sma456-oneshot";
    let body = r#"{"prompt":"hi"}"#;
    let len = body.len();
    let request = format!(
        "POST /agents/hanging/runs HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Content-Type: application/json\r\n\
         X-Session-Id: {session_id}\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}"
    );

    {
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();

        // Wait until the run has demonstrably started server-side, then let
        // `client` drop at the end of this block — a real mid-run disconnect.
        tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
            .await
            .expect("timed out waiting for the run to start")
            .expect("agent signalled run start");
    }

    let session = sessions.session(Some(session_id)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = session.snapshot().await.unwrap();
            if !snapshot.messages.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("session was never finalized after the one-shot client disconnected");
}
```

If `Duration` is not already imported in `concurrency.rs`, add `use std::time::Duration;` at the top.

- [ ] **Step 3: Run the lock-in test — it must pass with NO source change**

Run:
```bash
cargo test -p paigasus-helikon-runtime-axum --test concurrency oneshot_client_disconnect_still_finalizes_the_session
```

Expected: **PASS**. This is a characterization test, so GREEN-on-first-run is correct and confirms the spec's "axum is already correct" finding.

If it FAILS, do **not** patch `runs.rs` to make it pass. Stop and report — a failure means the spec's audit of axum was wrong and the ticket's scope changes.

- [ ] **Step 4: Run the crate's whole suite**

Run:
```bash
cargo test -p paigasus-helikon-runtime-axum --all-features
```

Expected: PASS. `support/mod.rs` is `#![allow(dead_code)]` module-wide, so the new fixtures won't trip unused warnings in the test binaries that don't use them.

- [ ] **Step 5: Format and lint**

Run:
```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-axum --all-features --all-targets -- -D warnings
```

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-runtime-axum/tests/support/mod.rs crates/paigasus-helikon-runtime-axum/tests/concurrency.rs
git commit -m "test(runtime-axum): SMA-456 lock in one-shot finalize on disconnect

runtime-axum's one-shot path is already safe -- create_run spawns the writer
for every transport before branching on the response shape -- but nothing
enforced that ordering. Pin it so a refactor sinking spawn_writer below the
branch fails loudly instead of silently reintroducing SMA-456's class.

Adds a signalling hanging agent and a spawn helper that exposes the session
provider; the existing fixtures complete too fast and hide it.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Document `Runner::run`'s drop hazard and persistence contract

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs:68-91` (the `run` doc block)

**Interfaces:**
- Consumes: nothing.
- Produces: docs only — no signature change, no behavior change.

**Context:** `run_streamed` already warns that dropping its stream skips finalize (`runner.rs:95-98`). `run` has no analogous warning about dropping its *future* — the exact trap behind SMA-456, and one any SDK user calling `runner.run()` from their own handler will hit. The **positive contract** half matters just as much: Task 1's fix is only sufficient because `run`'s future persists before it resolves. That is currently promised nowhere, even though `AgentCoreServerBuilder::runner` accepts any `Arc<dyn Runner<Ctx>>`.

Phrase both at **trait level** (a general truth about futures and about what `run` guarantees), not as a `TokioRunner` implementation claim.

- [ ] **Step 1: Add the two doc paragraphs**

In `crates/paigasus-helikon-core/src/runner.rs`, in the doc block for `run`, insert **after** the `**With a `Session`** ...` paragraph (which ends `... use [`Runner::resume`].`) and **before** the `**Cancellation/timeout is best-effort ...**` paragraph:

```rust
    /// **The returned future must be polled to completion for the run's events to
    /// be persisted.** Finalization happens inside this future, so — as with any
    /// future — dropping it early cancels the work still in flight, including the
    /// session write, and the turn is lost. A caller that cannot guarantee it will
    /// poll to completion (an HTTP handler whose client may disconnect, say) should
    /// drive the run on a detached task and await its result over a channel, rather
    /// than awaiting it inline.
    ///
    /// **Conversely, this future performs the session write before it resolves**, so
    /// callers may treat its resolution as a persistence barrier: once `run` returns,
    /// the turn is durable in the `Session`. This is what makes "detach and await the
    /// result over a channel" a sufficient remedy for the hazard above.
```

- [ ] **Step 2: Verify the docs build clean under `-D warnings`**

Run:
```bash
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps
```

Expected: clean. Watch for `rustdoc::broken_intra_doc_links` — the text deliberately uses plain prose (no new `[`...`]` links) except the pre-existing `[`Runner::resume`]`, so there should be nothing new to resolve.

- [ ] **Step 3: Verify core's tests still pass**

Run:
```bash
cargo test -p paigasus-helikon-core --all-features
```

Expected: PASS. (Doc comments can contain doctests; this addition has no code fences, so nothing new runs.)

- [ ] **Step 4: Format**

Run:
```bash
cargo fmt --all
```

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/runner.rs
git commit -m "docs(core): SMA-456 document Runner::run's drop hazard and persistence barrier

run_streamed warns that dropping its stream skips finalize; run had no
analogous warning about dropping its future -- the trap behind SMA-456. Also
state the positive contract: run's future persists the turn before it
resolves, which is what makes detach-and-await a sufficient remedy.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Document agentcore's disconnect semantics in the book

**Files:**
- Modify: `docs/book/src/concepts/runtimes.md` (the `## paigasus-helikon-runtime-agentcore — managed AWS deployment` section, which runs `:40-55`)

**Interfaces:**
- Consumes: nothing.
- Produces: docs only.

**Context:** `axum-server.md:87` documents this property for axum ("one-shot and SSE responses hold a `CancellationToken` drop-guard so a client disconnect cancels the run"), while the agentcore section says nothing. CLAUDE.md requires the book track user-facing changes in the same PR. The same-session overlap note is required by the spec's gate decision (document only, no follow-up ticket).

- [ ] **Step 1: Add the disconnect paragraph**

In `docs/book/src/concepts/runtimes.md`, insert a new paragraph **immediately before** the `**Termination is abrupt**` paragraph (currently `:55`):

```markdown
**Client disconnects**: both `/invocations` transports — buffered JSON and SSE — drive the run on a detached task and hold a `CancellationToken` drop-guard, so a client that walks away mid-run cancels the run *and* still gets its turn finalized into the `Session`. The persisted turn is partial (whatever the run had produced when it was cancelled), which is the deliberate trade: a disconnected client should not keep burning model tokens. Note that agentcore has no per-session serialization lock (unlike [`runtime-axum`](./axum-server.md)), so a cancelled run's finalize can overlap a retry of the same session id — AgentCore pins a session to a container, which makes genuinely concurrent same-session invocations unlikely enough in practice that the lock isn't worth its cost here.
```

- [ ] **Step 2: Build the book and verify the link check passes**

Run:
```bash
mdbook build docs/book
```

Expected: clean, no warnings. `[output.linkcheck] warning-policy = "error"`, so the relative link `./axum-server.md` must resolve — it is a sibling page in the same directory, so it will.

If `mdbook` is not installed, report that rather than skipping the check.

- [ ] **Step 3: Commit**

```bash
git add docs/book/src/concepts/runtimes.md
git commit -m "docs(docs): SMA-456 document agentcore disconnect semantics in the book

The book states axum's disconnect/cancellation property but said nothing for
agentcore. Record that both /invocations transports cancel and still finalize
on disconnect, the partial-turn trade, and the same-session overlap agentcore
has no SessionLocks to prevent.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Full CI gate sweep

**Files:** none (verification only)

**Interfaces:**
- Consumes: Tasks 1-4 committed.
- Produces: confidence that CI will pass.

**Context:** These are the exact gates from CLAUDE.md, job-for-job. Run them from the worktree root. Per the `--all-features` memory note, run the **workspace-wide** test gate exactly as written — a per-crate run can mask cross-crate feature-unification failures.

- [ ] **Step 1: Run every fast gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: both clean.

- [ ] **Step 2: Run the full workspace test gate**

```bash
cargo test --workspace --all-features
```

Expected: PASS. This is the gate CI runs; do not substitute per-crate runs.

- [ ] **Step 3: Run the docs gate**

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: clean, apart from the **known, accepted** `paigasus-helikon` lib-vs-CLI-binary filename-collision warning documented in CLAUDE.md. Do not "fix" that one.

- [ ] **Step 4: Verify no version fields were touched**

```bash
git diff main...HEAD --stat
git diff main...HEAD -- '*/Cargo.toml' 'Cargo.toml' 'release-plz.toml' '*CHANGELOG.md'
```

Expected: the second command prints **nothing**. If it prints anything, a version/manifest edit slipped in — revert it. This PR is release-plz pure-auto; no manual bumps.

- [ ] **Step 5: Report**

Report each gate's actual result. If any gate failed, report the failure output verbatim rather than claiming success.

---

## Self-Review

**Spec coverage:**

| Spec section | Task |
| --- | --- |
| §1 Detach `run_json` (+ `oneshot` import, rename + comment, doc block, `# Errors`, tracing) | Task 1, Steps 3-6 |
| §2 Regression test (SignallingSlowAgent, 30s/10s, raw TcpStream, RED first) | Task 1, Steps 1-2, 7 |
| §3 Lock-in test (3 fixtures: hanging agent, provider-exposing spawn helper, raw TcpStream) | Task 2 |
| §4 Core docs (drop warning **+** positive persistence contract) | Task 3 |
| §4 Book (disconnect semantics + same-session overlap) | Task 4 |
| Release mechanics (no manual bumps) | Global Constraints; Task 5 Step 4 |
| Out of scope (runner-side FinalizeGuard, SessionLocks, MCP, shutdown, READMEs) | No tasks — correct |

**Type consistency:** `run_json(state, ctx, cancel, input)` — the `cancel: CancellationToken` third parameter is consistent between Task 1 Step 4 (call site) and Step 6 (definition), and matches `run_sse`'s existing parameter order. `SignallingSlowAgent` (agentcore, `Agent<()>`) and `SignallingHangingAgent` (axum, generic `Agent<Ctx>`) are deliberately distinct types in different crates — the axum one is generic because `support/mod.rs`'s existing agents are. `spawn_hanging_server` returns `(SocketAddr, Arc<InMemorySessionProvider>)` in both its definition (Task 2 Step 1) and its use (Step 2).

**Placeholder scan:** none — every code step carries complete code, and every command carries its expected output.
