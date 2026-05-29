# SMA-346 — Surface structured `AgentError` at the Runner boundary

**Status:** Approved (design)
**Linear:** [SMA-346](https://linear.app/smaschek/issue/SMA-346/surface-structured-agenterror-at-the-runner-boundary-runresult)
**Branch:** `feature/sma-346-surface-structured-agenterror-at-the-runner-boundary`
**Date:** 2026-05-29

## Problem

`AgentEvent::RunFailed { error: String }` (SMA-313) intentionally stringifies failures so the
event stream stays `Clone`-able. The structured failure is still useful: callers want to
programmatically distinguish `MaxTurnsExceeded`, `NotImplemented`, model errors, and the rest
**without parsing strings**. Today every failure pathway flattens its `AgentError` to a `String`
for the event and the structured value is dropped on the floor.

The right surfacing point is the Runner boundary:

- `Runner::run` already returns `Result<RunResult, RunError>` — surface structured `AgentError`
  via `RunError::Agent`.
- `RunResultStreaming::collect()` currently converts `RunFailed { error }` to
  `RunError::Other(anyhow::anyhow!(error))`, losing structure — it should reconstruct from a
  richer signal.

**Constraint:** do *not* change `AgentEvent::RunFailed { error: String }`. The stream stays
string-based for `Clone` and snapshot stability (16 `AgentEvent` serde-roundtrip snapshots).

## Key insight

The structured `AgentError` **already exists at the moment of failure** — we only need to carry
it out-of-band rather than reconstruct it:

- The loop state machine (`loop_state.rs`) already stores the structured error in
  `LoopState::Failed(AgentError)` for `MaxTurnsExceeded(u32)`, `InvalidStructuredOutput { .. }`,
  `NotImplemented { feature }`, and internal `Other` cases.
- The three direct stream-block failures in `agent.rs` hold a `ModelError` (model invoke / model
  stream) or a `String` (`build_items`) that map cleanly to `AgentError::Model` /
  `AgentError::Other`.

So the work is a **side-channel** that carries the existing `AgentError` from inside the
`async_stream` block out to the boundary — keeping the string event identical.

There is a direct precedent in the same runtime crate: `TokioRunner::controlled()` already uses
an `Arc<Mutex<Outcome>>` to commit a terminal reason *before* the stream ends and read it *after*
draining. We mirror that pattern.

## Chosen approach

A slot lives on `RunContext` (decided over changing the `Agent::run` signature). Rationale:
lowest blast radius, reuses the established `Arc<Mutex<…>>` idiom, and keeps the `Agent` trait —
the SDK's most important public contract — stable. `RunContext` already initializes
`run_config: None` internally with no caller-supplied parameter, so a slot field added the same
way needs **no changes to any of the 7 construction sites**.

`RunContext` is not `Clone`, but that is fine: the slot is an `Arc`-backed handle that is
independently cloneable (exactly like `cancel` / `session`). The runner clones the handle
*before* moving `ctx` into `agent.run(...)`; the stream (which owns `ctx`) clones its own handle
to the same slot.

## Design

### 1. New type: `FailureSlot` (core)

Lives in `agent.rs` next to `AgentError`; re-exported from `lib.rs`.

```rust
/// Out-of-band carrier for a run's terminal structured AgentError.
/// The AgentEvent stream stays string-based (RunFailed { error: String })
/// for Clone + snapshot stability; the structured value rides this side-channel.
#[derive(Clone, Default)]
pub struct FailureSlot(Arc<Mutex<Option<AgentError>>>);

impl FailureSlot {
    pub fn new() -> Self { Self::default() }
    /// Record the structured error (called once, immediately before the terminal RunFailed event).
    pub fn set(&self, err: AgentError) { *self.0.lock().unwrap() = Some(err); }
    /// Take the recorded error at the boundary (read once after draining).
    pub fn take(&self) -> Option<AgentError> { self.0.lock().unwrap().take() }
}
```

`Clone` is the Arc-handle clone (same underlying slot), which is what lets the runner read what
the stream wrote. A manual `Debug` impl (via `Mutex`'s `try_lock`-based `Debug`) may be added for
ergonomics; not required.

### 2. `RunContext` gains the slot (core, `context.rs`)

- Add field `failure: FailureSlot`, initialized to `FailureSlot::new()` inside `RunContext::new`.
  **No signature change, no construction-site churn.**
- Add accessor `pub fn failure_handle(&self) -> FailureSlot { self.failure.clone() }`.
- `to_tool_context()` deliberately does **not** propagate the slot (same treatment as
  `run_config` — tools do not record terminal run failures).

### 3. Recording sites in `LlmAgent::run` (core, `agent.rs`)

Grab one handle at the top of the stream body: `let failure = ctx.failure_handle();`. Then `set`
the already-existing structured error immediately before each existing `yield RunFailed` — **the
string event is byte-for-byte unchanged**:

- **`model.invoke` err (~617)** and **model-stream err (~680)**: `e` is a `ModelError` →
  compute the message string first, then `failure.set(AgentError::Model(e))`.
- **`build_items` err (~689)**: `e` is a `String` →
  `failure.set(AgentError::Other(anyhow::anyhow!(e.clone())))`.
- **State-machine failures** (`MaxTurnsExceeded`, `InvalidStructuredOutput`, `NotImplemented`,
  internal `Other`): the structured value already sits in `LoopState::Failed(AgentError)`. At the
  `NextAction::Terminate` arm, move it out before returning:

  ```rust
  Terminate => {
      if let LoopState::Failed(err) = loop_state {
          failure.set(err); // moves the structured error out; we return immediately after
      }
      return;
  }
  ```

  Success terminals are not `LoopState::Failed`, so the guard skips them. Event ordering is
  unchanged (events are still yielded before this point).

This captures **all six** failure pathways with full fidelity.

> Note: `AgentError::Tool` has **no** terminal construction site today — tool errors become
> tool-result text content and the loop continues — so it is correctly out of scope.

### 4. `RunResultStreaming` + boundary mapping (core, `runner.rs`)

- Add private field `failure: Option<FailureSlot>`. `new()` sets `None` (backward-compatible).
  Add `with_failure(events, slot)` constructor.
- `collect()`: on `RunFailed`, **prefer the slot** → `Err(RunError::Agent(err))`; fall back to
  today's `RunError::Other(anyhow::anyhow!(string))` when the slot is absent or empty.
- `collect_typed()`: prefer the slot → return the real `AgentError` directly; keep the existing
  `StructuredOutputFailed`-event reconstruction as the no-slot fallback.

**Mapping rule:** structured *agent* failures → `RunError::Agent(AgentError::…)` (preserves the
full taxonomy, including `MaxTurnsExceeded`). Runner-level cancel/timeout stay
`RunError::Cancelled` / `RunError::Timeout`, sourced from `controlled()`'s `Outcome`, never the
slot. Clean layering: agent failures via the slot, run-control failures via the `Outcome`.

### 5. `TokioRunner` wiring (runtime-tokio, `lib.rs`)

In `run()`, clone the handle before moving `ctx` (beside the existing `cancel` / `session`
clones) and hand it to the streaming wrapper so `collect()` performs the mapping:

```rust
let failure = ctx.failure_handle();
let stream = agent.run(ctx, input).await?;
let (controlled_stream, outcome) = controlled(stream, cancel, timeout);
let collected = RunResultStreaming::with_failure(controlled_stream, failure).collect().await;
finalize(&session).await;
match outcome.get() {
    Outcome::Cancelled => Err(RunError::Cancelled),
    Outcome::TimedOut  => Err(RunError::Timeout),
    Outcome::Completed => collected, // now structured
}
```

`run_streamed()` also constructs its result via `with_failure`, so a later `.collect()` on the
streamed handle is structured too. Cancel/timeout still synthesize string `RunFailed` events as
today (the slot is empty in those cases — behavior unchanged).

### 6. Explicitly unchanged

`AgentEvent` shape, all 16 `AgentEvent` serde-roundtrip snapshots, `Clone` on `AgentEvent`, the
`Agent` trait signature, the `finalize`-always-runs guarantee, and cancel/timeout typing.

## Testing

- **Core unit/integration:**
  - Drive `LlmAgent::run` with a failing mock `Model` → assert the `ctx` slot holds
    `AgentError::Model`; a small `max_turns` → `AgentError::MaxTurnsExceeded(n)`.
  - `collect()` / `collect_typed()` via `with_failure` with a preset slot → structured
    `RunError::Agent` / `AgentError`; via `new()` (no slot) → unchanged fallback (keeps the
    existing `collect_typed` tests green).
- **runtime-tokio integration:** failing model → `RunError::Agent(AgentError::Model(..))`;
  max-turns → `RunError::Agent(AgentError::MaxTurnsExceeded(..))`; cancel/timeout → still
  `RunError::Cancelled` / `RunError::Timeout`.
- **Snapshots:** confirm zero regeneration (`AgentEvent` shape untouched).
- **Docs:** `///` on `FailureSlot`, `with_failure`, `failure_handle` (the `missing_docs` lint +
  80% doc-coverage gate).

## Release sequencing ⚠️

`paigasus-helikon-runtime-tokio` (already released) consumes **new `paigasus-helikon-core` API
added in this same PR** (`FailureSlot`, `failure_handle`, `RunResultStreaming::with_failure`).
This is exactly the documented "ascending crate uses same-PR core API" caveat (SMA-321): the
release-time `cargo publish --verify` builds the runtime-tokio tarball against the **registry**
core, so the new core API must already be published.

**Plan:** in the same PR, also bump `paigasus-helikon-core` (patch, e.g. `0.2.1 → 0.2.2`) plus
its `[workspace.dependencies]` pin and CHANGELOG, so release-plz publishes core first and
runtime-tokio verifies against the fresh core (dependency-ordered publish).

**Alternative considered:** split into a core-first PR then a runtime-tokio PR — sidesteps the
caveat entirely, but the ticket is a single SMA. The single-PR-with-core-bump path is preferred
since it matches the documented recipe in `CLAUDE.md`.

## Files touched

- `crates/paigasus-helikon-core/src/agent.rs` — `FailureSlot` type; recording at the 3 direct
  sites + the `Terminate` arm.
- `crates/paigasus-helikon-core/src/context.rs` — `failure` field + `failure_handle()` accessor.
- `crates/paigasus-helikon-core/src/runner.rs` — `RunResultStreaming::failure` +
  `with_failure()`; slot-preferring mapping in `collect()` and `collect_typed()`.
- `crates/paigasus-helikon-core/src/lib.rs` — export `FailureSlot`.
- `crates/paigasus-helikon-runtime-tokio/src/lib.rs` — clone handle pre-move; `with_failure`
  wiring in `run()` and `run_streamed()`.
- Tests in both crates; CHANGELOGs; core version bump + workspace pin (see release sequencing).
