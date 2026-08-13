# SMA-422 — Terminal-vs-Cancel Precedence Resolver Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the "a genuine terminal event beats a late cancel/timeout" rule out of `paigasus-helikon-runtime-tokio` into `paigasus-helikon-core`, and make all five runtime crates share the one definition.

**Architecture:** Core gains three small public items — `AgentEvent::is_terminal()`, a `RunInterrupt` enum with `run_error()` / `terminal_message()` renderings, and a pure `effective_interrupt(interrupt, saw_terminal)` resolver. Each runner keeps the control-flow shape suited to its execution model (tokio scans events, temporal short-circuits on a cached `Phase::Done`) and calls the same decision. Nothing about run behaviour changes.

**Tech Stack:** Rust 2024, MSRV 1.94, tokio, async-trait, futures, Temporal Rust SDK, axum, actix-web.

## Global Constraints

- **Pure refactor.** No user-visible behaviour change. Every pre-existing test must pass untouched.
- **Worktree root:** `/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/9e42104d-bf4f-4dd8-8424-232b6cb0309b/scratchpad/wt-sma-422`. All paths below are relative to it. **Never** use the main checkout at `/Users/smaschek/dev/paigasus/paigasus-helikon`.
- **Branch:** `feature/sma-422-hoist-the-terminal-vs-cancel-precedence-resolver-into-core`. Never run `git checkout`, `git switch`, `git reset`, or any other HEAD-moving command.
- **No version bumps.** Do not edit any `version =` field in any `Cargo.toml`, nor `release-plz.toml`, nor any `CHANGELOG.md`. release-plz handles this after merge.
- **Commit format:** `<type>(<scope>): SMA-422 <lowercase subject>`. Allowed scopes for this work: `core`, `runtime-tokio`, `runtime-temporal`, `runtime-axum`, `runtime-actix`, `runtime`, `docs`, `plan`. (`convco` enforces this via a `commit-msg` hook.)
- **Before every commit:** run `cargo fmt --all` and `cargo clippy --workspace --all-features --all-targets -- -D warnings`. The `pre-commit` hook is a deliberate no-op and will NOT catch formatting.
- **`missing_docs` is `warn` workspace-wide and the docs gate runs with `RUSTDOCFLAGS=-D warnings`.** Every new `pub` item needs a `///` doc comment.
- **Intra-doc links from a `pub` item may only target other `pub` items** — linking to a private or `pub(crate)` item fails the docs gate via `rustdoc::private_intra_doc_links`.
- **Work synchronously.** Run `cargo` commands in the foreground and wait for them. Do not background a build/test and end your turn before it reports a terminal status.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/paigasus-helikon-core/src/agent.rs` | Owns `AgentEvent`; gains `is_terminal()` + its exhaustiveness guard | 1 |
| `crates/paigasus-helikon-core/src/runner.rs` | Owns the `Runner` boundary types; gains `RunInterrupt` + `effective_interrupt` | 2 |
| `crates/paigasus-helikon-runtime-tokio/tests/run_streamed.rs` | Adds the missing streamed-timeout characterization test | 3 |
| `crates/paigasus-helikon-runtime-tokio/src/lib.rs` | Drops its local `Outcome` / `is_terminal`; calls core | 4 |
| `crates/paigasus-helikon-runtime-temporal/src/driver.rs` | `InterruptKind` → `RunInterrupt` + deprecated alias + precedence tests | 5 |
| `crates/paigasus-helikon-runtime-temporal/src/workflow.rs` | Constructs `RunInterrupt` in its `select!` | 5 |
| `crates/paigasus-helikon-runtime-temporal/src/error.rs` | Routes its interrupt→`RunError` map through `run_error()` | 5 |
| `crates/paigasus-helikon-runtime-axum/src/{event_log,handlers/*}.rs` | Drops its duplicated `is_terminal` | 6 |
| `crates/paigasus-helikon-runtime-actix/src/{event_log,handlers/*}.rs` | Drops its duplicated `is_terminal` | 6 |
| `docs/book/src/concepts/core-primitives.md` | Documents the rule for custom-`Runner` authors | 7 |

---

## Task 1: Core — `AgentEvent::is_terminal` and its exhaustiveness guard

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs:346` (doc: "Fourteen variants" → 17)
- Modify: `crates/paigasus-helikon-core/src/agent.rs:478` (add `impl AgentEvent` immediately after the enum's closing `}`)
- Test: `crates/paigasus-helikon-core/src/agent.rs` (new inline `#[cfg(test)] mod terminal_tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn AgentEvent::is_terminal(&self) -> bool` — used by Tasks 4, 5, 6.

**Note:** do **not** add an intra-doc link to `effective_interrupt` here. It does not exist until Task 2, and a broken intra-doc link fails the docs gate. Task 2 links backwards to `is_terminal` instead.

- [ ] **Step 1: Write the failing test**

Append this module at the **end** of `crates/paigasus-helikon-core/src/agent.rs` (the file already ends with a `#[cfg(test)] mod` block — add this as a sibling after it):

```rust
#[cfg(test)]
mod terminal_tests {
    use super::*;

    /// Independent classification of every [`AgentEvent`] variant.
    ///
    /// The `match` is deliberately **exhaustive, with no wildcard arm**.
    /// `#[non_exhaustive]` has no effect inside the defining crate, so adding a
    /// variant to `AgentEvent` fails to compile *here* until someone makes an
    /// explicit terminal / non-terminal decision for it. That is what stops a
    /// newly added terminal variant from silently defaulting to non-terminal
    /// inside `AgentEvent::is_terminal`'s `matches!`.
    fn classify(ev: &AgentEvent) -> bool {
        match ev {
            AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. } => true,
            AgentEvent::RunStarted { .. }
            | AgentEvent::TurnStarted { .. }
            | AgentEvent::TokenDelta { .. }
            | AgentEvent::ReasoningDelta { .. }
            | AgentEvent::ToolCallDelta { .. }
            | AgentEvent::MessageOutput { .. }
            | AgentEvent::ToolCallItem { .. }
            | AgentEvent::ToolOutputItem { .. }
            | AgentEvent::HandoffItem { .. }
            | AgentEvent::AgentUpdated { .. }
            | AgentEvent::GuardrailTriggered { .. }
            | AgentEvent::ApprovalRequested { .. }
            | AgentEvent::PermissionDenied { .. }
            | AgentEvent::RepairStarted { .. }
            | AgentEvent::StructuredOutputFailed { .. } => false,
        }
    }

    fn sample_item() -> Item {
        Item::AssistantMessage {
            content: vec![crate::ContentPart::Text {
                text: "hi".to_owned(),
            }],
            agent: None,
        }
    }

    /// One instance of every variant, so the two classifications are compared
    /// across the whole surface rather than a hand-picked sample.
    fn every_variant() -> Vec<AgentEvent> {
        vec![
            AgentEvent::RunStarted {
                agent: "a".to_owned(),
            },
            AgentEvent::TurnStarted { turn: 0 },
            AgentEvent::TokenDelta {
                text: "t".to_owned(),
            },
            AgentEvent::ReasoningDelta {
                text: "r".to_owned(),
            },
            AgentEvent::ToolCallDelta {
                call_id: "c".to_owned(),
                name: None,
                args_delta: "{}".to_owned(),
            },
            AgentEvent::MessageOutput {
                item: sample_item(),
            },
            AgentEvent::ToolCallItem {
                item: sample_item(),
            },
            AgentEvent::ToolOutputItem {
                item: sample_item(),
            },
            AgentEvent::HandoffItem {
                from: "a".to_owned(),
                to: "b".to_owned(),
            },
            AgentEvent::AgentUpdated {
                agent: "b".to_owned(),
            },
            AgentEvent::GuardrailTriggered {
                kind: GuardrailKind::InputPolicy,
                info: serde_json::Value::Null,
            },
            AgentEvent::ApprovalRequested {
                call_id: "c".to_owned(),
                tool: "t".to_owned(),
                args: serde_json::Value::Null,
            },
            AgentEvent::PermissionDenied {
                tool: "t".to_owned(),
                reason: "no".to_owned(),
            },
            AgentEvent::RepairStarted { attempt: 1 },
            AgentEvent::StructuredOutputFailed {
                schema_errors: vec!["e".to_owned()],
                final_text: "x".to_owned(),
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
            AgentEvent::RunFailed {
                error: "boom".to_owned(),
            },
        ]
    }

    #[test]
    fn every_variant_covers_the_whole_enum() {
        assert_eq!(
            every_variant().len(),
            17,
            "every_variant() must construct one instance of each AgentEvent variant"
        );
    }

    #[test]
    fn is_terminal_agrees_with_the_exhaustive_classification() {
        for ev in &every_variant() {
            assert_eq!(
                ev.is_terminal(),
                classify(ev),
                "{ev:?}: is_terminal disagrees with the exhaustive classification"
            );
        }
    }

    #[test]
    fn exactly_two_variants_are_terminal() {
        let terminal: Vec<_> = every_variant()
            .into_iter()
            .filter(AgentEvent::is_terminal)
            .collect();
        assert_eq!(
            terminal.len(),
            2,
            "expected exactly RunCompleted + RunFailed: {terminal:?}"
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p paigasus-helikon-core terminal_tests
```

Expected: FAIL to compile, `no method named 'is_terminal' found for reference '&AgentEvent'`.

- [ ] **Step 3: Write the minimal implementation**

In `crates/paigasus-helikon-core/src/agent.rs`, immediately after the `pub enum AgentEvent { ... }` closing brace (line 478) and before the `// ── Private helpers for the LlmAgent driver ──` comment, insert:

```rust
impl AgentEvent {
    /// `true` for the two events that end a run: [`AgentEvent::RunCompleted`]
    /// and [`AgentEvent::RunFailed`].
    ///
    /// This is the single definition of "terminal" every runner shares. A
    /// runner's cancel/timeout boundary loses to a terminal that already
    /// occurred; the `terminal_tests::classify` guard keeps a newly added
    /// variant from silently defaulting to non-terminal here.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::RunCompleted { .. } | Self::RunFailed { .. })
    }
}
```

- [ ] **Step 4: Fix the stale variant count**

In `crates/paigasus-helikon-core/src/agent.rs`, the `AgentEvent` doc comment at line 346 reads:

```rust
/// Fourteen variants spanning lifecycle, raw streaming deltas,
```

Change `Fourteen` to `Seventeen`. (Verified: the enum has 17 variants. No lint catches prose drift, and this task adds a test that enumerates all of them, so the count is now load-bearing.)

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test -p paigasus-helikon-core terminal_tests
```

Expected: PASS, 3 tests.

- [ ] **Step 6: Confirm the guard actually bites**

Temporarily change `is_terminal`'s body to `matches!(self, Self::RunCompleted { .. })`, re-run the tests, and confirm `is_terminal_agrees_with_the_exhaustive_classification` and `exactly_two_variants_are_terminal` both FAIL. Then restore the correct body and re-run to confirm PASS. A test that passes against wrong code is worthless; this step proves it does not.

- [ ] **Step 7: Run the format, lint, and doc gates**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps
```

Expected: all three clean.

- [ ] **Step 8: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs
git commit -m "feat(core): SMA-422 add AgentEvent::is_terminal with an exhaustiveness guard"
```

---

## Task 2: Core — `RunInterrupt` and `effective_interrupt`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs:489` (add after the `RunError` enum's closing `}`)
- Modify: `crates/paigasus-helikon-core/src/runner.rs:94-101` and `:117-119` (`Runner` doc intra-doc links)
- Test: `crates/paigasus-helikon-core/src/runner.rs` (new inline `#[cfg(test)] mod interrupt_tests`)

**Interfaces:**
- Consumes: `AgentEvent::is_terminal` (Task 1) — referenced from docs only.
- Produces, all re-exported at the crate root via `pub use runner::*` in `src/lib.rs:60`:
  - `pub enum RunInterrupt { Cancelled, TimedOut }` — `#[non_exhaustive]`, `Clone + Copy + Debug + PartialEq + Eq`
  - `pub fn RunInterrupt::run_error(self) -> RunError`
  - `pub fn RunInterrupt::terminal_message(self) -> &'static str`
  - `pub fn effective_interrupt(interrupt: Option<RunInterrupt>, saw_terminal: bool) -> Option<RunInterrupt>`

  Used by Tasks 4 and 5.

- [ ] **Step 1: Write the failing test**

Append at the **end** of `crates/paigasus-helikon-core/src/runner.rs`, after the existing `mod runconfig_tests` block:

```rust
#[cfg(test)]
mod interrupt_tests {
    use super::*;

    /// The complete truth table for the precedence rule:
    /// `{None, Cancelled, TimedOut} × {saw_terminal, !saw_terminal}`.
    #[test]
    fn interrupt_wins_only_when_no_terminal_was_seen() {
        assert_eq!(effective_interrupt(None, false), None);
        assert_eq!(effective_interrupt(None, true), None);

        assert_eq!(
            effective_interrupt(Some(RunInterrupt::Cancelled), false),
            Some(RunInterrupt::Cancelled),
            "a cancel that aborted the run in-flight must win"
        );
        assert_eq!(
            effective_interrupt(Some(RunInterrupt::Cancelled), true),
            None,
            "a genuine terminal must beat a late cancel"
        );

        assert_eq!(
            effective_interrupt(Some(RunInterrupt::TimedOut), false),
            Some(RunInterrupt::TimedOut),
            "a deadline that aborted the run in-flight must win"
        );
        assert_eq!(
            effective_interrupt(Some(RunInterrupt::TimedOut), true),
            None,
            "a genuine terminal must beat a late timeout"
        );
    }

    #[test]
    fn run_error_rendering() {
        assert!(matches!(
            RunInterrupt::Cancelled.run_error(),
            RunError::Cancelled
        ));
        assert!(matches!(
            RunInterrupt::TimedOut.run_error(),
            RunError::Timeout
        ));
    }

    #[test]
    fn terminal_message_rendering() {
        assert_eq!(RunInterrupt::Cancelled.terminal_message(), "run cancelled");
        assert_eq!(RunInterrupt::TimedOut.terminal_message(), "run timed out");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p paigasus-helikon-core interrupt_tests
```

Expected: FAIL to compile, `cannot find type 'RunInterrupt' in this scope`.

- [ ] **Step 3: Write the minimal implementation**

In `crates/paigasus-helikon-core/src/runner.rs`, immediately after the `pub enum RunError { ... }` closing brace (line 489), insert:

```rust
/// Why a runner's control boundary aborted a run before its natural end.
///
/// A runner that wraps the agent's event stream with cancellation and/or a
/// deadline observes at most one of these. Whether it actually *wins* — that
/// is, overrides the run's own outcome — is decided by [`effective_interrupt`],
/// never by the runner on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunInterrupt {
    /// The run's [`crate::CancellationToken`] fired.
    Cancelled,
    /// The run exceeded its [`RunConfig::timeout`].
    TimedOut,
}

impl RunInterrupt {
    /// The [`RunError`] this interrupt surfaces at the runner boundary.
    #[must_use]
    pub fn run_error(self) -> RunError {
        // No wildcard arm: `#[non_exhaustive]` does not apply inside the
        // defining crate, so a new variant breaks this match at compile time —
        // which is the point.
        match self {
            Self::Cancelled => RunError::Cancelled,
            Self::TimedOut => RunError::Timeout,
        }
    }

    /// Canonical `error` text for the terminal [`crate::AgentEvent::RunFailed`]
    /// a streaming runner synthesizes when this interrupt wins.
    #[must_use]
    pub fn terminal_message(self) -> &'static str {
        match self {
            Self::Cancelled => "run cancelled",
            Self::TimedOut => "run timed out",
        }
    }
}

/// Apply the runner-boundary precedence rule: **a genuine terminal event beats
/// a late cancel/timeout.**
///
/// `interrupt` is what the runner's control boundary observed (`None` if it
/// never fired). `saw_terminal` is whether the run produced a genuine
/// terminal event — see [`crate::AgentEvent::is_terminal`]. Returns the
/// interrupt only when it *wins*: when it aborted the run in-flight, before any
/// terminal.
///
/// This is the executable form of the contract [`Runner::run`] states in prose.
/// It exists so that every `Runner` — third-party implementations especially,
/// which are its main audience — applies one shared rule instead of re-deriving
/// it. The window it closes: a cancel that fires *after* the terminal already
/// went out (during a suspending `OnRunComplete` hook, say) must not
/// retroactively turn a completed run into a cancelled one.
///
/// ```
/// use paigasus_helikon_core::{effective_interrupt, RunInterrupt};
///
/// // Aborted in-flight: the interrupt wins.
/// assert_eq!(
///     effective_interrupt(Some(RunInterrupt::Cancelled), false),
///     Some(RunInterrupt::Cancelled)
/// );
/// // A terminal already occurred: the interrupt loses.
/// assert_eq!(effective_interrupt(Some(RunInterrupt::Cancelled), true), None);
/// ```
#[must_use]
pub fn effective_interrupt(
    interrupt: Option<RunInterrupt>,
    saw_terminal: bool,
) -> Option<RunInterrupt> {
    if saw_terminal {
        None
    } else {
        interrupt
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p paigasus-helikon-core interrupt_tests
cargo test -p paigasus-helikon-core --doc effective_interrupt
```

Expected: PASS — 3 unit tests and 1 doctest.

- [ ] **Step 5: Link the `Runner` trait prose to the executable rule**

The trait docs currently state the rule in prose only, which is exactly the drift this ticket removes. In `crates/paigasus-helikon-core/src/runner.rs`, find this paragraph in `Runner::run`'s doc (starts line 94):

```rust
    /// **Cancellation/timeout is best-effort and loses to a genuine terminal
    /// event that already occurred.** If the run reaches a terminal
```

Append one sentence to the end of that paragraph (i.e. after the existing line ``/// "I called `cancel()` ⇒ I get `Cancelled`".``):

```rust
    /// Implementors must apply this rule via [`effective_interrupt`] rather
    /// than re-deriving it; [`RunInterrupt`] names the two boundary interrupts.
```

Then in `Runner::run_streamed`'s doc, find (line 117):

```rust
    /// The same cancellation precedence as [`Runner::run`] applies: once a real
    /// terminal event has been yielded, a late cancel/timeout does not append a
    /// second, synthetic terminal — the stream ends after the real one.
```

Append after it:

```rust
    /// Gate the synthetic terminal on [`effective_interrupt`], and render it
    /// with [`RunInterrupt::terminal_message`].
```

- [ ] **Step 6: Run the format, lint, and doc gates**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps
```

Expected: all clean. The doc gate is the one that catches a broken intra-doc link from Step 5 — all four link targets (`effective_interrupt`, `RunInterrupt`, `RunInterrupt::terminal_message`, `AgentEvent::is_terminal`) are `pub`, so none trips `rustdoc::private_intra_doc_links`.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-core/src/runner.rs
git commit -m "feat(core): SMA-422 add RunInterrupt and the effective_interrupt precedence rule"
```

---

## Task 3: tokio — characterization test for the streamed timeout arm

Nothing currently pins `Outcome::TimedOut ⇒ RunFailed{"run timed out"}` in `run_streamed` (`src/lib.rs:249-252`), and Task 4 rewrites exactly that block. This test must PASS both before and after Task 4 — that is its entire purpose. It is a characterization test, not red-green.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/run_streamed.rs`

**Interfaces:**
- Consumes: existing test helpers from `tests/common/mod.rs` — `noop_run_context()`, `text_agent(model, tools)`, `PendingModel` (a `Model` that never resolves).
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Write the test**

Append to `crates/paigasus-helikon-runtime-tokio/tests/run_streamed.rs`, immediately after `streamed_cancel_emits_terminal_runfailed` (which ends at line 143):

```rust
/// The streamed **timeout** counterpart of
/// `streamed_cancel_emits_terminal_runfailed`. Pins the synthesized terminal's
/// message for the deadline arm, which nothing else covers. (SMA-422)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_timeout_emits_terminal_runfailed() {
    let ctx = noop_run_context();
    let agent = text_agent(Arc::new(PendingModel), Vec::new());

    let rs = TokioRunner
        .run_streamed(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::new().with_timeout(Duration::from_millis(50)),
        )
        .await
        .expect("stream starts");

    let events = tokio::time::timeout(Duration::from_secs(5), async {
        let mut s = rs.events;
        let mut evs = Vec::new();
        while let Some(ev) = s.next().await {
            evs.push(ev);
        }
        evs
    })
    .await
    .expect("stream must end within 5s of the deadline");

    assert!(
        matches!(events.last(), Some(AgentEvent::RunFailed { error }) if error == "run timed out"),
        "last event must be RunFailed(run timed out): {events:?}"
    );
}
```

- [ ] **Step 2: Run it and verify it PASSES against current code**

```bash
cargo test -p paigasus-helikon-runtime-tokio --test run_streamed streamed_timeout_emits_terminal_runfailed
```

Expected: PASS. It characterizes behaviour that already exists.

- [ ] **Step 3: Mutation-check that the assertion actually bites**

Temporarily change `crates/paigasus-helikon-runtime-tokio/src/lib.rs:251` from
`error: "run timed out".to_owned()` to `error: "run cancelled".to_owned()`, re-run the command from Step 2, and confirm it **FAILS**. Then revert the change and confirm it passes again.

Without this check you cannot distinguish "the timeout arm is correct" from "the test never reached the timeout arm" — a passing test proves nothing until you have seen it fail.

- [ ] **Step 4: Run the format and lint gates**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-tokio --all-features --all-targets -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-runtime-tokio/tests/run_streamed.rs
git commit -m "test(runtime-tokio): SMA-422 pin the streamed timeout terminal message"
```

---

## Task 4: tokio — migrate to the core helpers

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/src/lib.rs` (lines 14-17, 22-46, 48-91, 157, 174-191, 210, 237-255)

**Interfaces:**
- Consumes: `AgentEvent::is_terminal` (Task 1); `RunInterrupt`, `RunInterrupt::run_error`, `RunInterrupt::terminal_message`, `effective_interrupt` (Task 2).
- Produces: nothing new. `TokioRunner`'s public API is unchanged.

**Behaviour-equivalence note for the implementer.** Today's `Outcome` has three variants; `Option<RunInterrupt>` has `None` where `Outcome::Completed` was. The mapping is exact:
`Completed → None`, `Cancelled → Some(RunInterrupt::Cancelled)`, `TimedOut → Some(RunInterrupt::TimedOut)`.
In `run`, `effective_interrupt(i, saw) .map(run_error)` reproduces the old three-arm `match` exactly. In `run_streamed`, `if let Some(i) = effective_interrupt(i, saw)` is exactly `!saw_terminal && interrupt.is_some()`.

- [ ] **Step 1: Confirm the guard tests pass before you touch anything**

```bash
cargo test -p paigasus-helikon-runtime-tokio
```

Expected: PASS. Record the test count — it must be identical at Step 7.

- [ ] **Step 2: Replace the local `Outcome` type with the core interrupt**

In `crates/paigasus-helikon-runtime-tokio/src/lib.rs`, change the import block at lines 14-17 to add the three new items:

```rust
use paigasus_helikon_core::{
    effective_interrupt, Agent, AgentEvent, AgentInput, CancellationToken, RunConfig, RunContext,
    RunError, RunInterrupt, RunResult, RunResultStreaming, Runner, Session, SessionRecorder,
};
```

Then **delete** lines 22-46 entirely — the `Outcome` enum, `OutcomeHandle`, its `impl`, and the local `is_terminal` fn — and replace them with:

```rust
/// Read handle for the interrupt committed by [`controlled`], if any.
struct InterruptHandle(Arc<Mutex<Option<RunInterrupt>>>);

impl InterruptHandle {
    fn get(&self) -> Option<RunInterrupt> {
        *self.0.lock().unwrap()
    }
}
```

- [ ] **Step 3: Update `controlled` to commit an `Option<RunInterrupt>`**

Replace the whole of `controlled` (lines 48-91) with:

```rust
/// Wrap an agent event stream with cancel/deadline control.
///
/// Passes agent events through. On cancellation or deadline it commits the
/// reason into the returned handle and ends the stream (dropping the inner
/// stream cancels nested in-flight awaits within one poll). The interrupt, if
/// any, is committed *before* the terminating `None`, so a caller reading the
/// handle after draining never sees a stale value. A run that ends naturally
/// commits nothing and the handle stays `None`.
fn controlled(
    mut stream: BoxStream<'static, AgentEvent>,
    cancel: CancellationToken,
    timeout: Option<Duration>,
) -> (BoxStream<'static, AgentEvent>, InterruptHandle) {
    let cell = Arc::new(Mutex::new(None));
    let handle = InterruptHandle(Arc::clone(&cell));
    let out = async_stream::stream! {
        let sleep = async move {
            match timeout {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                biased;
                maybe_ev = stream.next() => {
                    match maybe_ev {
                        Some(ev) => yield ev,
                        None => break, // inner stream done => no interrupt
                    }
                }
                () = cancel.cancelled() => {
                    *cell.lock().unwrap() = Some(RunInterrupt::Cancelled);
                    break;
                }
                () = &mut sleep => {
                    *cell.lock().unwrap() = Some(RunInterrupt::TimedOut);
                    break;
                }
            }
        }
    };
    (Box::pin(out), handle)
}
```

- [ ] **Step 4: Migrate `run`**

At line 157, rename the binding:

```rust
        let (controlled_stream, interrupt) = controlled(stream, cancel, timeout);
```

Then replace lines 174-191 (the comment block, the `saw_terminal` let, and the `match`) with:

```rust
        // A genuine terminal event (RunCompleted/RunFailed) is the run's true
        // outcome; a cancel/timeout overrides ONLY when no terminal was
        // observed — i.e. it actually aborted the run in-flight. The rule
        // itself lives in core as `effective_interrupt` so every runner applies
        // one definition (SMA-422); see `Runner::run`'s docs for the contract
        // and SMA-421 for the bug it closes.
        let saw_terminal = collected
            .as_ref()
            .map(|r| r.events.iter().any(AgentEvent::is_terminal))
            .unwrap_or(true); // Err(_) from collect() ⇔ a RunFailed was observed

        match effective_interrupt(interrupt.get(), saw_terminal).map(RunInterrupt::run_error) {
            Some(err) => Err(err),
            None => collected,
        }
```

- [ ] **Step 5: Migrate `run_streamed`**

At line 210, rename the binding the same way:

```rust
        let (controlled_stream, interrupt) = controlled(stream, cancel, timeout);
```

Then replace lines 237-255 (the trailing comment block and the `if !saw_terminal { match ... }`) with:

```rust
            // Synthesize a terminal ONLY when the run aborted in-flight (no real
            // terminal was ever yielded). Reaching here with `!saw_terminal`
            // means the loop never finalized, so finalize directly. A late
            // cancel/timeout that fired AFTER a real terminal — e.g. during a
            // suspending OnRunComplete hook — must NOT emit a second, synthetic
            // terminal; `effective_interrupt` is what decides that (SMA-422,
            // closing SMA-421). Note the natural-completion case (no terminal,
            // no interrupt) deliberately does not finalize here — pre-existing
            // behaviour, preserved.
            if let Some(i) = effective_interrupt(interrupt.get(), saw_terminal) {
                finalize(&session, &recorder).await;
                yield AgentEvent::RunFailed { error: i.terminal_message().to_owned() };
            }
```

- [ ] **Step 6: Re-point the surviving SMA-421 rationale**

Confirm no stale reference to the deleted local helpers remains:

```bash
grep -n "Outcome\|is_terminal" crates/paigasus-helikon-runtime-tokio/src/lib.rs
```

Expected: only `AgentEvent::is_terminal` on the `saw_terminal` line. If any `Outcome::` or bare `is_terminal(` remains, you missed a site.

- [ ] **Step 7: Run the full crate test suite**

```bash
cargo test -p paigasus-helikon-runtime-tokio
```

Expected: PASS, with the **same test count as Step 1**. These are the SMA-421 regression tests plus Task 3's — `run_control.rs` guards `run` (`cancel_aborts_in_flight_run`, `timeout_returns_timeout`, `prefired_cancel_still_completes_ready_run`, `terminal_then_late_cancel_reports_completed`) and `run_streamed.rs` guards `run_streamed` (`streamed_cancel_emits_terminal_runfailed`, `terminal_then_late_cancel_no_synthetic_terminal`, `streamed_timeout_emits_terminal_runfailed`). If any fails, the migration is not behaviour-preserving — fix it, do not adjust the test.

- [ ] **Step 8: Run the format, lint, and doc gates**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-tokio --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-runtime-tokio --all-features --no-deps
```

- [ ] **Step 9: Commit**

```bash
git add crates/paigasus-helikon-runtime-tokio/src/lib.rs
git commit -m "refactor(runtime-tokio): SMA-422 use core's effective_interrupt and is_terminal"
```

---

## Task 5: temporal — share the interrupt vocabulary and test the precedence branch

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/driver.rs:84-92` (replace `InterruptKind` with a deprecated alias), `:340-353` (`interrupt`), `:722` (existing test)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/workflow.rs:51` (import), `:277-278` (`select!` arms)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/error.rs:57-58` (route through `run_error()`)
- Test: `crates/paigasus-helikon-runtime-temporal/src/driver.rs` (two new tests in the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `RunInterrupt`, `effective_interrupt`, `AgentEvent::is_terminal` (Tasks 1-2).
- Produces: `DurableDriver::interrupt(self, kind: RunInterrupt) -> DurableRunOutcome` (signature change); `pub type InterruptKind` remains as a deprecated alias.

**Replay-safety note:** `RunInterrupt` never crosses a serialization boundary — only `RunStatusPayload` does (`payloads.rs:61-70`). This change does not alter the workflow's command sequence, so in-flight replays are unaffected.

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` in `crates/paigasus-helikon-runtime-temporal/src/driver.rs`, after `interrupt_returns_partial_events` (which ends at line 736):

```rust
    /// SMA-422 / SMA-421 for the durable driver: once the driver has reached a
    /// terminal outcome, a later `interrupt` must NOT retroactively relabel the
    /// run as cancelled. This is the `Phase::Done` short-circuit — temporal's
    /// equivalent of `TokioRunner`'s `saw_terminal` gate — which had no test.
    #[test]
    fn terminal_wins_over_late_interrupt() {
        let mut d = DurableDriver::new(input(vec![user("hi")]), plan_no_tools());
        assert_matches!(d.next_effect(), DriverEffect::RenderInstructions);
        d.apply_instructions("sys".to_owned());
        assert_matches!(d.next_effect(), DriverEffect::CallModel(_));
        d.apply_model(model_text_turn("hello"));
        assert_matches!(d.next_effect(), DriverEffect::Finished(_));

        // The interrupt arrives AFTER the run already finished.
        let outcome = d.interrupt(RunInterrupt::Cancelled);
        assert_matches!(&outcome.status, RunStatusPayload::Completed(_));

        // The driver's structural gate and core's scanning gate must agree.
        let saw_terminal = outcome.events.iter().any(AgentEvent::is_terminal);
        assert!(saw_terminal, "a finished run must carry a terminal event");
        assert_eq!(
            paigasus_helikon_core::effective_interrupt(Some(RunInterrupt::Cancelled), saw_terminal),
            None,
            "core's rule must agree with the driver's Phase::Done short-circuit"
        );
    }

    /// The same precedence for a terminal *failure*: a late timeout must not
    /// mask the run's real `AgentFailed` status.
    #[test]
    fn terminal_failure_wins_over_late_interrupt() {
        let mut d = DurableDriver::new(input(vec![user("hi")]), plan_no_tools());
        assert_matches!(d.next_effect(), DriverEffect::RenderInstructions);
        d.apply_instructions("sys".to_owned());
        assert_matches!(d.next_effect(), DriverEffect::CallModel(_));
        d.apply_model_failure(ErrorKindPayload::Model {
            message: "connection lost".to_owned(),
        });
        assert_matches!(d.next_effect(), DriverEffect::Finished(_));

        let outcome = d.interrupt(RunInterrupt::TimedOut);
        assert_matches!(
            &outcome.status,
            RunStatusPayload::AgentFailed(ErrorKindPayload::Model { .. })
        );
        assert!(outcome.events.iter().any(AgentEvent::is_terminal));
    }
```

- [ ] **Step 2: Run them to verify they fail**

```bash
cargo test -p paigasus-helikon-runtime-temporal --lib terminal
```

Expected: FAIL to compile, `cannot find value 'RunInterrupt' in this scope`.

- [ ] **Step 3: Replace `InterruptKind` with the core enum plus a deprecated alias**

In `crates/paigasus-helikon-runtime-temporal/src/driver.rs`, delete the enum at lines 84-92 and put in its place:

```rust
/// Former name of [`paigasus_helikon_core::RunInterrupt`].
///
/// `driver` is a `pub mod`, so this name is public API; it is kept as an alias
/// so downstream `crate::driver::InterruptKind` paths (and
/// `InterruptKind::Cancelled` variant access, which resolves through a type
/// alias) keep working. The rule this type participates in now lives in core
/// (SMA-422).
#[deprecated(note = "renamed; use `paigasus_helikon_core::RunInterrupt` instead")]
pub type InterruptKind = paigasus_helikon_core::RunInterrupt;
```

Then add `RunInterrupt` to the crate's existing `use paigasus_helikon_core::{...}` import at the top of `driver.rs`.

- [ ] **Step 4: Update `interrupt` to take `RunInterrupt`**

Replace lines 340-353 with:

```rust
    pub fn interrupt(self, kind: RunInterrupt) -> DurableRunOutcome {
        if let Phase::Done(outcome) = self.phase {
            return outcome;
        }
        let status = match kind {
            RunInterrupt::Cancelled => RunStatusPayload::Cancelled,
            RunInterrupt::TimedOut => RunStatusPayload::TimedOut,
            // `RunInterrupt` is `#[non_exhaustive]`, so a wildcard is required
            // here. An interrupt kind this crate does not know is still a run
            // that did not finish, so surface it as a failure rather than
            // mislabeling it as one of the two known interrupts. Panicking is
            // not an option: this runs inside a Temporal workflow, where a
            // workflow-task failure retries indefinitely.
            other => RunStatusPayload::AgentFailed(ErrorKindPayload::Other {
                message: format!("unhandled run interrupt: {other:?}"),
            }),
        };
        DurableRunOutcome {
            status,
            events: self.events,
            usage: self.usage,
        }
    }
```

Leave the doc comment above `interrupt` (lines 335-339) as it is — it already describes the `Phase::Done` short-circuit correctly.

- [ ] **Step 5: Update the existing test and the workflow call site**

In `driver.rs:722`, change `d.interrupt(InterruptKind::Cancelled)` to `d.interrupt(RunInterrupt::Cancelled)`.

In `crates/paigasus-helikon-runtime-temporal/src/workflow.rs:51`, remove `InterruptKind` from the `use crate::driver::{...}` list, and add `RunInterrupt` to the file's `use paigasus_helikon_core::{...}` import.

In `workflow.rs:277-278`, change the `select!` arms:

```rust
            _ = deadline => RunInterrupt::TimedOut,
            _ = cancelled => RunInterrupt::Cancelled,
```

- [ ] **Step 6: Route `error.rs`'s interrupt mapping through the canonical rendering**

In `crates/paigasus-helikon-runtime-temporal/src/error.rs`, replace lines 57-58:

```rust
        RunStatusPayload::Cancelled => Err(paigasus_helikon_core::RunInterrupt::Cancelled.run_error()),
        RunStatusPayload::TimedOut => Err(paigasus_helikon_core::RunInterrupt::TimedOut.run_error()),
```

This removes the fourth copy of the interrupt→`RunError` map. The existing tests `cancelled_maps_to_run_error_cancelled` and `timed_out_maps_to_run_error_timeout` (`error.rs:212-235`) are unchanged and guard the routing.

- [ ] **Step 7: Run the tests to verify they pass**

```bash
cargo test -p paigasus-helikon-runtime-temporal --lib
```

Expected: PASS, including the two new tests and the unchanged `outcome_mapping_tests`.

- [ ] **Step 8: Prove the new tests actually bite**

Temporarily delete the `if let Phase::Done(outcome) = self.phase { return outcome; }` short-circuit from `interrupt`, re-run Step 7, and confirm `terminal_wins_over_late_interrupt` and `terminal_failure_wins_over_late_interrupt` both **FAIL**. Then restore it and confirm PASS. This is the branch the tests exist to protect; verify they detect its loss.

- [ ] **Step 9: Confirm no internal use of the deprecated alias survives**

```bash
grep -rn "InterruptKind" crates/paigasus-helikon-runtime-temporal/src/
```

Expected: exactly one hit — the `pub type` alias definition itself. Any other hit would trip `-D warnings` via the `deprecated` lint.

- [ ] **Step 10: Run the format, lint, and doc gates**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-temporal --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-runtime-temporal --all-features --no-deps
```

- [ ] **Step 11: Commit**

```bash
git add crates/paigasus-helikon-runtime-temporal/src/driver.rs \
        crates/paigasus-helikon-runtime-temporal/src/workflow.rs \
        crates/paigasus-helikon-runtime-temporal/src/error.rs
git commit -m "refactor(runtime-temporal): SMA-422 adopt core RunInterrupt and test the precedence branch"
```

---

## Task 6: axum + actix — delete both duplicated `is_terminal` copies

These two crates are held to **byte-identical wire parity** by `tests/runtime-http-conformance/tests/parity.rs`, so they migrate together in one commit. Doing one without the other is precisely the drift this ticket removes.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-axum/src/event_log.rs:16-25` (delete fn), `:107`, `:205`, `:221`
- Modify: `crates/paigasus-helikon-runtime-axum/src/handlers/events.rs:29`, `:111`
- Modify: `crates/paigasus-helikon-runtime-axum/src/handlers/runs.rs:58`, `:417`
- Modify: `crates/paigasus-helikon-runtime-actix/src/event_log.rs:16-25` (delete fn), `:107`, `:205`, `:221`
- Modify: `crates/paigasus-helikon-runtime-actix/src/handlers/events.rs:27`, `:116`
- Modify: `crates/paigasus-helikon-runtime-actix/src/handlers/runs.rs:80`, `:496`

**Interfaces:**
- Consumes: `AgentEvent::is_terminal` (Task 1).
- Produces: nothing. `synthetic_terminal_frame` in both crates' `registry.rs` is **unchanged** — its CWE-209 public strings are transport policy, not the precedence rule.

- [ ] **Step 1: Delete the duplicated helper in both crates**

In **both** `crates/paigasus-helikon-runtime-axum/src/event_log.rs` and `crates/paigasus-helikon-runtime-actix/src/event_log.rs`, delete lines 16-25 — the doc comment and the function:

```rust
/// Returns `true` for events that signal end-of-run.
///
/// Only [`AgentEvent::RunCompleted`] and [`AgentEvent::RunFailed`] are terminal;
/// all other variants are non-terminal.
pub(crate) fn is_terminal(ev: &AgentEvent) -> bool {
    matches!(
        ev,
        AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. }
    )
}
```

- [ ] **Step 2: Update the call sites in both crates**

Change every bare `is_terminal(&ev)` to the method form `ev.is_terminal()`:

- `axum/src/event_log.rs:107` — `let terminal = is_terminal(&ev);` → `let terminal = ev.is_terminal();`
- `axum/src/event_log.rs:205` and `:221` — `state.done = is_terminal(&ev);` → `state.done = ev.is_terminal();`
- `axum/src/handlers/events.rs:111` — `if is_terminal(&ev) {` → `if ev.is_terminal() {`
- `axum/src/handlers/runs.rs:417` — `state.saw_terminal |= is_terminal(&ev);` → `state.saw_terminal |= ev.is_terminal();`
- `actix/src/event_log.rs:107`, `:205`, `:221` — identical to axum's
- `actix/src/handlers/events.rs:116` — `if is_terminal(&ev) {` → `if ev.is_terminal() {`
- `actix/src/handlers/runs.rs:496` — `state.saw_terminal |= is_terminal(&ev);` → `state.saw_terminal |= ev.is_terminal();`

- [ ] **Step 3: Remove the now-unused imports**

- `axum/src/handlers/events.rs:29` — drop `event_log::is_terminal,` from the `use crate::{...}` list
- `axum/src/handlers/runs.rs:58` — change `event_log::{is_terminal, EventLog},` to `event_log::EventLog,`
- `actix/src/handlers/events.rs:27` — drop `event_log::is_terminal,` from the `use crate::{...}` list
- `actix/src/handlers/runs.rs:80` — change `event_log::{is_terminal, EventLog},` to `event_log::EventLog,`

Each `event_log.rs` may also now have an unused `AgentEvent` import — leave it if the file still uses the type elsewhere (it does, in the `EventLog` signatures); clippy in Step 5 will tell you if not.

- [ ] **Step 4: Verify no copy survives anywhere in the workspace**

```bash
grep -rn "fn is_terminal" crates/
```

Expected: exactly two hits — `paigasus-helikon-core/src/agent.rs` (the new method) and `paigasus-helikon-runtime-agentcore/src/a2a/types.rs:92` (`TaskState::is_terminal`, an unrelated A2A task-state predicate that must NOT be touched).

- [ ] **Step 5: Run both crates' tests and the parity suite**

```bash
cargo test -p paigasus-helikon-runtime-axum --all-features
cargo test -p paigasus-helikon-runtime-actix --all-features
cargo test -p paigasus-helikon-runtime-http-conformance --all-features
```

Expected: PASS. The third is the axum↔actix byte-parity suite — the gate that makes doing both crates in one commit non-negotiable.

- [ ] **Step 6: Run the format, lint, and no-default-features gates**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-axum -p paigasus-helikon-runtime-actix --all-features --all-targets -- -D warnings
cargo build -p paigasus-helikon-runtime-axum --no-default-features
cargo build -p paigasus-helikon-runtime-actix --no-default-features
```

The last two mirror CI's required `build-no-default-features` job, which is the only gate compiling these crates with default features off.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-runtime-axum/src crates/paigasus-helikon-runtime-actix/src
git commit -m "refactor(runtime): SMA-422 use core AgentEvent::is_terminal in axum and actix"
```

---

## Task 7: Documentation and the full CI gate sweep

**Files:**
- Modify: `docs/book/src/concepts/core-primitives.md:17`

**Interfaces:**
- Consumes: everything from Tasks 1-6.
- Produces: nothing.

- [ ] **Step 1: Document the rule for custom-`Runner` authors**

In `docs/book/src/concepts/core-primitives.md`, the `Runner<Ctx>` bullet at line 17 currently ends:

```markdown
Object-safe: methods take `&dyn Agent<Ctx>`.
```

Append to that same bullet:

```markdown
 Runners that add a cancel/deadline boundary must honour one precedence rule: **a genuine terminal event (`RunCompleted`/`RunFailed`) beats a late cancel or timeout**, which wins only when it aborted the run in-flight. Core owns that rule rather than each runner re-deriving it — call `effective_interrupt(interrupt, saw_terminal)` with `RunInterrupt::{Cancelled, TimedOut}`, and use `AgentEvent::is_terminal` to compute `saw_terminal`. `RunInterrupt::run_error()` and `::terminal_message()` give the canonical `RunError` and synthetic-terminal text.
```

- [ ] **Step 2: Verify the book builds clean**

```bash
mdbook build docs/book
```

Expected: clean. `[output.linkcheck] warning-policy = "error"`, so a broken link fails the build. If `mdbook` is not installed, say so and skip this step rather than installing it.

- [ ] **Step 3: Run every CI gate, exactly as CI runs them**

Run these from the worktree root, in order, in the foreground:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo build -p paigasus-helikon-runtime-axum --no-default-features
cargo build -p paigasus-helikon-runtime-actix --no-default-features
```

`cargo test --workspace --all-features` is the exact gate — do not substitute per-crate runs, which have masked feature-unification failures in this workspace before.

**If the bedrock tests fail on macOS with `NATIVE_ROOTS` errors:** that failure tracks the checkout path, not the code. This plan already runs inside a scratchpad worktree, where it does not reproduce. If you see it, report it rather than "fixing" it.

- [ ] **Step 4: Run the doc-coverage gate**

```bash
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
```

Expected: PASS. This task adds only documented `pub` items, so coverage should rise. If the nightly toolchain is not installed, report that and skip rather than installing it.

- [ ] **Step 5: Confirm no version or changelog file was touched**

```bash
git diff origin/main --stat -- '**/Cargo.toml' '**/CHANGELOG.md' release-plz.toml
```

Expected: **empty output**. release-plz owns every version bump; a manual bump here would defeat the facade cascade.

- [ ] **Step 6: Commit**

```bash
git add docs/book/src/concepts/core-primitives.md
git commit -m "docs(docs): SMA-422 document the terminal-vs-cancel precedence rule"
```

---

## Self-Review

**Spec coverage** — every section of the spec maps to a task:

| Spec requirement | Task |
|---|---|
| `AgentEvent::is_terminal` in `agent.rs` | 1 |
| Exhaustiveness guard, agentcore-style | 1 |
| `agent.rs:346` "Fourteen variants" → 17 | 1 |
| `RunInterrupt` (`#[non_exhaustive]`) + `run_error` + `terminal_message` | 2 |
| `effective_interrupt` + public-contract justification in its doc | 2 |
| `effective_interrupt` truth table + rendering tests | 2 |
| `Runner::run` / `run_streamed` intra-doc links | 2 |
| New `streamed_timeout_emits_terminal_runfailed` | 3 |
| tokio: delete `Outcome` / local `is_terminal`; `InterruptHandle` rename | 4 |
| tokio: `controlled` doc comment rewritten | 4 |
| tokio: `unwrap_or(true)` + comment preserved verbatim | 4 |
| tokio: finalize gap knowingly preserved (noted in the new comment) | 4 |
| temporal: `InterruptKind` → `RunInterrupt` at all 5 sites | 5 |
| temporal: deprecated `InterruptKind` type alias | 5 |
| temporal: non-panicking wildcard arm | 5 |
| temporal: `error.rs:57-58` routed through `run_error()` | 5 |
| temporal: `terminal_wins_over_late_interrupt` (+ failure variant) | 5 |
| temporal: replay-safety stated | 5 (task preamble) |
| axum: delete `is_terminal`, 5 call sites | 6 |
| actix: delete `is_terminal`, 5 call sites | 6 |
| `synthetic_terminal_frame` unchanged in both | 6 |
| agentcore untouched | — (no task; absence is the deliverable, and Task 6 Step 4's grep asserts its unrelated `TaskState::is_terminal` survives) |
| mdBook `core-primitives.md` | 7 |
| READMEs unchanged | — (conscious no-op per spec) |
| No manual version bumps | Global Constraints + Task 7 Step 5 |

**Placeholder scan:** none. Every code step carries the literal code to write; every command step carries the exact command and its expected result.

**Type consistency:** `RunInterrupt`, `effective_interrupt`, `run_error()`, `terminal_message()`, `is_terminal()`, and `InterruptHandle` are spelled identically in Tasks 1-6. `effective_interrupt`'s signature `(Option<RunInterrupt>, bool) -> Option<RunInterrupt>` is used consistently in Tasks 2, 4, and 5.

**Ordering:** Task 1 deliberately omits a forward link to `effective_interrupt` (which does not exist until Task 2), because a broken intra-doc link fails the `-D warnings` docs gate. Task 2 links backwards to `is_terminal` instead. Tasks 4, 5, and 6 each depend only on Tasks 1-2 and are mutually independent.
