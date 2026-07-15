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

**Provenance.** The spike was written against `invoke.rs`'s test module and reverted after
measurement; it is not preserved as a separate commit. Its design survives verbatim as the
regression test in §2 below, which is the same harness with the hang restored to 30s — so a
reviewer re-runs the evidence by checking out the parent of the fix commit and running the new
regression test, which must fail RED there.

**The spike used `Connection: close`** (matching the SSE test at `invoke.rs:719`). Whether hyper's
h1 dispatcher notices a peer close while parked on the response future can depend on it still
polling the read half, so a keep-alive client behind AgentCore's platform proxy might not reproduce
the drop. **This does not weaken the fix, which is strictly better either way:** if hyper drops the
future, the detached task still finalizes; if hyper does *not* drop it, the run completes naturally
and the session is written as before. Only the prompt-cancellation optimization depends on the drop
being detected.

## Findings that correct the ticket

**`runtime-axum`'s one-shot path is not gapped.** The ticket asks to "verify"; verification says
its one-shot is already correct. `create_run` (`handlers/runs.rs:188-207`) calls `spawn_writer` at
step 6 — for *every* transport — before branching on the response shape at step 7. The run is
therefore always driven by a detached task that drains to terminal, and `oneshot_response`
additionally holds a `DropGuard`. No axum code change is required; the deliverable there is a test
that locks the property in.

## Rejected alternative: fix it inside the runner

An earlier draft of this spec claimed the fix *cannot* live in the runner, because `Runner::run`
takes `agent: &(dyn Agent<Ctx> + '_)` — a non-`'static` reference — so a `Runner` cannot
`tokio::spawn` its own body. **That claim is false and has been removed.** The `&dyn Agent`
lifetime does block spawning the *whole* body, but finalize does not need the agent:
`finalize(session, recorder)` (`runtime-tokio/src/lib.rs:118`) takes only `Arc<dyn Session>` and
`Arc<Mutex<SessionRecorder>>` — both already owned and both `'static`. A `FinalizeGuard` whose
`Drop` spawns finalize would therefore fix SMA-456 *inside* `TokioRunner::run`, with no signature
change, for **every** caller.

It is rejected on merits rather than on feasibility:

- **It cannot deliver this spec's disconnect policy.** A `Drop`-guard fires only once the future is
  already being dropped; it can persist what the recorder happened to hold, but there is no run
  left to cancel or drain, and no result to return to a still-connected client. `run_json` must
  await the run regardless, so the handler-side work does not disappear.
- **`tokio::spawn` from `Drop` is a real hazard.** It requires an ambient runtime handle and
  panics without one — and the moment it is most likely to fire (runtime shutdown, teardown) is
  exactly when `Handle::current()` is least reliable. It also needs disarming on the normal path or
  it double-finalizes.
- **It silently changes `Runner::run`'s semantics for every existing caller**, some of which may
  reasonably rely on "dropped future ⇒ no write".
- **SMA-422 already owns that territory** — the shared-core hoist of finalize/cancel-precedence
  logic for durable runners. Pre-empting it here would conflict.
- **The ticket asks for the SSE pattern** ("same decoupled-driver pattern as SSE"), and mirroring
  `run_sse` keeps agentcore's two transports structurally identical.

The call site remains the right place to fix this — but because of the reasons above, not because
the runner-side fix is impossible.

## Decisions

| Decision | Choice | Rationale |
| --- | --- | --- |
| Fix vs document | **Detach the run** | The spike proves permanent, silent data loss; documenting it would bless that in a session-backed SDK. |
| Disconnect policy | **Cancel the run** | Matches `run_sse` and axum's `oneshot_response`, so all three transports agree; avoids burning model tokens for an absent client. Accepted cost: the persisted turn is partial. |
| Same-session overlap | **Accept and document** | See below — a newly-persisting cancelled run can now overlap a retry. Pre-existing for SSE since SMA-332; this fix extends it to JSON. |
| Observability | **Add tracing** | The detached path is otherwise silent in exactly the scenario the ticket exists for. |
| Core docs | **Drop warning + persistence contract** | `run_streamed` warns about dropping the stream; `run` has no analogous warning about dropping the future — the exact trap behind this bug. The contract half is load-bearing: see below. |
| Book | **Document agentcore's semantics** | `axum-server.md:87` states this property for axum; `runtimes.md` says nothing for agentcore. |

