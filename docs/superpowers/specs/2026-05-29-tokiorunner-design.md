# SMA-321 — TokioRunner: cancellation, timeouts, parallel tool calls

**Status:** Design (awaiting review)
**Issue:** [SMA-321](https://linear.app/smaschek/issue/SMA-321)
**Branch:** `feature/sma-321-tokiorunner-cancellation-timeouts-parallel-tool-calls`
**Date:** 2026-05-29
**Blocks:** [SMA-346](https://linear.app/smaschek/issue/SMA-346) (structured `AgentError` at the Runner boundary)

## 1. Summary

Implement `TokioRunner`, the default ephemeral execution backend, as a concrete
`Runner<Ctx>` in `paigasus-helikon-runtime-tokio`. It adds run-level execution
control — cancellation, timeout, bounded tool-call concurrency, and a retry-policy
surface — on top of the agent loop that already exists in `LlmAgent::run`.

To make the loop reusable by the runner (and by future durable runners), the async
driver is **extracted** out of `LlmAgent::run` into a shared, tokio-free
`core::driver` function. `TokioRunner` stays thin: it consumes `agent.run()`'s
event stream and wraps it with tokio-specific control at the boundary.

## 2. Context: what already exists (SMA-313/314/319/320)

- **`transition` (`core::loop_state`)** — a pure, resumable state-machine step.
  No async, no IO. Unchanged by this work.
- **The async driver — currently inside `LlmAgent::run` (`core::agent`)** — drives
  `transition` in a loop: calls the model, pumps the model event stream, runs tool
  calls concurrently (`run_tools_concurrent` via unbounded `join_all`), and yields a
  `BoxStream<'static, AgentEvent>`.
- **`Agent` trait** — opaque: exposes only `name`, `description`, and
  `run(ctx, input) -> BoxStream<AgentEvent>`. It does **not** expose the model,
  tools, or settings.
- **`Runner` trait** — object-safe by design (takes `&dyn Agent<Ctx>`), with
  `run -> Result<RunResult, RunError>` and `run_streamed -> Result<RunResultStreaming, RunError>`.
- **`RunConfig`** — currently only `max_turns`. Read from `LlmAgent.config`, **not**
  from the `RunConfig` passed to `Runner::run`.
- **`RunContext`** — already owns the canonical `CancellationToken` (`ctx.cancel()`),
  plus session handle, hook registry, tracer, and user context.

Consequences that shape this design:

1. **The Runner cannot re-drive the loop for an opaque `&dyn Agent`.** It can only
   consume the agent's event stream. The driver must live in the agent (or a shared
   core fn the agent calls).
2. **AC#2 and AC#3 are already satisfied** by the existing driver + `transition`
   (parallel tool calls run concurrently; events are emitted in
   lifecycle → semantic → terminal order). This work *bounds* concurrency and
   *preserves* ordering through the runner — it does not invent them.
3. **Cancellation already flows** `ctx.cancel()` → `model.invoke(req, cancel)`, and
   tools observe a child token via `ctx.to_tool_context()`.

## 3. Decisions (locked during brainstorming)

| # | Decision | Choice |
|---|----------|--------|
| D1 | Driver ownership | **Extract a shared driver** into `core::driver`; `LlmAgent::run` and future durable runners reuse it. `TokioRunner` still reaches it via `agent.run()`. |
| D2 | How per-invocation `RunConfig` reaches the driver | **Thread via `RunContext`.** The runner installs the effective config into the context; the driver reads it. No `Agent` trait change. |
| D3 | Session integration scope | **Defer entirely.** SMA-321 lands execution control only. A no-op `finalize()` seam marks where persistence + compaction will land in a follow-up. |
| D4 | Cancellation mechanism | **Drop-based at the runner boundary.** `select!` on the cancel token / deadline; dropping the stream cancels nested awaits within one poll. |
| D5 | `retry_policy` | **Land the type + field off-by-default; defer the mechanism** (a composable `RetryingModel<M>` decorator) to a follow-up. Keeps core tokio-free. *(Recommendation — confirm at review.)* |

**Hard constraint:** `paigasus-helikon-core` stays free of the tokio *runtime*
(no `tokio::time::sleep`, `tokio::spawn`, or `tokio::sync::Semaphore`). It already
depends on `tokio-util` only for `CancellationToken`. The separate
`paigasus-helikon-runtime-tokio` crate is the entire reason this boundary exists;
preserving it keeps the durability seam meaningful. D5 and the choice of
`futures_util::buffered` over a tokio `Semaphore` both follow from this.

## 4. Core changes (`paigasus-helikon-core`)

### 4.1 `RunConfig` (in `runner.rs`)

Grows three fields; stays `#[non_exhaustive]`; `Default` unchanged in behavior.

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunConfig {
    pub max_turns: u32,                                   // existing; default 16
    pub timeout: Option<Duration>,                       // new; None = no deadline
    pub parallel_tool_call_limit: Option<NonZeroUsize>,  // new; None = unbounded
    pub retry_policy: RetryPolicy,                        // new; default = disabled
}
```

```rust
/// Retry policy for transient model errors. Disabled by default (ADR-10:
/// no silent auto-retry; retries are opt-in application-layer config).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RetryPolicy {
    /// Maximum retry attempts after the first failure. `0` (default) = no retry.
    pub max_retries: u32,
    /// Base backoff between attempts. Honored once the mechanism lands (D5).
    pub initial_backoff: Option<Duration>,
}
```

- **No `cancellation` field.** `RunContext::cancel()` is the single canonical token.
  Adding a second on `RunConfig` would create two sources of truth and force
  reconciliation. This is an intentional deviation from the ticket's bullet, justified
  by D2 (config threads *through* the context, where the token already lives).
- `RunConfig::new()` remains the default constructor. Builders for the new fields
  (`with_timeout`, `with_parallel_tool_call_limit`, `with_retry_policy`) are added for
  ergonomics.

### 4.2 `RunContext` (in `context.rs`)

Carries the per-invocation `RunConfig` so the driver can read it.

```rust
// new field
run_config: Option<RunConfig>,

