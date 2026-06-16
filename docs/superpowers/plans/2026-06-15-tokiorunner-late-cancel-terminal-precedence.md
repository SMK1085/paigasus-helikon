# TokioRunner Late-Cancel Terminal Precedence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A genuine terminal event (`RunCompleted`/`RunFailed`) wins over a cancel/timeout that fires only after it (e.g. during a suspending `OnRunComplete` hook), so `run` no longer misreports a completed run as cancelled and `run_streamed` no longer emits a second, synthetic terminal.

**Architecture:** Gate the cancel/timeout override in `TokioRunner` on whether a terminal event was actually observed. In `run`, scan `collected.events`; in `run_streamed`, track `saw_terminal` + a `finalized` guard in the generator. Reproduce the window deterministically with a hook that cancels from inside `OnRunComplete` and then suspends (no sleeps). Document the resulting cancel precedence as runner-agnostic policy on the `Runner` trait + `HookEvent::OnRunComplete`.

**Tech Stack:** Rust, Tokio, `async-stream`, `async-trait`, `futures-util`. Crates: `paigasus-helikon-runtime-tokio` (fix + tests), `paigasus-helikon-core` (doc-only).

**Spec:** `docs/superpowers/specs/2026-06-15-tokiorunner-late-cancel-terminal-precedence-design.md`

**Branch / git guardrails:** All work stays on the current branch `feature/sma-421-tokiorunner-a-late-canceltimeout-can-override-an-already`. Do **NOT** run any HEAD- or branch-moving git command (`checkout`, `switch`, `reset`, `rebase`, `branch -f`, `restore --source`). Only stage the explicit paths named in each commit step — never `git add -A`/`git add .` (`.env`/`.claude` are untracked-but-not-ignored). Commits are signed via a 1Password SSH key; if a commit fails with `1Password: failed to fill whole buffer`, stop and ask the user to unlock 1Password, then retry the commit — do not bypass signing.

---

## File Structure

| File | Responsibility | Action |
|------|----------------|--------|
| `crates/paigasus-helikon-runtime-tokio/src/lib.rs` | The fix: `is_terminal` helper, `run` `saw_terminal` gate, `run_streamed` `saw_terminal`+`finalized` guard | Modify |
| `crates/paigasus-helikon-runtime-tokio/tests/common/mod.rs` | Deterministic `CancelOnRunCompleteHook` + `run_context_with_cancel_and_hooks` helper | Modify |
| `crates/paigasus-helikon-runtime-tokio/tests/run_control.rs` | New `run` regression test | Modify |
| `crates/paigasus-helikon-runtime-tokio/tests/run_streamed.rs` | New `run_streamed` regression test | Modify |
| `crates/paigasus-helikon-core/src/runner.rs` | Doc-only: cancel-precedence policy on `Runner::run`/`run_streamed` | Modify |
| `crates/paigasus-helikon-core/src/hook.rs` | Doc-only: `OnRunComplete` best-effort contract | Modify |

---

## Task 1: `run` — terminal wins over a late cancel

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/common/mod.rs`
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/run_control.rs`
- Modify: `crates/paigasus-helikon-runtime-tokio/src/lib.rs`

- [ ] **Step 1: Add the test hook + context helper to `common/mod.rs`**

In the import block at the top of `crates/paigasus-helikon-runtime-tokio/tests/common/mod.rs`, add `Hook`, `HookDecision`, `HookEvent` to the existing `paigasus_helikon_core` use list. The current list is:

```rust
use paigasus_helikon_core::{
    CancellationToken, ConversationSnapshot, HookRegistry, Instructions, LlmAgent, Model,
    ModelCapabilities, ModelError, ModelEvent, ModelRequest, ModelSettings, RunConfig, RunContext,
    SequenceId, Session, SessionError, SessionEvent, Tool, ToolContext, ToolError, ToolOutput,
    TracerHandle,
};
```

Replace it with:

```rust
use paigasus_helikon_core::{
    CancellationToken, ConversationSnapshot, Hook, HookDecision, HookEvent, HookRegistry,
    Instructions, LlmAgent, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
    ModelSettings, RunConfig, RunContext, SequenceId, Session, SessionError, SessionEvent, Tool,
    ToolContext, ToolError, ToolOutput, TracerHandle,
};
```

Then, immediately after the `PendingModel` `impl Model` block (right before `/// Barrier-synced tool:`), add:

