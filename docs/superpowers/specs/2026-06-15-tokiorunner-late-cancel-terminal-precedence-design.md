# SMA-421 — TokioRunner: a late cancel/timeout must not override an already-terminal run

**Status:** design approved 2026-06-15; review applied (M1/L1/L2/N1)
**Linear:** [SMA-421](https://linear.app/smaschek/issue/SMA-421) (related: SMA-321, SMA-392, SMA-346; follow-up [SMA-422](https://linear.app/smaschek/issue/SMA-422))
**Scope:** fix logic in `crates/paigasus-helikon-runtime-tokio` only; doc-only additions to `paigasus-helikon-core` (`Runner::run`, `HookEvent::OnRunComplete`). No public API change.

## Problem

`TokioRunner` adds run-level control (cancel/timeout/aggregation) at the agent-stream
boundary in `crates/paigasus-helikon-runtime-tokio/src/lib.rs`. The agent stream (in
`paigasus-helikon-core/src/agent.rs`) yields the terminal event **before** the generator
returns: on `NextAction::Terminate` it does

```
yield RunCompleted / RunFailed     // controlled passes it through; recorder + collect observe it
await OnRunComplete hook            // SUSPENSION WINDOW
return                             // stream ends
```

If a cancel or timeout fires while the generator is suspended in that `OnRunComplete`
await, `controlled`'s `biased` `select!` polls the (now `Pending`) inner stream first,
then takes the cancel/deadline branch and commits `Outcome::Cancelled` / `Outcome::TimedOut`.
Consequences:

- **`run`** — `collected` is already `Ok` (it captured `RunCompleted`), but the final
  `match outcome.get()` returns `Err(Cancelled)` / `Err(Timeout)`. A run that genuinely
  **completed is misreported as cancelled/timed-out.**
- **`run_streamed`** — the real terminal was already yielded by the `while` loop; the
  post-loop `match outcome.get()` then synthesizes a **second**, synthetic `RunFailed`.
  **Two terminal events on one run** (and `finalize` runs twice — harmless, since
  `SessionRecorder::drain()` uses `mem::take`, so the second append is empty).

The window is narrow: it requires a genuinely *suspending* `OnRunComplete` hook (no-op /
empty hooks resolve in the same poll, so `biased` + same-poll completion closes the
window) **and** a cancel/timeout firing precisely during that suspension. No existing test
exercises it.

This is pre-existing SMA-321 control flow, byte-identical between `main` and the SMA-392
branch, and unrelated to session persistence. It was deferred from PR #84 as out of scope.

## Decision

**Terminal wins.** A genuine terminal event (`RunCompleted` / `RunFailed`) is the run's
true outcome; a cancel/timeout overrides **only** when it actually aborted in-flight work —
i.e. when no terminal event was ever observed. This deliberately reverses the prior
precedence ("a cancel/timeout outcome wins even if `collected` is Ok") for the
post-terminal window, and it is consistent with the SMA-346 structured-error boundary: a
terminal `RunFailed` now surfaces its real `RunError::Agent(..)` rather than being masked
by a late `Err(Cancelled)`.

The pre-terminal behavior is unchanged: a cancel/timeout that aborts a run before any
terminal event still reports `Err(Cancelled)` / `Err(Timeout)`.

This precedence is **runner-agnostic run semantics**, but the fix is implemented in
`runtime-tokio` only (its `Outcome` / `controlled()` / `tokio::select!` mechanism is
tokio-specific, and the durable Temporal/AgentCore runners are 1-line stubs today — there
is no second implementation to shape a shared abstraction against). The policy is captured
in the `Runner` trait docs now (see Change 4); hoisting it into a shared core resolver is
tracked as follow-up [SMA-422](https://linear.app/smaschek/issue/SMA-422), to be done when
the durable runners are actually built (review item **M1**).

## Changes

Changes 1–3 are the fix, all in `crates/paigasus-helikon-runtime-tokio/src/lib.rs`.
Change 4 is doc-comments only, in `paigasus-helikon-core`.

### 1. File-local helper

```rust
fn is_terminal(ev: &AgentEvent) -> bool {
    matches!(ev, AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. })
}
```

### 2. `run` — gate the override on `saw_terminal`

After `finalize`, before the final `match`:

```rust
// A genuine terminal event wins over a cancel/timeout that fired only after it
// (e.g. during a suspending OnRunComplete hook). The override applies solely
// when the run aborted in-flight, before any terminal event. (SMA-421)
let saw_terminal = collected
    .as_ref()
    .map(|r| r.events.iter().any(is_terminal))
    .unwrap_or(true); // Err(_) from collect() ⇔ a RunFailed was observed

match outcome.get() {
    Outcome::Cancelled if !saw_terminal => Err(RunError::Cancelled),
    Outcome::TimedOut if !saw_terminal => Err(RunError::Timeout),
    _ => collected,
}
```

Correctness notes:
- `collect()` returns `Err` **only** when a `RunFailed` was observed, so `Err ⇒
  saw_terminal` (the `unwrap_or(true)`).
- An `Ok` whose `events` contain no terminal is exactly the genuine in-flight cancel/timeout
  case (`PendingModel` never yields a terminal) — `saw_terminal == false`, so the override
  still fires and reports `Cancelled` / `Timeout`.

### 3. `run_streamed` — suppress synthetic terminal, finalize once

Track `saw_terminal` and a `finalized` guard in the generator:

```rust
let out = async_stream::stream! {
    let mut saw_terminal = false;
    let mut finalized = false;
    while let Some(ev) = recorded.next().await {
        if is_terminal(&ev) {
            // Finalize BEFORE exposing the terminal: a consumer may drop the
            // stream the moment it sees the terminal event.
            if !finalized {
                finalize(&session, &recorder).await;
                finalized = true;
            }
            saw_terminal = true;
        }
        yield ev;
    }
    // Only synthesize a terminal when the run aborted in-flight (no real
    // terminal was ever yielded). A late cancel/timeout that fired after a real
    // terminal must NOT emit a second one.
    if !saw_terminal {
        match outcome.get() {
            Outcome::Cancelled => {
                if !finalized { finalize(&session, &recorder).await; finalized = true; }
                yield AgentEvent::RunFailed { error: "run cancelled".to_owned() };
            }
            Outcome::TimedOut => {
                if !finalized { finalize(&session, &recorder).await; finalized = true; }
                yield AgentEvent::RunFailed { error: "run timed out".to_owned() };
            }
            Outcome::Completed => {}
        }
    }
};
```

### 4. Documentation (core, doc-only — review items L1 / L2 / M1)

Doc-comment additions only; no signature or behavior change.

- **`Runner::run` / `run_streamed` docs** (`core/src/runner.rs`) — state the cancel
  precedence policy: **cancellation/timeout is best-effort and loses to a genuine terminal
  event that already occurred.** A caller can no longer assume "I called `cancel()` ⇒ I get
  `Err(Cancelled)`"; if the run reached a terminal first, its real outcome
  (`Ok` / `Err(Agent(..))`) is reported. This is the trait-level home for the runner-agnostic
  policy (M1) and documents the L2 caller-facing consequence.
- **`HookEvent::OnRunComplete` docs** (`core/src/hook.rs`) — note that `OnRunComplete` is
  **best-effort and may be aborted mid-execution** if the run is cancelled during its
  window (the cancel drops the agent stream, cancelling the suspended hook). Consumers
  needing guaranteed post-run cleanup must not rely solely on `OnRunComplete` (L1).

## Tests

### New deterministic regression coverage

The window is reproduced **without sleeps or timing races** by a hook that cancels the run
from inside `OnRunComplete` and then suspends. Because the agent yields the terminal event
*before* firing `OnRunComplete`, this guarantees the exact ordering: terminal collected →
hook cancels → `controlled`'s `biased` select sees a `Pending` stream + a ready cancel →
commits `Cancelled` → drops the inner stream (cancelling the suspended hook). The whole
cascade runs in a single synchronous poll.

New helper in `tests/common/mod.rs`:

```rust
/// On OnRunComplete: cancel the run from inside the hook, then suspend.
/// Deterministically reproduces the "terminal event yielded, then cancel during
/// the post-terminal hook await" window.
pub struct CancelOnRunCompleteHook;

#[async_trait]
impl<Ctx: Send + Sync + 'static> Hook<Ctx> for CancelOnRunCompleteHook {
    async fn on_event(&self, ctx: &RunContext<Ctx>, event: &HookEvent) -> HookDecision {
        if matches!(event, HookEvent::OnRunComplete) {
            ctx.cancel().cancel();
            std::future::pending::<()>().await;
        }
        HookDecision::Allow
    }
}
```

Hook wiring (N1, verified against source): `Interceptors::fire` (`core/src/control.rs`)
dispatches `self.agent_hooks.iter().chain(registry.iter())` — both the agent's `hooks`
field **and** the ctx `HookRegistry` fire for `OnRunComplete`, and `HookRegistry::push`
(`core/src/context.rs`) exists. So attach `CancelOnRunCompleteHook` via the ctx
`HookRegistry` — **no `text_agent` change needed.** Add a small test helper that builds a
`RunContext` with a pre-populated registry (e.g. `run_context_with_cancel_and_hooks(cancel,
hooks)` / push onto the registry before `RunContext::new`).

Two new tests (in `run_control.rs` / `run_streamed.rs`):

- **`terminal_then_late_cancel_reports_completed` (`run`)** — `MockModel::quick_hi()` +
  `CancelOnRunCompleteHook`. Assert `Ok` with `final_output == "hi"` (not `Err(Cancelled)`).
- **`terminal_then_late_cancel_no_synthetic_terminal` (`run_streamed`)** — same setup.
  Collect the events; assert exactly **one** terminal event (`RunCompleted`) and **no**
  trailing synthetic `RunFailed`.

### Existing tests that must stay green (regression guard)

- `cancel_aborts_in_flight_run`, `timeout_returns_timeout` — `PendingModel`, no terminal →
  `saw_terminal == false` → override still fires.
- `prefired_cancel_still_completes_ready_run` — outcome resolves to `Completed` → unchanged.
- `finalize_runs_on_every_run_exit` — finalize counts unchanged (1 per exit).
- `finalize_runs_on_streamed_exits` — `run_streamed` finalize-once preserves the count.

## Out of scope

- `paigasus-helikon-core/src/workflow.rs` synthetic terminals and the core `resume`/
  `resume_streamed` defaults — the ticket scopes this to `runtime-tokio`.
- Hoisting the precedence rule into a shared core resolver — tracked as
  [SMA-422](https://linear.app/smaschek/issue/SMA-422) (review item M1); deferred until the
  durable runners exist. SMA-421 documents the policy in the trait docs (Change 4).
- No public API change → no version bumps, no release ritual. The `core` edits are
  doc-comments only; they ride the same PR.

## Verification

Run the full local CI gate set from `CLAUDE.md` (fmt, clippy `-D warnings`, `cargo test
--workspace --all-features`, doc, doc-coverage) before opening the PR.
