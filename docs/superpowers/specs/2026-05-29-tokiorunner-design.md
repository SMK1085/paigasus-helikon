# SMA-321 — TokioRunner: cancellation, timeouts, parallel tool calls

**Status:** Design (approved)
**Issue:** [SMA-321](https://linear.app/smaschek/issue/SMA-321)
**Branch:** `feature/sma-321-tokiorunner-cancellation-timeouts-parallel-tool-calls`
**Date:** 2026-05-29
**Blocks:** [SMA-346](https://linear.app/smaschek/issue/SMA-346) (structured `AgentError` at the Runner boundary)
**Supersedes (planned-design reconciliation):** see §1.1 and ADR-13.

## 1. Summary

Implement `TokioRunner`, the default ephemeral execution backend, as a concrete
`Runner<Ctx>` in `paigasus-helikon-runtime-tokio`. It adds run-level execution
control — cancellation, timeout, and bounded tool-call concurrency — on top of the
agent loop that already exists in `LlmAgent::run`.

`TokioRunner` is **thin**: it consumes `agent.run()`'s `AgentEvent` stream and wraps
it with tokio-specific control at the boundary. It does **not** own or re-drive the
loop — the `Agent` owns the driver, and the pure `transition` state machine is the
durability seam (ADR-13). The functional changes the runner needs from the loop
(bounded concurrency, reading new `RunConfig` fields) are made **in place** in
`core::agent`; no driver is extracted.

### 1.1 Relationship to the planned design (this supersedes stale docs)

The original Notion/Linear planning predates SMA-313/314 and contradicts the as-built
code. Reconciled on 2026-05-29:

- **ADR-13** *"Runner is object-safe; the Agent owns the loop driver"* was written to
  record the decision the code already embodies. It refines (does not overturn) ADR-6.
- The **Agent Loop & State Machine** page's *"the runner drives `LoopState`"* sentence
  was corrected to *"the Agent owns the loop driver; a Runner consumes the stream."*
- The **SMA-321 scope bullets** ("owns the LoopState driver", the `cancellation` /
  `retry_policy` `RunConfig` fields, "select! at every await point") were trimmed to
  match this design.
- ADR-6 and the **Core Primitives** page still show the earlier *generic*
  `Runner<A: Agent>` sketch; ADR-13 supersedes that sketch wherever it appears. (Those
  two pages were left as historical record per the agreed scope; ADR-13 is the
  authority.)

## 2. Context: what already exists (SMA-313/314/319/320)

- **`transition` (`core::loop_state`)** — a pure, resumable state-machine step. No
  async, no IO. **The durability seam** a future Temporal/AgentCore runner reuses
  (driving it step-by-step with persistence between steps). Unchanged by this work.
- **The async driver — inside `LlmAgent::run` (`core::agent`)** — drives `transition`
  in a loop: calls the model, pumps the model event stream, runs tool calls
  concurrently (`run_tools_concurrent` via unbounded `join_all`), and yields a
  `BoxStream<'static, AgentEvent>`.
- **`Agent` trait** — opaque: exposes only `name`, `description`, and
  `run(ctx, input) -> BoxStream<AgentEvent>`. No access to the model, tools, or settings.
- **`Runner` trait** — **object-safe** (takes `&(dyn Agent<Ctx> + '_)`; ADR-13), with
  `run -> Result<RunResult, RunError>` and `run_streamed -> Result<RunResultStreaming, RunError>`.
- **`RunConfig`** — currently only `max_turns`, read from `LlmAgent.config`.
- **`RunContext`** — already owns the canonical `CancellationToken` (`ctx.cancel()`),
  session handle, hook registry, tracer, and user context.

Consequences that shape this design:

1. **The Runner cannot re-drive the loop for an opaque `&dyn Agent`.** It consumes the
   agent's event stream and adds boundary control (ADR-13).
2. **AC#2 and AC#3 are already satisfied** by the existing driver + `transition`
   (parallel tool calls run concurrently; events are emitted lifecycle → semantic →
   terminal). This work *bounds* concurrency and *preserves* ordering through the
   runner — it does not invent them.
3. **Cancellation already flows** `ctx.cancel()` → `model.invoke(req, cancel)`, and
   tools observe a child token via `ctx.to_tool_context()`.

## 3. Decisions

| # | Decision | Choice |
|---|----------|--------|
| D1 | Driver ownership / extraction | **No extraction.** Make functional changes (bounded concurrency, config threading) **in place** in `core::agent`. The durability seam is `transition`, not the async driver, so a shared `core::driver` would serve no real second consumer (ADR-13). *(Chosen over extracting a shared `core::driver`: nothing in SMA-321 consumes it, and durable runners reuse `transition`, not the async driver.)* |
| D2 | How per-invocation `RunConfig` reaches the driver | **Thread via `RunContext`.** The runner installs the effective config into the context; the driver reads it. No `Agent` trait change. |
| D3 | Session integration scope | **Defer entirely.** A no-op `finalize()` seam (in *both* runner methods) marks where persistence + compaction land in a follow-up. |
| D4 | Cancellation mechanism | **Drop-based at the runner boundary.** `biased` `select!` on the cancel token / deadline; dropping the stream cancels nested awaits within one poll. |
| D5 | `retry_policy` | **Omit entirely from SMA-321.** Do not ship an inert public knob on the published 0.1.0 surface; `#[non_exhaustive]` lets it be added later with its mechanism. *(An inert config knob on a published surface is worse than its absence; `#[non_exhaustive]` keeps the door open to add it later.)* |

**Hard constraint:** `paigasus-helikon-core` stays free of the tokio *runtime* (no
`tokio::time::sleep`, `tokio::spawn`, or `tokio::sync::Semaphore`). It depends on
`tokio-util` only for `CancellationToken`. This is why bounded concurrency uses
`futures_util::buffered` (§4.3) and why `timeout` is enforced only in the runtime crate
(§4.1 / §5).

## 4. Core changes (`paigasus-helikon-core`)

### 4.1 `RunConfig` (in `runner.rs`)

Grows two fields; stays `#[non_exhaustive]`; `Default` behavior unchanged.

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunConfig {
    /// [driver-scoped] Max model turns. Honored by the core loop driver,
    /// including on a bare `agent.run()` with no runner. Default 16.
    pub max_turns: u32,
    /// [runner-scoped] Wall-clock run deadline. Honored ONLY by a runtime
    /// backend (e.g. `TokioRunner`); a bare `agent.run()` cannot time out
    /// (core has no timer). `None` = no deadline.
    pub timeout: Option<Duration>,
    /// [driver-scoped] Cap on concurrent tool-call execution. Honored by the
    /// core loop driver. `None` = unbounded (today's behavior).
    pub parallel_tool_call_limit: Option<NonZeroUsize>,
}
```

- **`#[doc]` makes the split explicit** (`[driver-scoped]` vs `[runner-scoped]`) so
  "I set `timeout` and nothing happened" on the bare `agent.run()` path is impossible
  to hit blind.
- **No `cancellation` field** — `RunContext::cancel()` is the single canonical token
  (intentional deviation from the original ticket bullet).
- **No `retry_policy`** (D5).
- Builders `with_timeout` / `with_parallel_tool_call_limit` for ergonomics.

**Config dual-source, acknowledged.** Config now resolves from
`ctx.run_config()` (runner-injected) *or* `self.config` (agent field), by precedence
(§4.2). This is the "two sources" shape rejected for `cancellation` — the difference is
that `RunConfig` is *inert data* resolved once at run start, whereas a cancel token is a
*live signal* whose duplication would force ongoing reconciliation. The data dual-source
is acceptable; the live-signal one was not.

### 4.2 `RunContext` (in `context.rs`)

Carries the per-invocation `RunConfig` so the driver can read it.

```rust
run_config: Option<RunConfig>,   // new field; defaults to None

pub fn with_run_config(mut self, config: RunConfig) -> Self { self.run_config = Some(config); self }
pub fn run_config(&self) -> Option<&RunConfig> { self.run_config.as_ref() }
```

- `RunContext::new(...)` signature is **unchanged**; `run_config` defaults to `None`,
  so all current call sites (and `core`'s existing `noop_run_context` test helper)
  compile and behave identically.
- **`to_tool_context()` must NOT copy `run_config` into `ToolContext`** —
  tools have no business reading the run's timeout/concurrency policy. A doc comment on
  the field states it is the runner-injection channel, not general context state.
- **Precedence (resolved in `LlmAgent::run`):** effective config =
  `ctx.run_config().cloned().unwrap_or_else(|| self.config.clone())`.

### 4.3 In-place loop changes (no extraction)

The async driver stays inside `LlmAgent::run`. Two in-place edits:

1. **Resolve effective config** (§4.2 precedence) at the top of `run`, and use it for
   `max_turns` and `parallel_tool_call_limit` instead of reading `self.config` directly.
2. **Bounded, order-preserving tool concurrency** in `run_tools_concurrent`, using
   `futures_util` (no tokio):
   - `parallel_tool_call_limit == None` → keep `join_all` (unbounded; today's behavior).
   - `Some(n)` → `futures_util::stream::iter(futs).buffered(n.get()).collect().await`.

   `buffered` (not `buffer_unordered`) preserves call order in the outcome `Vec`, so
   `ToolResult` items keep a deterministic conversation order regardless of completion
   order.

No relocation of `async_stream`, `build_items`, `tool_output_to_content_parts`, or
`ToolCallAccum` — they remain in `core::agent`.

### 4.4 `RunError::Timeout`

Add an additive variant (`RunError` is `#[non_exhaustive]`):

```rust
/// The run exceeded its configured `RunConfig::timeout`.
#[error("run timed out")]
Timeout,
```

`RunError::Cancelled`, `Agent(AgentError)`, `Other(anyhow::Error)` already exist.

## 5. `TokioRunner` (`paigasus-helikon-runtime-tokio`)

Stateless: `pub struct TokioRunner;` + `Default`. Shared prelude in both methods:

```rust
let ctx = ctx.with_run_config(config.clone());
let cancel = ctx.cancel().clone();          // clone handles before ctx is moved
let session = ctx.session().clone();        // for the finalize seam (no-op now)
let stream = agent.run(ctx, input).await.map_err(RunError::Agent)?;
```

`agent.run` takes `RunContext` by value, so any handle `finalize` needs is cloned from
the context first (here, the `Arc<dyn Session>`).

### 5.1 Shared draining helper

Both methods route the agent stream through one private helper rather than duplicating
accumulation or the `select!` loop:

```rust
// pseudo-signature
fn controlled(
    stream: BoxStream<'static, AgentEvent>,
    cancel: CancellationToken,
    timeout: Option<Duration>,
) -> (BoxStream<'static, AgentEvent>, OutcomeHandle);
```

- The wrapper uses **`tokio::select! { biased; … }`** with the **stream branch first**,
  then cancel, then deadline. `biased` ordering means a run whose terminal event
  (`RunCompleted`) is ready in the same poll that cancel/deadline fires is reported as
  *completed*, not cancelled/timed-out (otherwise random selection makes
  AC#1 flaky). On the control branch it also does a final non-blocking drain of any
  already-ready terminal event before deciding.
- On cancel/deadline the wrapper records the reason in `OutcomeHandle`
  (`Completed | Cancelled | TimedOut`) and ends; dropping the inner `stream` cancels
  nested in-flight awaits (model HTTP stream, tool futures) within one poll (D4). When
  the inner stream ends on its own, the wrapper commits `Completed`.
- **Ordering invariant:** the wrapper must commit the `OutcomeHandle` value
  **in the same poll that it yields the terminating `None`** (before the consumer sees
  end-of-stream), so a caller reading the handle *after* draining never observes a stale
  or default outcome. The plan pins this (e.g. set the shared cell, then return `None`).

### 5.2 `run` — aggregate to `RunResult`

`run` wraps the stream with `controlled(...)`, accumulates via the existing
`RunResultStreaming::collect` logic (one definition of "stream → `RunResult`"),
captures the `Result` **without `?`-short-circuiting**, runs `finalize`, *then*
maps the `OutcomeHandle` + collect result:

- handle `Cancelled` → `Err(RunError::Cancelled)`; `TimedOut` → `Err(RunError::Timeout)`.
  (The wrapper ends the stream cleanly on these, so `collect` returns the partial
  accumulation; the typed error comes from the handle, not from a stringified
  `RunFailed` — which would otherwise flatten to `RunError::Other`.)
- handle `Completed` → return `collect`'s own `Result`: `Ok(RunResult)` on success, or
  `Err(RunError::Other)` on a genuine **agent failure** (a `RunFailed` event in the
  stream; structured form deferred to SMA-346).

**`finalize(&session).await` (no-op now) runs on all four exits — normal, agent
failure, cancel, timeout — in both methods.** The agent-failure
path is the trap: because `run` reuses `collect` (which returns `Err` on `RunFailed`),
`finalize` must be sequenced *before* the error is propagated, never after a `?`. Do not
write `let r = collect().await?; finalize().await;`.

### 5.3 `run_streamed` — pass-through with control + finalize seam

Return a `RunResultStreaming` wrapping an `async_stream!` that pumps the
`controlled(...)` stream:

- Each agent event is yielded through unchanged (preserving AC#3 ordering).
- On `Cancelled` / `TimedOut`, yield a terminal `AgentEvent::RunFailed { error }`
  (`"run cancelled"` / `"run timed out"`) for streaming consumers. *(String-based per
  SMA-313; SMA-346 will carry the structured error.)*
- After the inner stream ends on **any** path — normal, agent failure (`RunFailed`
  passes through), cancel, timeout — call `finalize(&session).await` (**no-op in
  SMA-321**), then end the outer stream. This is what makes the outer stream "not done
  until finalization finishes" and is the documented seam for session persistence +
  compaction.

**Error-fidelity note across entry points.** `run` is the path that
preserves the typed cancel/timeout reason (`RunError::Cancelled` / `Timeout`).
`run_streamed(...).collect()` instead sees the injected `RunFailed { error: String }`
and flattens it to `RunError::Other` — consistent with SMA-313/346 keeping the event
stream string-based. This gap is fully closed by SMA-346 (structured `AgentError` at the
boundary).

### 5.4 Cancellation caveat

"Aborts within one polling boundary" holds **for cooperative futures**. A tool doing
blocking, non-`await` CPU work will not yield until its next await point; drop-based
cancellation cannot preempt it. `run_tools_concurrent` uses `join_all` (not
`tokio::spawn`), so the in-flight tool futures are dropped with the stream — the
guarantee is about cooperative cancellation, not hard preemption.

### 5.5 Typed `RunResult<T>`

`TokioRunner::run` returns `RunResult` (= `RunResult<String>`), consistent with SMA-320
/ SMA-346 deferring the typed surface. Structured-output callers still go through
`RunResult::<String>::parse_final::<T>()` / `collect_typed`. The runner does **not**
deliver typed output end-to-end here.

## 6. Acceptance criteria → tests

All tests use scripted mocks. `runtime-tokio` gets its own `tests/common/mod.rs`
(`MockModel`, `MockTool`, `MockToolBarrier`, a counting `NoopSession`,
`noop_run_context`) — `core`'s test helpers are not exported across crates.

| AC / concern | Test | Assertion |
|----|------|-----------|
| #1 cancellation | cancel while a tool/model await is in flight | aborts within one poll; `run` → `RunError::Cancelled` |
| #1 same-poll race | cancel fires in the same poll as `RunCompleted` | reported `Completed`, not `Cancelled` (M1) |
| timeout | `timeout` shorter than a slow mock model | `run` → `RunError::Timeout` |
| #2 concurrency | 5 barrier-synced tools through `TokioRunner` | all run concurrently (barrier releases; outer `timeout` guard catches a serial deadlock) |
| #2 bound | `parallel_tool_call_limit = 2` with 4 barrier tools in two waves | concurrency capped at 2 |
| #3 ordering | happy-path run through `TokioRunner` | events ordered lifecycle → semantic → terminal |
| finalize | counting `NoopSession`; run normal / **agent-failure (`RunFailed`-scripted)** / cancel / timeout | `finalize` invoked exactly once on every one of the four paths, in both `run` and `run_streamed` |
| core | `buffered` order-preservation unit test | outcome order == call order under a limit |
| regression | existing `core` loop tests (`loop_happy_path`, `loop_parallel_tools`, `structured_output`, …) | stay green after the in-place edits |

TDD: write each test against the public API first, watch it fail, then implement.

## 7. Crate ascend + wiring

`paigasus-helikon-runtime-tokio` ascends `0.0.0 → 0.1.0` via the CLAUDE.md 4-step
recipe, landed as a `chore(release): SMA-321 lift stage-1 gates for
paigasus-helikon-runtime-tokio` commit on this branch (release-infra commits use
`chore`/`docs`, never `feat`/`fix`):

1. Bump `version = "0.0.0"` → `"0.1.0"` in the crate `Cargo.toml`.
2. Remove `publish = false` from that `Cargo.toml`.
3. Remove the crate's `release = false` block from `release-plz.toml`.
4. Bump the `[workspace.dependencies]` entry for the crate to `version = "0.1.0"`
   (currently pinned `0.0.0`; without this, cargo rejects the path crate at `0.1.0`
   against the `^0.0.0` requirement — verified).

Add dependencies to `runtime-tokio/Cargo.toml`:

- Runtime: `paigasus-helikon-core`, `tokio` (workspace; `full`), `async-trait`,
  `futures-core`, `futures-util`, `async-stream`, `tokio-util`.
- Dev: `tokio` (test macros, multi-thread), `anyhow`, `serde_json` for the local mocks.

Copy the standard `[lints] workspace = true` opt-in block (already present in the stub).
The facade already re-exports `runtime_tokio` behind the `runtime-tokio` feature — the
only flow-through is the dependency version bump.

## 8. Follow-up tickets to file

1. **Session persistence + compaction in `finalize()`** — write the run's semantic items
   to `ctx.session()`, seed the conversation from `ctx.session().snapshot()` at start,
   and drive `LoopState::Compacting`. Pairs with SMA-346.
2. **Retry policy + mechanism** — add `RunConfig::retry_policy` *with* a working
   mechanism in the same release. Likely a composable `RetryingModel<M>` decorator in
   `runtime-tokio` so backoff timers stay out of core.
3. *(Optional)* **`core::driver` extraction** — only if/when a second real consumer
   appears, and reframed around `transition` (the actual durability seam), not the async
   driver.

Doc reconciliation (ADR-13, Agent Loop page, SMA-321 scope) is **already done** (§1.1) —
not a follow-up.

## 9. Out of scope

- `retry_policy` (deferred — follow-up #2).
- Driver extraction into `core::driver` (deferred — follow-up #3).
- Structured `AgentError` through the Runner boundary — **SMA-346** (this ticket lands
  the typed-return surface SMA-346 extends; the event stream stays string-based per
  SMA-313).
- Session persistence/load + compaction (deferred — follow-up #1).
- Handoffs, approvals, guardrails, hooks (loop variants still `NotImplemented`).