### The runner contract this fix depends on

The fix works only because `TokioRunner::run` does `collect().await` → `finalize().await` → return
(`runtime-tokio/src/lib.rs:169-172`), so `rx.await` resolving *implies the session write already
landed*. That property is nowhere documented: `Runner::run`'s docs
(`core/src/runner.rs:68-91`) say only "the run's events are persisted at exit", and
`AgentCoreServerBuilder::runner` (`agentcore/src/server.rs:123`) accepts **any**
`Arc<dyn Runner<Ctx>>`. A custom runner that resolves before persisting would get zero benefit from
this fix, silently.

Since this design argues the fix belongs at the call site, that contract becomes load-bearing and
must be stated. The core docs deliverable therefore carries **both** halves: the drop warning *and*
the positive contract ("`run`'s future performs the session write before it resolves; callers may
treat its resolution as a persistence barrier"). Without the second, the warning tells callers to
detach without telling them why detaching is sufficient.

### Same-session overlap (accepted)

`AppStateInner` (`agentcore/src/server.rs:37-50`) has **no** `locks` field — agentcore has no
equivalent of axum's `SessionLocks` (`axum/src/handlers/runs.rs:171-198`), which acquires a
per-session lock before the run and holds it for the run's duration.

Today a JSON-mode disconnect tears the run down instantly and it **never appends**. After this fix
the cancelled run continues briefly and *does* append. Because AgentCore pins a session id to a
container and may retry an invocation, a retry's `load_and_record` snapshot
(`runtime-tokio/src/lib.rs:97-113`) can be taken *before* the previous run's `finalize` lands —
yielding stale history plus a duplicated or out-of-order user message.

This is accepted for this ticket, and noted in the book alongside the disconnect line. It is not
introduced here: SMA-332's SSE fix has the same property, and this fix brings JSON into line with
it rather than creating a new class.

**Decided at the spec gate (2026-07-15): document only, no follow-up ticket.** AgentCore pins a
session id to a container, which makes genuinely concurrent same-session invocations unrealistic
enough in practice not to warrant `SessionLocks` machinery in agentcore. The overlap is recorded
here and in the book so the reasoning is discoverable if that assumption ever breaks.

## Design

### 1. Detach `run_json`

`run_json` takes the run's `CancellationToken`, spawns the run, and awaits the result over a
`oneshot` channel:

Requires widening the import at `invoke.rs:43` to `use tokio::sync::{mpsc, oneshot};`.

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
        if tx.send(result).is_err() {
            // The client disconnected: nobody is left to receive the outcome.
            // The run's session write has already happened (finalize runs before
            // `run` resolves), so this is bookkeeping, not a lost turn.
            tracing::debug!("invocation client disconnected; run outcome discarded");
        }
    });

    let _disconnect = cancel.drop_guard();
    let result = rx
        .await
        .map_err(|_| {
            tracing::error!("run task ended without reporting a result (panic or runtime shutdown)");
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

In `invocations`, the retained token clone is renamed `cancel_for_sse` → `cancel_for_run` (both
branches now consume it) and passed to `run_json`. **The comment at `invoke.rs:152-154` must be
updated too** — it currently says "`run_sse` needs its own handle on the token", which is wrong
once both branches use it.

`run_json` also gains a `# Disconnect semantics` doc block mirroring `run_sse`'s
(`invoke.rs:199-229`), and `invocations`' `# Errors` list (`invoke.rs:127-133`) gains the new
`"run task ended without a result"` (500) case.

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
if the disconnect both cancels **within the poll window** *and* still finalizes — pinning both
halves of the contract. Do not "simplify" the 30s down.

Precisely: this pins "cancels within ~10s", not "cancels promptly". A true promptness bound is not
asserted, and `controlled`'s `biased` select polls the agent stream before the cancel branch
(`runtime-tokio/src/lib.rs:71-73`), so an always-ready agent stream could starve cancellation
entirely — pre-existing and out of scope here, but the reason this spec avoids claiming
"promptly".

### 3. Lock-in test (axum)

`crates/paigasus-helikon-runtime-axum/tests/concurrency.rs` gains a one-shot disconnect test of the
same 30s/10s shape, asserting the session is finalized after a one-shot client disconnects mid-run.
Axum already passes this; the test exists so that a refactor sinking `spawn_writer` below the
transport branch fails loudly instead of silently reintroducing SMA-456's class.

**This is not free — it needs three new fixtures**, which an earlier draft of this spec hid by
billing it as "axum already passes this". The existing harness cannot express the test:

1. **A hanging agent.** `support/mod.rs` has none — `ScriptedAgent` (`support/mod.rs:23`)
   completes immediately and `OrderingAgent` (`support/mod.rs:250`) sleeps only 20ms. Add a
   signalling hanging agent mirroring agentcore's `SignallingSlowAgent`.
2. **A spawn helper that exposes the session provider.** `spawn_echo_server() -> SocketAddr`
   (`support/mod.rs:70`) returns only the address, and the builder's default
   `InMemorySessionProvider` (`axum/src/server.rs:222`) is unreachable from the test. Add a variant
   that injects `Arc<InMemorySessionProvider>` via `.session_provider(...)` (`axum/src/server.rs:138`)
   and returns `(SocketAddr, Arc<InMemorySessionProvider>)`.
3. **A raw `TcpStream` client.** Every test in `concurrency.rs` drives the server through `reqwest`
   (`concurrency.rs:48, 92, 180`); dropping a `reqwest::send()` future is not a reliable mid-run TCP
   disconnect. Use a raw `TcpStream`, as the agentcore SSE test does (`invoke.rs:725`).

### 4. Documentation

- `crates/paigasus-helikon-core/src/runner.rs` — `Runner::run` gains **both**:
  1. the drop warning mirroring `run_streamed`'s, phrased at trait level (dropping the future
     cancels in-progress work, including the finalize step that persists the turn) rather than as a
     `TokioRunner` implementation claim, with the guidance that callers who cannot guarantee polling
     to completion should drive the run on a detached task and await the result over a channel; and
  2. the positive persistence contract — `run`'s future performs the session write before it
     resolves, so callers may treat its resolution as a persistence barrier. This is what makes
     "detach and await the result" a *sufficient* fix rather than merely a plausible one.
- `docs/book/src/concepts/runtimes.md` — the agentcore section gains the disconnect semantics
  (both `/invocations` transports cancel the run on client disconnect while still finalizing the
  session), plus a note on the same-session overlap described above.

## Out of scope

- Changing `Runner::run`'s signature to allow internal spawning (breaking change; no ticket).
- A runner-side `FinalizeGuard` — see "Rejected alternative" above.
- Hoisting the shared finalize/cancel-precedence logic into core — already tracked as SMA-422.
- `SessionLocks` parity for agentcore — follow-up candidate, see "Same-session overlap".
- `runtime-axum` behavior changes — it is already correct.
- **MCP mode** (`server.rs:271`) — unaffected: each MCP call gets a fresh, unshared in-memory
  session by construction (`runtimes.md:55`), so there is no cross-invocation turn to lose.
  Conscious skip, not an oversight.
- **Graceful shutdown of detached runs** — `AgentCoreServer::serve` (`server.rs:287-298`) has none,
  and AgentCore documents no `SIGTERM` contract (`runtimes.md:55`). A detached run dropped at
  runtime shutdown is an accepted non-goal, and introduces no new exposure: `tx.send` happens
  *after* `finalize`, so a `200` is never returned before the write has landed.
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