```rust
/// On `OnRunComplete`: cancel the run from inside the hook, then suspend. The
/// agent yields the terminal event BEFORE firing `OnRunComplete`, so this
/// deterministically reproduces the "terminal already out, cancel fires during
/// the post-terminal hook await" window (SMA-421) in a single synchronous poll —
/// no sleeps, no timing races. The suspended hook is dropped when the cancel
/// tears down the agent stream.
pub struct CancelOnRunCompleteHook;

#[async_trait]
impl<Ctx> Hook<Ctx> for CancelOnRunCompleteHook
where
    Ctx: Send + Sync + 'static,
{
    async fn on_event(&self, ctx: &RunContext<Ctx>, event: &HookEvent) -> HookDecision {
        if matches!(event, HookEvent::OnRunComplete) {
            ctx.cancel().cancel();
            std::future::pending::<()>().await;
        }
        HookDecision::Allow
    }
}
```

Then, immediately after the existing `run_context_with_cancel` fn, add a hooks-carrying variant:

```rust
pub fn run_context_with_cancel_and_hooks(
    cancel: CancellationToken,
    hooks: Vec<Arc<dyn Hook<()>>>,
) -> RunContext<()> {
    let mut registry = HookRegistry::new();
    for h in hooks {
        registry.push(h);
    }
    RunContext::new(
        Arc::new(()),
        Arc::new(NoopSession) as Arc<dyn Session>,
        registry,
        TracerHandle::default(),
        cancel,
    )
}
```

- [ ] **Step 2: Add the failing `run` test to `run_control.rs`**

In `crates/paigasus-helikon-runtime-tokio/tests/run_control.rs`, add `Hook` to the core import (it becomes `use paigasus_helikon_core::{AgentInput, CancellationToken, Hook, RunConfig, RunError, Runner, Session};`) and add `CancelOnRunCompleteHook` + `run_context_with_cancel_and_hooks` to the `common` import. Then append this test:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_then_late_cancel_reports_completed() {
    // A suspending OnRunComplete hook cancels the run AFTER the terminal event
    // already went out. The completed run must be reported as Ok — the late
    // cancel must not override a genuine terminal. (SMA-421)
    let cancel = CancellationToken::new();
    let ctx = run_context_with_cancel_and_hooks(
        cancel,
        vec![std::sync::Arc::new(CancelOnRunCompleteHook) as std::sync::Arc<dyn Hook<()>>],
    );
    let agent = text_agent(MockModel::quick_hi(), Vec::new());

    let res = tokio::time::timeout(
        Duration::from_secs(5),
        TokioRunner.run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        ),
    )
    .await
    .expect("run must settle within 5s");

    assert!(res.is_ok(), "terminal must win over a late cancel: {res:?}");
    assert_eq!(res.unwrap().final_output, "hi");
}
```

- [ ] **Step 3: Run the test to verify it FAILS**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_control terminal_then_late_cancel_reports_completed`
Expected: FAIL — the assertion `res.is_ok()` panics because today's `run` returns `Err(RunError::Cancelled)` (the late cancel overrides the already-collected `RunCompleted`).

- [ ] **Step 4: Implement the `run` fix in `lib.rs`**

In `crates/paigasus-helikon-runtime-tokio/src/lib.rs`, add a file-local helper. Place it right after the `OutcomeHandle` `impl` block (before the `controlled` fn doc comment):

```rust
/// Did the run reach a terminal event? Used to decide whether a late
/// cancel/timeout may override the collected outcome (SMA-421).
fn is_terminal(ev: &AgentEvent) -> bool {
    matches!(
        ev,
        AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. }
    )
}
```

Then replace the final block of `run` (the comment + `match outcome.get()` after `finalize(&session, &recorder).await;`). The current block is:

```rust
        // A cancel/timeout outcome wins even if `collected` is Ok (the run may
        // have finished in the same poll the signal fired); `biased` keeps that
        // window small. This precedence is deliberate (SMA-321) — see
        // `prefired_cancel_still_completes_ready_run`.
        match outcome.get() {
            Outcome::Cancelled => Err(RunError::Cancelled),
            Outcome::TimedOut => Err(RunError::Timeout),
            Outcome::Completed => collected,
        }
```

Replace it with:

```rust
        // A genuine terminal event (RunCompleted/RunFailed) is the run's true
        // outcome; a cancel/timeout overrides ONLY when no terminal was observed
        // — i.e. it actually aborted the run in-flight. This closes the window
        // where a late cancel (e.g. during a suspending OnRunComplete hook) fires
        // after the terminal already went out. Cancellation is best-effort and
        // loses to a terminal that already occurred — see the Runner::run docs.
        // (SMA-421; deliberately revisits the SMA-321 precedence. The shared-core
        // hoist for durable runners is tracked as SMA-422.)
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

- [ ] **Step 5: Run the test to verify it PASSES**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_control terminal_then_late_cancel_reports_completed`
Expected: PASS — `run` now returns `Ok(RunResult)` with `final_output == "hi"`.

- [ ] **Step 6: Run the full `run_control` suite to confirm no regressions**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_control`
Expected: PASS — `cancel_aborts_in_flight_run`, `timeout_returns_timeout` (PendingModel, no terminal → override still fires), `prefired_cancel_still_completes_ready_run` (Completed outcome → unchanged), `finalize_runs_on_every_run_exit` (counts unchanged), and the new test all green.

- [ ] **Step 7: Format and lint**

Run: `cargo fmt --all`
Run: `cargo clippy -p paigasus-helikon-runtime-tokio --all-targets -- -D warnings`
Expected: clean (no diagnostics).

- [ ] **Step 8: Commit**

```bash
git add crates/paigasus-helikon-runtime-tokio/src/lib.rs \
        crates/paigasus-helikon-runtime-tokio/tests/common/mod.rs \
        crates/paigasus-helikon-runtime-tokio/tests/run_control.rs
git commit -m "fix(runtime-tokio): SMA-421 keep a genuine terminal over a late cancel in run"
```

---

## Task 2: `run_streamed` — suppress the synthetic terminal, finalize once

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/run_streamed.rs`
- Modify: `crates/paigasus-helikon-runtime-tokio/src/lib.rs`

- [ ] **Step 1: Add the failing `run_streamed` test**

In `crates/paigasus-helikon-runtime-tokio/tests/run_streamed.rs`, add `Hook` to the core import and add `CancelOnRunCompleteHook` + `run_context_with_cancel_and_hooks` to the `common` import. (`Arc`, `Duration`, `AgentEvent`, and `StreamExt` are already imported.) Then append this test:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_then_late_cancel_no_synthetic_terminal() {
    // Same window via run_streamed: the real terminal already went out, then a
    // late cancel fires during the OnRunComplete hook. The stream must NOT append
    // a second, synthetic RunFailed. (SMA-421)
    let cancel = CancellationToken::new();
    let ctx = run_context_with_cancel_and_hooks(
        cancel,
        vec![Arc::new(CancelOnRunCompleteHook) as Arc<dyn Hook<()>>],
    );
    let agent = text_agent(MockModel::quick_hi(), Vec::new());

    let rs = TokioRunner
        .run_streamed(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("stream starts");

    let events: Vec<AgentEvent> =
        tokio::time::timeout(Duration::from_secs(5), rs.events.collect::<Vec<_>>())
            .await
            .expect("stream must end within 5s");

    let terminals = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. }
            )
        })
        .count();
    assert_eq!(terminals, 1, "exactly one terminal event: {events:?}");
    assert!(
        matches!(events.last(), Some(AgentEvent::RunCompleted { .. })),
        "the single terminal must be the real RunCompleted: {events:?}"
    );
}
```

- [ ] **Step 2: Run the test to verify it FAILS**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_streamed terminal_then_late_cancel_no_synthetic_terminal`
Expected: FAIL — today's `run_streamed` yields the real `RunCompleted`, then the post-loop `match outcome.get()` synthesizes a second `RunFailed { error: "run cancelled" }`, so `terminals == 2` and `events.last()` is `RunFailed`.

- [ ] **Step 3: Implement the `run_streamed` fix in `lib.rs`**

In `crates/paigasus-helikon-runtime-tokio/src/lib.rs`, replace the entire `let out = async_stream::stream! { … };` block in `run_streamed`. The current block is:

```rust
        let out = async_stream::stream! {
            while let Some(ev) = recorded.next().await {
                // Finalize BEFORE exposing a terminal event: a consumer may stop
                // polling (and drop the stream) the moment it sees the terminal,
                // so anything after the `yield` could never run.
                if matches!(
                    ev,
                    AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. }
                ) {
                    finalize(&session, &recorder).await;
                }
                yield ev;
            }
            // Cancel/timeout: the inner stream ended without a terminal event, so
            // synthesize one — again after finalize, for the same reason.
            match outcome.get() {
                Outcome::Cancelled => {
                    finalize(&session, &recorder).await;
                    yield AgentEvent::RunFailed { error: "run cancelled".to_owned() };
                }
                Outcome::TimedOut => {
                    finalize(&session, &recorder).await;
                    yield AgentEvent::RunFailed { error: "run timed out".to_owned() };
                }
                Outcome::Completed => {}
            }
        };
```

Replace it with:

```rust
        let out = async_stream::stream! {
            let mut saw_terminal = false;
            let mut finalized = false;
            while let Some(ev) = recorded.next().await {
                // Finalize BEFORE exposing a terminal event: a consumer may stop
                // polling (and drop the stream) the moment it sees the terminal,
                // so anything after the `yield` could never run.
                if is_terminal(&ev) {
                    if !finalized {
                        finalize(&session, &recorder).await;
                        finalized = true;
                    }
                    saw_terminal = true;
                }
                yield ev;
            }
            // Synthesize a terminal ONLY when the run aborted in-flight (no real
            // terminal was ever yielded). A late cancel/timeout that fired after a
            // real terminal — e.g. during a suspending OnRunComplete hook — must
            // NOT emit a second, synthetic terminal. (SMA-421)
            if !saw_terminal {
                match outcome.get() {
                    Outcome::Cancelled => {
                        if !finalized {
                            finalize(&session, &recorder).await;
                            finalized = true;
                        }
                        yield AgentEvent::RunFailed { error: "run cancelled".to_owned() };
                    }
                    Outcome::TimedOut => {
                        if !finalized {
                            finalize(&session, &recorder).await;
                            finalized = true;
                        }
                        yield AgentEvent::RunFailed { error: "run timed out".to_owned() };
                    }
                    Outcome::Completed => {}
                }
            }
        };
```

- [ ] **Step 4: Run the test to verify it PASSES**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_streamed terminal_then_late_cancel_no_synthetic_terminal`
Expected: PASS — the stream ends after the real `RunCompleted`; `terminals == 1` and the last event is `RunCompleted`.

- [ ] **Step 5: Run the full `run_streamed` suite to confirm no regressions**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_streamed`
Expected: PASS — `streamed_event_order`, `five_tools_run_concurrently`, `streamed_cancel_emits_terminal_runfailed` (PendingModel, no terminal → synthesizes `RunFailed("run cancelled")` exactly as before), `finalize_runs_on_streamed_exits` (counts unchanged: finalize-once preserves 1 per exit), `finalize_runs_even_if_consumer_stops_at_terminal`, and the new test all green.

- [ ] **Step 6: Format and lint**

Run: `cargo fmt --all`
Run: `cargo clippy -p paigasus-helikon-runtime-tokio --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-runtime-tokio/src/lib.rs \
        crates/paigasus-helikon-runtime-tokio/tests/run_streamed.rs
git commit -m "fix(runtime-tokio): SMA-421 suppress synthetic terminal after a real one in run_streamed"
```

---

## Task 3: Document the cancel precedence (core, doc-only)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs`
- Modify: `crates/paigasus-helikon-core/src/hook.rs`

These are doc-comment additions only — no signatures, no behavior. They give the runner-agnostic precedence policy a trait-level home (review item M1) and document the caller-facing (L2) and hook (L1) consequences.

- [ ] **Step 1: Document the precedence on `Runner::run`**

In `crates/paigasus-helikon-core/src/runner.rs`, the `run` doc comment currently ends with the `Runner::resume` sentence right before the `async fn run(` signature. After that paragraph and before `async fn run(`, add a new doc paragraph:

```rust
    ///
    /// **Cancellation/timeout is best-effort and loses to a genuine terminal
    /// event that already occurred.** If the run reaches a terminal
    /// (`RunCompleted`/`RunFailed`) before — or in the same poll as — a cancel or
    /// timeout, the runner reports that real outcome (`Ok`, or the structured
    /// `Err(RunError::Agent(..))`), not `Err(RunError::Cancelled)` /
    /// `Err(RunError::Timeout)`. The cancel/timeout wins only when it aborted the
    /// run in-flight, before any terminal event. A caller therefore cannot assume
    /// "I called `cancel()` ⇒ I get `Cancelled`".
```