// new methods
pub fn with_run_config(mut self, config: RunConfig) -> Self { self.run_config = Some(config); self }
pub fn run_config(&self) -> Option<&RunConfig> { self.run_config.as_ref() }
```

- `RunContext::new(...)` signature is unchanged; `run_config` defaults to `None`.
  This keeps the existing test helper (`noop_run_context`) and all current call sites
  compiling untouched.
- **Precedence (resolved in `LlmAgent::run`):** effective config =
  `ctx.run_config().cloned().unwrap_or_else(|| self.config.clone())`. A runner sets the
  context config; a direct `agent.run()` falls back to the agent's own `config`.

### 4.3 Extract the driver into `core::driver`

Move the `async_stream! { … }` body plus the private helpers (`build_items`,
`run_tools_concurrent`, `tool_output_to_content_parts`, `ToolCallAccum`) out of
`LlmAgent::run` into a new `driver.rs` module:

```rust
pub fn drive<Ctx>(
    model: Arc<dyn Model>,
    tools: Vec<Arc<dyn Tool<Ctx>>>,
    tool_defs: Vec<ToolDef>,
    model_settings: ModelSettings,
    output_type: Option<OutputType>,
    agent_name: String,
    instructions_text: String,
    config: RunConfig,            // already resolved by the caller
    ctx: RunContext<Ctx>,
    input: AgentInput,
) -> BoxStream<'static, AgentEvent>
where
    Ctx: Send + Sync + 'static;
```

`LlmAgent::run` becomes a thin shim: snapshot `self`, resolve the effective config
(§4.2 precedence), and call `driver::drive(...)`. Behavior is identical to today when
no context config is set and `parallel_tool_call_limit` is `None`.

> Implementation note: `model` is taken as `Arc<dyn Model>` (object-safe via
> `async_trait`) so `drive` is not generic over the concrete model type. `LlmAgent`
> coerces its `Arc<M>` at the call site.

### 4.4 Bounded tool-call concurrency

Replace `join_all` in the tool fan-out with order-preserving bounded concurrency,
using **`futures_util`** (no tokio):

- `parallel_tool_call_limit == None` → keep `join_all` (unbounded, today's behavior).
- `Some(n)` → `futures_util::stream::iter(futs).buffered(n.get()).collect().await`.

`buffered` (not `buffer_unordered`) preserves call order in the outcome `Vec`, so
`ToolResult` items keep a deterministic order in the conversation regardless of which
tool finishes first.

### 4.5 `RunError::Timeout`

Add an additive variant (enum is `#[non_exhaustive]`):

```rust
/// The run exceeded its configured `RunConfig::timeout`.
#[error("run timed out")]
Timeout,
```

### 4.6 Exports

Re-export `RetryPolicy` from `core::lib` (with a `///` doc comment — the docs job runs
`-D warnings`). `RunError::Timeout` needs no new export.

## 5. `TokioRunner` (`paigasus-helikon-runtime-tokio`)

Stateless: `pub struct TokioRunner;` + `Default`. Both trait methods follow the same
prelude:

```rust
let ctx = ctx.with_run_config(config.clone());
let cancel = ctx.cancel().clone();          // clone handles before ctx is moved
let session = ctx.session().clone();        // for the finalize seam (no-op now)
let stream = agent.run(ctx, input).await.map_err(RunError::Agent)?;
```

`agent.run` takes `RunContext` by value, so any handle `finalize` (§5.2) needs is
cloned from the context first — here the `Arc<dyn Session>`.

### 5.1 `run` — aggregate to `RunResult`

Drain `stream` in a `tokio::select!` loop, accumulating events / `final_output` /
`usage` exactly as `RunResultStreaming::collect` does, but cancel- and
deadline-aware:

```rust
let sleep = config.timeout.map(tokio::time::sleep);  // pinned; None => never fires
loop {
    select! {
        maybe_ev = stream.next() => match maybe_ev {
            Some(ev) => { /* accumulate; on RunFailed -> Err(RunError::Other) */ }
            None     => break,                       // stream exhausted
        },
        _ = cancel.cancelled()       => return Err(RunError::Cancelled),
        _ = sleep_fires(&mut sleep)  => return Err(RunError::Timeout),
    }
}
```

On `Cancelled` / `Timeout` the function returns, `stream` is dropped, and all nested
in-flight awaits (model HTTP stream, tool futures) are cancelled within one poll. The
cancel token also continues to flow to `model.invoke` (graceful network teardown) and
to tool child tokens (cooperative cleanup) — drop is the backstop, not the only signal.

### 5.2 `run_streamed` — pass-through with control + finalize seam

Return a `RunResultStreaming` whose inner stream is a new `async_stream!` that pumps
the agent stream under the same `select!`:

- Each agent event is yielded through unchanged (preserving AC#3 ordering).
- On `cancel` / `timeout`: yield a terminal `AgentEvent::RunFailed { error }`
  (`"run cancelled"` / `"run timed out"`), then end. *(String-based per SMA-313;
  SMA-346 will carry structured error.)*
- After the inner stream completes (normally or via the control branches), call
  `finalize(&session).await` — **a no-op in SMA-321** — then end the outer stream.
  This is the documented seam where session persistence + compaction land later, and
  is what makes the outer stream "not done until finalization finishes" (the ticket's
  `run_streamed` contract), even though finalization currently does nothing.

## 6. Acceptance criteria → tests

All tests use scripted mocks. `runtime-tokio` gets its own `tests/common/mod.rs`
(`MockModel`, `MockTool`, `MockToolBarrier`, `NoopSession`, `noop_run_context`) — the
core test helpers are not exported across crates.

| AC | Test | Assertion |
|----|------|-----------|
| #1 cancellation | cancel while a tool/model await is in flight | run aborts within one poll; `run` → `RunError::Cancelled` |
| (timeout) | `timeout` shorter than a slow mock model | `run` → `RunError::Timeout` |
| #2 concurrency | 5 barrier-synced tools through `TokioRunner` | all run concurrently (barrier releases; `timeout` guard catches serial deadlock) |
| #2 bound | `parallel_tool_call_limit = 2` with 4 barrier tools split into two waves | concurrency is capped at 2 |
| #3 ordering | happy-path run through `TokioRunner` | events ordered lifecycle → semantic items → terminal |
| regression | existing `core` loop tests (`loop_happy_path`, `loop_parallel_tools`, `structured_output`, …) | stay green after driver extraction |
| core | `buffered` order-preservation unit test | outcome order == call order under a limit |

TDD: write each test against the public API first, watch it fail, then implement.

## 7. Crate ascend + wiring

`paigasus-helikon-runtime-tokio` ascends `0.0.0 → 0.1.0` via the CLAUDE.md 4-step
recipe, landed as a `chore(release): SMA-321 lift stage-1 gates for
paigasus-helikon-runtime-tokio` commit on this branch:

1. Bump `version = "0.0.0"` → `"0.1.0"` in the crate `Cargo.toml`.
2. Remove `publish = false` from that `Cargo.toml`.
3. Remove the crate's `release = false` block from `release-plz.toml`.
4. Bump the `[workspace.dependencies]` entry for the crate to `version = "0.1.0"`.

Add dependencies to `runtime-tokio/Cargo.toml`:

- Runtime: `paigasus-helikon-core`, `tokio` (workspace; `full`), `async-trait`,
  `futures-core`, `futures-util`, `async-stream`, `tokio-util`.
- Dev: `tokio` (test macros, multi-thread), plus whatever the local `tests/common`
  mocks need (`anyhow`, `serde_json`).

Copy the standard `[lints] workspace = true` opt-in block (already present in the stub).
The facade already re-exports `runtime_tokio` behind the `runtime-tokio` feature — no
facade change needed beyond the dependency version bump flowing through.

## 8. Follow-up tickets to file

1. **Session persistence + compaction in `finalize()`** — write the run's semantic
   items to `ctx.session()` and seed the conversation from `ctx.session().snapshot()`
   at start; drive `LoopState::Compacting`. Pairs with SMA-346.
2. **Retry mechanism** (D5) — a composable `RetryingModel<M>` decorator honoring
   `RunConfig::retry_policy`, keeping backoff timers out of core.

## 9. Out of scope

- Structured `AgentError` through the Runner boundary — that is **SMA-346** (this
  ticket lands the typed-return surface SMA-346 extends; the event stream stays
  string-based per SMA-313).
- Handoffs, approvals, guardrails, hooks (loop variants still `NotImplemented`).
- Durable runners (Temporal / AgentCore) — they reuse `transition` / `core::driver`
  later.
```
