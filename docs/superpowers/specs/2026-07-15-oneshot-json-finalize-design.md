# SMA-456 — Finalize one-shot JSON runs on client disconnect

**Status:** approved (2026-07-15)
**Linear:** [SMA-456](https://linear.app/smaschek/issue/SMA-456)
**Branch:** `feature/sma-456-runtime-agentcore-runtime-axum-finalize-one-shot-json-runs`

## Problem

`TokioRunner::run` (`crates/paigasus-helikon-runtime-tokio/src/lib.rs:169-172`) collects the
run's event stream and then calls `finalize(&session, &recorder)` — both inline in a single
future:

```rust
let collected = RunResultStreaming::with_failure(recorded, failure).collect().await;
finalize(&session, &recorder).await;
```

`runtime-agentcore`'s `run_json` (`crates/paigasus-helikon-runtime-agentcore/src/invoke.rs:186`)
awaits that call *inside the axum handler future*. When a client disconnects mid-run, axum drops
the handler future, `finalize` never executes, and the turn's session write is silently lost.

SMA-332's review already fixed the sibling SSE path (`run_sse`) with a detached driver task plus a
`DropGuard` over the run's `CancellationToken`. The buffered JSON path never got the same
treatment.

## Evidence (spike, 2026-07-15)

The ticket's premise was verified empirically rather than assumed, because a plausible alternative
existed: hyper might not detect a disconnect on a *buffered* response until it tries to write, in
which case the handler future would run to completion and the write would merely be delayed, not
lost.

A spike added a JSON-mode disconnect test (real TCP, `Accept: application/json`, disconnect gated
on the agent signalling that the run had started server-side):

| Variant | Agent hang | Poll window | Result |
| --- | --- | --- | --- |
| Disconnect | 30s | 10s | FAIL — session empty |
| Disconnect | **2s** | **10s** | **FAIL — session still empty** |
| Control (no disconnect) | 2s | 10s | PASS — session finalized |

The 2s/10s disconnect variant is the decisive one: the agent would have completed at t=2s, well
inside the 10s poll window, yet the session was *still* empty. That rules out "delayed" and proves
"lost". The passing control — identical harness, identical agent, only the disconnect removed —
proves the disconnect is the cause rather than a broken fixture.

**Conclusions:**

1. axum 0.8 / hyper (HTTP/1.1) **does** drop the handler future on client disconnect for buffered
   responses.
2. The bug is **live**, not latent, and the loss is **permanent**.

## Findings that correct the ticket

**`runtime-axum`'s one-shot path is not gapped.** The ticket asks to "verify"; verification says
its one-shot is already correct. `create_run` (`handlers/runs.rs:188-207`) calls `spawn_writer` at
step 6 — for *every* transport — before branching on the response shape at step 7. The run is
therefore always driven by a detached task that drains to terminal, and `oneshot_response`
additionally holds a `DropGuard`. No axum code change is required; the deliverable there is a test
that locks the property in.

**The fix cannot live in the runner.** `Runner::run` takes `agent: &(dyn Agent<Ctx> + '_)` — a
non-`'static` reference. A `Runner` implementation cannot `tokio::spawn` its own body to protect
finalize without a breaking trait-signature change. The call site is the correct place to fix this.

## Decisions

| Decision | Choice | Rationale |
| --- | --- | --- |
| Fix vs document | **Detach the run** | The spike proves permanent, silent data loss; documenting it would bless that in a session-backed SDK. |
| Disconnect policy | **Cancel the run** | Matches `run_sse` and axum's `oneshot_response`, so all three transports agree; avoids burning model tokens for an absent client. Accepted cost: the persisted turn is partial. |
| Core docs | **Add the drop warning** | `run_streamed` warns about dropping the stream; `run` has no analogous warning about dropping the future — the exact trap behind this bug, and one any SDK user calling `runner.run()` in their own handler will hit. |
| Book | **Document agentcore's semantics** | `axum-server.md:87` states this property for axum; `runtimes.md` says nothing for agentcore. |

## Design

### 1. Detach `run_json`

`run_json` takes the run's `CancellationToken`, spawns the run, and awaits the result over a
`oneshot` channel:

```rust
async fn run_json<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    ctx: RunContext<Ctx>,
    cancel: CancellationToken,
    input: AgentInput,
) -> Result<Response, AgentCoreError> {
    let runner = Arc::clone(&state.runner);
    let agent = Arc::clone(&state.agent);
    let run_config = state.run_config.clone();
    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        let result = runner.run(agent.as_ref(), ctx, input, run_config).await;
        let _ = tx.send(result); // ignore: the client may already be gone
    });

    let _disconnect = cancel.drop_guard();
    let result = rx
        .await
        .map_err(|_| AgentCoreError::Internal("run task ended without a result".to_owned()))?
        .map_err(|e| AgentCoreError::Internal(format!("run failed: {e}")))?;

    Ok(Json(InvocationResponse {
        final_output: result.final_output,
        usage: result.usage,
    })
    .into_response())
}
```

In `invocations`, the retained token clone is renamed `cancel_for_sse` → `cancel_for_run` (both
branches now consume it) and passed to `run_json`.

**Implementation traps to preserve:**

- The guard MUST bind to a named `_disconnect`, never `let _ = cancel.drop_guard()`. The latter
  drops the guard immediately and cancels every run the instant it starts.
- `agent.as_ref()` borrows an `Arc` owned *by the async block*. This is what makes the
  non-`'static` `&dyn Agent<Ctx>` legal inside `tokio::spawn`; `run_sse` already relies on the
  same construction.
- Dropping the guard after a clean completion is a harmless no-op (cancelling a finished run does
  nothing).

**Behavioral notes:** a panic inside the run is now isolated to the detached task and surfaces as a
500 (`run task ended without a result`) instead of propagating into the connection task.

### 2. Regression test (agentcore)

Mirrors `sse_client_disconnect_still_finalizes_the_session`, in `invoke.rs`'s test module.

A `SignallingSlowAgent` sends on an `UnboundedSender` from its **first stream element**, then hangs
before it would emit `RunCompleted`. The signal is required because a JSON client receives nothing
until the run ends — unlike SSE there is no frame to key the disconnect off, and a fixed sleep
would be flaky.

Shape: bind a real `TcpListener`, `axum::serve` the router, connect a raw `TcpStream`, write the
request with `Accept: application/json`, await the start signal, drop the client, then poll
`session.snapshot()` for a non-empty `messages`. Assert the turn's `UserMessage` is persisted.

**The timings are load-bearing.** The agent hangs **30s** while the test polls for **10s**. A
shorter hang (e.g. the spike's 2s) would let a *non-cancelling* implementation pass, because the
run would finalize naturally inside the poll window. With hang > poll window, the test passes only
if the disconnect both cancels promptly **and** still finalizes — pinning both halves of the
contract. Do not "simplify" the 30s down.

### 3. Lock-in test (axum)

`crates/paigasus-helikon-runtime-axum/tests/concurrency.rs` gains a one-shot disconnect test of the
same 30s/10s shape, asserting the session is finalized after a one-shot client disconnects
mid-run. Axum already passes this; the test exists so that a refactor sinking `spawn_writer` below
the transport branch fails loudly instead of silently reintroducing SMA-456's class.

### 4. Documentation

- `crates/paigasus-helikon-core/src/runner.rs` — `Runner::run` gains a drop warning mirroring
  `run_streamed`'s. Phrased at trait level (dropping the future cancels in-progress work, including
  the finalize step that persists the turn) rather than as a `TokioRunner` implementation claim,
  with the guidance that callers who cannot guarantee polling to completion should drive the run on
  a detached task and await the result over a channel.
- `docs/book/src/concepts/runtimes.md` — one line in the agentcore section documenting that both
  `/invocations` transports cancel the run on client disconnect while still finalizing the session.

## Out of scope

- Changing `Runner::run`'s signature to allow internal spawning (breaking change; no ticket).
- Hoisting the shared finalize/cancel-precedence logic into core — already tracked as SMA-422.
- `runtime-axum` behavior changes — it is already correct.
- Crate README edits — the agentcore README documents install/usage and never mentions
  cancellation, so it is not drifting. Conscious skip, per CLAUDE.md's README rule.

## Release mechanics

No manual version bumps. `runtime-agentcore` is already released and this PR adds no new core API,
so release-plz's pure-auto path applies (the same-PR manual-core-bump ritual is only for stubs
ascending from `0.0.0` against same-PR core API). The core docs touch will cause the squashed
commit to re-release `core` and cascade dependents — harmless, per SMA-421.

## Verification

- `cargo test -p paigasus-helikon-runtime-agentcore` (new regression test RED before the fix, GREEN
  after).
- `cargo test -p paigasus-helikon-runtime-axum` (new lock-in test GREEN without any source change).
- Full CI gates: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-features
  --all-targets -- -D warnings`, `cargo test --workspace --all-features`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`, doc-coverage, and
  `mdbook build docs/book`.