- [ ] **Step 2: Document the precedence on `Runner::run_streamed`**

In the same file, the `run_streamed` doc comment ends with the "must be driven to its terminal …" sentence right before `async fn run_streamed(`. After that paragraph and before `async fn run_streamed(`, add:

```rust
    ///
    /// The same cancellation precedence as [`Runner::run`] applies: once a real
    /// terminal event has been yielded, a late cancel/timeout does not append a
    /// second, synthetic terminal — the stream ends after the real one.
```

- [ ] **Step 3: Document the `OnRunComplete` best-effort contract**

In `crates/paigasus-helikon-core/src/hook.rs`, the `OnRunComplete` variant is currently:

```rust
    /// Fired once at the end of a run.
    OnRunComplete,
```

Replace it with:

```rust
    /// Fired once at the end of a run.
    ///
    /// Best-effort: if the run is cancelled while this hook is still running, the
    /// hook may be aborted mid-execution (cancellation tears down the agent
    /// stream, dropping the suspended hook). Consumers needing guaranteed
    /// post-run cleanup must not rely solely on `OnRunComplete`.
    OnRunComplete,
```

- [ ] **Step 4: Verify docs build with warnings-as-errors**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --no-deps`
Expected: success — no broken intra-doc links (the only bracketed link added is `[`Runner::run`]`, which already resolves elsewhere in this trait; `RunError::*` are plain code spans, not links).

- [ ] **Step 5: Format and lint**

Run: `cargo fmt --all`
Run: `cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/runner.rs \
        crates/paigasus-helikon-core/src/hook.rs
git commit -m "docs(core): SMA-421 document cancel-loses-to-terminal precedence and OnRunComplete best-effort"
```

---

## Task 4: Full local CI gate + PR

**Files:** none (verification only).

- [ ] **Step 1: Reproduce every CI gate locally**

Run each, in order; all must pass:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
```

Expected: all green. (The doc-coverage step requires `rustup toolchain install nightly-2026-05-01`; if absent, install it or note the skip.)

- [ ] **Step 2: Confirm the working tree is clean and the branch holds exactly three commits**

Run: `git status --porcelain` → expect empty.
Run: `git log --oneline origin/main..HEAD` → expect the three new commits (run fix, run_streamed fix, core docs) on top of the spec commit.

- [ ] **Step 3: Push and open the PR**

```bash
git push -u origin feature/sma-421-tokiorunner-a-late-canceltimeout-can-override-an-already
```

Then open the PR with a title that satisfies `pr-title.yml` (full Conventional Commits prefix + lowercase subject after the `SMA-###`):

```
fix(runtime-tokio): SMA-421 keep a genuine terminal over a late cancel/timeout
```

PR body should summarize: the post-terminal `OnRunComplete`-suspension window, the "terminal wins" decision, the two regression tests, the doc additions, and link SMA-421 (closes) + SMA-422 (follow-up). Linear auto-closes SMA-421 on merge — no manual status move.

---

## Notes for the executor

- **The deterministic harness:** `CancelOnRunCompleteHook` works because `agent.rs` yields the terminal event *before* firing `OnRunComplete` on `NextAction::Terminate`. The hook then cancels the shared token (`CancellationToken::clone` shares state — the existing `cancel_aborts_in_flight_run` relies on this) and suspends on `pending()`. `controlled`'s `biased` `select!` polls the now-`Pending` inner stream first, then the ready cancel branch, commits `Cancelled`, breaks, and drops the inner stream (which drops the suspended hook). The whole cascade is one synchronous poll — no sleeps.
- **Why `unwrap_or(true)` in `run`:** `RunResultStreaming::collect()` returns `Err` *only* after observing a `RunFailed` (verified in `core/src/runner.rs`), so an `Err` collection always means a terminal was seen. An `Ok` with no terminal in `events` is the genuine in-flight cancel/timeout (`PendingModel`) — `saw_terminal == false`, override still fires.
- **`finalized` guard:** in `run_streamed`, `finalized` makes finalize-once explicit and guards the (theoretical) multi-terminal case in the loop; under `!saw_terminal` the synth branch's `if !finalized` is defensively always-true and harmless.
- **Scope:** no public API change; the `core` edits are doc-comments only. No version bumps, no release ritual. Hoisting the precedence into a shared core resolver is deliberately deferred to SMA-422.
