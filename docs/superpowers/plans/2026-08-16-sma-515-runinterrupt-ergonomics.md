# SMA-515 `RunInterrupt` Ergonomics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the last hand-rolled piece of the interrupt terminal frame into `paigasus-helikon-core` — add `RunInterrupt::terminal_event()` and `impl From<RunInterrupt> for RunError` — and migrate the two runners that build that frame by hand.

**Architecture:** Both new core items **delegate** to the existing accessors (`terminal_message()` / `run_error()`) rather than re-matching on `self`, so a future `RunInterrupt` variant breaks compilation in exactly one place. `runtime-tokio` swaps one expression. `runtime-temporal` changes its rendering helper from `-> Option<String>` to `-> Option<AgentEvent>` and extracts the run's double-terminal guard into a named, unit-testable helper — the guard is currently untested at any level, and this is the only part of the change that could silently regress anything.

**Tech Stack:** Rust 2024, MSRV 1.94. No new dependencies. Tests are in-file `#[cfg(test)]` modules plus one rustdoc doctest.

**Spec:** `docs/superpowers/specs/2026-08-16-sma-515-runinterrupt-ergonomics-design.md`

## Global Constraints

- **Zero behaviour change.** No message text, no event variant, no `Option` inhabitance, and no guard semantics may differ before and after. Everything here is restructuring.
- **Delegate, never re-match.** Neither `terminal_event()` nor `From::from` may contain its own `match self`. One wildcard-free match per rendering is what makes the `#[non_exhaustive]` compile-break guard meaningful.
- **Never `assert_eq!` two `AgentEvent`s.** `AgentEvent` derives no `PartialEq` (`crates/paigasus-helikon-core/src/agent.rs:353`; no manual impl exists in core). Destructure and compare the payload. Do **not** add `PartialEq` to `AgentEvent` — it is a `#[non_exhaustive]` public enum whose payloads (`Item`, `ContentPart`, `GuardrailKind`) would all be dragged in. Do **not** compare `format!("{ev:?}")` strings.
- **Assert against accessors, not literals.** `terminal_message_rendering` (`crates/paigasus-helikon-core/src/runner.rs:734`) already pins `"run cancelled"` / `"run timed out"`. New tests compare against `terminal_message()`, not restated strings. The one exception is the pre-existing temporal test that deliberately pins both (keep it as-is).
- **No version bumps and no `chore(release)` commit.** All three crates are already released; release-plz handles patch bumps and the facade cascade from the merged commit's paths. A manual bump would defeat the cascade.
- **Commit format:** `<type>(<scope>): SMA-515 <lowercase subject>`. Scopes `core`, `runtime-tokio`, `runtime-temporal` are all in `.versionrc`'s `scopeRegex`. Commits are signed via a 1Password SSH key — if a commit fails with "failed to fill whole buffer", the vault is locked: stop and ask, do not bypass signing.
- **Run `cargo fmt --all` before every commit.** The pre-commit hook is a deliberate no-op; `cargo fmt` failures surface only at push time otherwise.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/paigasus-helikon-core/src/runner.rs` | Owns `RunInterrupt` and its renderings; gains `terminal_event()`, the `From` impl, and 4 tests. Also carries the normative `Runner::run_streamed` contract rustdoc. | 1, 2 |
| `docs/book/src/concepts/core-primitives.md` | Public prose summary of the runner contract. | 1 |
| `crates/paigasus-helikon-runtime-tokio/src/lib.rs` | Consumes `terminal_event()` in `run_streamed`. | 3 |
| `crates/paigasus-helikon-runtime-temporal/src/runner.rs` | Renders and appends the synthetic terminal; helper split into "render" + "append with guard". | 4 |

---

### Task 1: `RunInterrupt::terminal_event()` in core, plus the docs that describe it

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs:122-123` (the `Runner::run_streamed` contract rustdoc)
- Modify: `crates/paigasus-helikon-core/src/runner.rs:523-531` (`terminal_message`'s rustdoc) and insert the new method after it
- Modify: `crates/paigasus-helikon-core/src/runner.rs` → `mod interrupt_tests` (append before the closing brace at line 758)
- Modify: `docs/book/src/concepts/core-primitives.md:17`

**Interfaces:**
- Consumes: `RunInterrupt::terminal_message(self) -> &'static str` and `AgentEvent::RunFailed { error: String }`, both already present. `AgentEvent` is already imported at `runner.rs:14` — no new `use` is needed.
- Produces: `RunInterrupt::terminal_event(self) -> AgentEvent`, and a test-module fixture `fn every_interrupt() -> Vec<RunInterrupt>` that Task 2 reuses.

- [ ] **Step 1: Write the failing tests**

Append inside `mod interrupt_tests` in `crates/paigasus-helikon-core/src/runner.rs`, after `terminal_message_versus_run_error_display` and before the module's closing brace:

```rust
    /// Every [`RunInterrupt`] variant, as the shared fixture for the tests
    /// below. Mirrors `agent.rs`'s `terminal_tests::every_variant`.
    ///
    /// `_exhaustive`'s `match` is deliberately wildcard-free: `#[non_exhaustive]`
    /// has no effect inside the defining crate, so adding a variant fails to
    /// compile *here* — one place — until someone makes an explicit decision.
    /// It cannot by itself force the new variant into the returned vec; that is
    /// what `every_interrupt_covers_the_whole_enum` is the second nudge for.
    fn every_interrupt() -> Vec<RunInterrupt> {
        fn _exhaustive(i: RunInterrupt) {
            match i {
                RunInterrupt::Cancelled | RunInterrupt::TimedOut => {}
            }
        }
        vec![RunInterrupt::Cancelled, RunInterrupt::TimedOut]
    }

    #[test]
    fn every_interrupt_covers_the_whole_enum() {
        // Distinct discriminants, not length: a plain count would also pass for
        // two copies of the same variant.
        let discriminants: std::collections::HashSet<_> = every_interrupt()
            .iter()
            .map(std::mem::discriminant)
            .collect();
        assert_eq!(
            discriminants.len(),
            2,
            "every_interrupt() must construct one instance of each distinct RunInterrupt variant"
        );
    }

    /// `terminal_event` must agree with `terminal_message` by construction.
    /// Asserted against the accessor, never a restated literal — the literals
    /// are already pinned by `terminal_message_rendering`.
    #[test]
    fn terminal_event_agrees_with_terminal_message() {
        for i in every_interrupt() {
            match i.terminal_event() {
                AgentEvent::RunFailed { error } => {
                    assert_eq!(error, i.terminal_message());
                }
                other => panic!("{i:?}: terminal_event must be RunFailed, got {other:?}"),
            }
        }
    }

    /// The synthesized frame must be classified terminal by the very predicate
    /// runners use to compute `saw_terminal` for `effective_interrupt`.
    #[test]
    fn terminal_event_is_terminal() {
        for i in every_interrupt() {
            assert!(
                i.terminal_event().is_terminal(),
                "{i:?}: the synthesized frame must satisfy AgentEvent::is_terminal"
            );
        }
    }
```

Note the `match` rather than a `let … else`: binding `error` by value moves the event, so the `else` block could not name it in the panic message.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core interrupt_tests`
Expected: **compile error**, `no method named 'terminal_event' found for enum 'RunInterrupt'`. A compile failure is the correct red state here — the method does not exist yet.

- [ ] **Step 3: Add the method**

In `crates/paigasus-helikon-core/src/runner.rs`, inside `impl RunInterrupt`, insert after `terminal_message`'s closing brace (line 531) and before the impl block's closing brace:

```rust
    /// The terminal [`AgentEvent`] a streaming runner yields when this interrupt
    /// wins — see [`effective_interrupt`].
    ///
    /// Renders through [`RunInterrupt::terminal_message`], so a runner never
    /// assembles the frame itself and every runner emits identical bytes for the
    /// same interrupt. There is deliberately no second `match self` here: one
    /// wildcard-free match per rendering is what makes the `#[non_exhaustive]`
    /// compile-break guard worth anything.
    #[must_use]
    pub fn terminal_event(self) -> AgentEvent {
        AgentEvent::RunFailed {
            error: self.terminal_message().to_owned(),
        }
    }
```

- [ ] **Step 4: Point `terminal_message`'s rustdoc at it**

Replace the doc comment at `crates/paigasus-helikon-core/src/runner.rs:523-524`:

```rust
    /// Canonical `error` text for the terminal [`crate::AgentEvent::RunFailed`]
    /// a streaming runner synthesizes when this interrupt wins.
```

with:

```rust
    /// Canonical `error` text for the terminal [`crate::AgentEvent::RunFailed`]
    /// a streaming runner synthesizes when this interrupt wins.
    ///
    /// This is the text alone. Use [`RunInterrupt::terminal_event`] when you
    /// need the whole frame — a runner should not assemble one by hand.
```

- [ ] **Step 5: Retarget the normative `Runner` contract rustdoc**

This paragraph is what third-party runner authors read on docs.rs, and no CI gate catches staleness. Replace `crates/paigasus-helikon-core/src/runner.rs:122-123`:

```rust
    /// Gate the synthetic terminal on [`effective_interrupt`], and render it
    /// with [`RunInterrupt::terminal_message`].
```

with:

```rust
    /// Gate the synthetic terminal on [`effective_interrupt`], and build it with
    /// [`RunInterrupt::terminal_event`] rather than assembling the frame by hand.
```

Leave `Runner::run`'s doc at lines 102-103 alone — it names `effective_interrupt` and `RunInterrupt` but not the rendering, so it is not stale.

- [ ] **Step 6: Update the book**

In `docs/book/src/concepts/core-primitives.md:17`, replace the final sentence of the `Runner<Ctx>` bullet:

```
`RunInterrupt::run_error()` and `::terminal_message()` give the canonical `RunError` and synthetic-terminal text.
```

with:

```
`RunInterrupt::run_error()` gives the canonical `RunError`, and `::terminal_event()` builds the synthetic terminal frame itself — `::terminal_message()` is the text alone.
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core interrupt_tests`
Expected: PASS — 7 tests in `interrupt_tests` (4 pre-existing + 3 new; `every_interrupt` is a fixture, not a test).

- [ ] **Step 8: Verify the docs gate and the book**

Run:
```bash
cargo fmt --all
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --no-deps
mdbook build docs/book
```
Expected: both clean. The `cargo doc` run is what catches a broken intra-doc link in the two rustdoc edits; `mdbook build` is configured with `warning-policy = "error"` on its link checker.

- [ ] **Step 9: Commit**

```bash
git add crates/paigasus-helikon-core/src/runner.rs docs/book/src/concepts/core-primitives.md
git commit -m "feat(core): SMA-515 add RunInterrupt::terminal_event"
```

---

### Task 2: `impl From<RunInterrupt> for RunError`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs` — new `impl` block immediately after `impl RunInterrupt`'s closing brace (which is line 532 before Task 1; Task 1 adds ~14 lines above it, so locate it by the `}` that closes `impl RunInterrupt`, just before the `/// Apply the runner-boundary precedence rule` doc on `effective_interrupt`)
- Modify: `crates/paigasus-helikon-core/src/runner.rs` → `mod interrupt_tests`

**Interfaces:**
- Consumes: `RunInterrupt::run_error(self) -> RunError` (already present); `every_interrupt()` from Task 1.
- Produces: `impl From<RunInterrupt> for RunError`, enabling `let e: RunError = interrupt.into();`.

- [ ] **Step 1: Write the failing test**

Append inside `mod interrupt_tests`, after `terminal_event_is_terminal`:

```rust
    /// `From` must delegate to `run_error`, not re-derive the mapping.
    ///
    /// `RunError` is not `PartialEq` (its `Agent`/`Other` payloads are not), and
    /// `Display` would compare rendered text rather than the mapping — so compare
    /// discriminants, which is exactly what the delegation is responsible for.
    #[test]
    fn from_delegates_to_run_error() {
        for i in every_interrupt() {
            assert_eq!(
                std::mem::discriminant(&RunError::from(i)),
                std::mem::discriminant(&i.run_error()),
                "{i:?}: From must agree with run_error"
            );
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-core interrupt_tests::from_delegates_to_run_error`
Expected: **compile error**, `the trait 'From<RunInterrupt>' is not implemented for 'RunError'`.

- [ ] **Step 3: Write the impl**

Insert directly after the closing brace of `impl RunInterrupt` in `crates/paigasus-helikon-core/src/runner.rs`:

```rust
/// Delegates to [`RunInterrupt::run_error`], which stays the canonical,
/// documented name for this mapping.
///
/// The `.into()` form is for runner implementations reporting an interrupt that
/// won at the control boundary:
///
/// ```
/// use paigasus_helikon_core::{effective_interrupt, RunError, RunInterrupt};
///
/// fn report(interrupt: Option<RunInterrupt>, saw_terminal: bool) -> Result<(), RunError> {
///     if let Some(i) = effective_interrupt(interrupt, saw_terminal) {
///         return Err(i.into());
///     }
///     Ok(())
/// }
///
/// // The interrupt aborted the run in-flight: it wins and converts.
/// assert!(matches!(
///     report(Some(RunInterrupt::TimedOut), false),
///     Err(RunError::Timeout)
/// ));
/// // A genuine terminal already occurred: the interrupt loses.
/// assert!(report(Some(RunInterrupt::TimedOut), true).is_ok());
/// ```
impl From<RunInterrupt> for RunError {
    fn from(interrupt: RunInterrupt) -> Self {
        interrupt.run_error()
    }
}
```

The doc and doctest go on the **impl block**, not on `fn from`: rustdoc collapses `From` impls under "Trait Implementations", and an example one click deeper is an example nobody reads. The doctest is this impl's only consumer — there is no in-repo `.into()` call site — so it must exercise the real shape, which is why it runs through `effective_interrupt` rather than converting in isolation.

- [ ] **Step 4: Run the test and the doctest to verify they pass**

Run:
```bash
cargo test -p paigasus-helikon-core interrupt_tests
cargo test -p paigasus-helikon-core --doc
```
Expected: both PASS. The second command is not optional — a doctest never runs under `cargo build` or `cargo doc`, so this is the only proof the example compiles and holds.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/runner.rs
git commit -m "feat(core): SMA-515 add From<RunInterrupt> for RunError"
```

---

### Task 3: Migrate the `runtime-tokio` call site

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/src/lib.rs:229`

**Interfaces:**
- Consumes: `RunInterrupt::terminal_event()` from Task 1. `RunInterrupt` is already imported in this file (it is used at line 169), so no `use` change is needed. Check whether `AgentEvent` is still referenced elsewhere in the file after the edit — it is (line 166, 209) — so do **not** remove it from the imports.

**No new tests.** This is an evidenced call, not a skip: `crates/paigasus-helikon-runtime-tokio/tests/run_streamed.rs:140` and `:175` already assert `error == "run cancelled"` / `"run timed out"` on `events.last()`, and `:290` (`terminal_then_late_cancel_no_synthetic_terminal`) covers the precedence branch. Those tests are the regression net for this substitution.

- [ ] **Step 1: Run the existing tests to establish green**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_streamed`
Expected: PASS. Record the test count — it must be identical after the edit.

- [ ] **Step 2: Make the substitution**

In `crates/paigasus-helikon-runtime-tokio/src/lib.rs`, inside `run_streamed`'s `async_stream::stream!` block, replace line 229:

```rust
                yield AgentEvent::RunFailed { error: i.terminal_message().to_owned() };
```

with:

```rust
                yield i.terminal_event();
```

Leave the surrounding comment block (lines 218-226) unchanged — it explains the `effective_interrupt` gate, which is untouched.

- [ ] **Step 3: Run the tests to verify they still pass**

Run: `cargo test -p paigasus-helikon-runtime-tokio`
Expected: PASS, with the same test count as Step 1. Any change in the asserted terminal text means the substitution was not pure and must be investigated, not accommodated.

- [ ] **Step 4: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-runtime-tokio/src/lib.rs
git commit -m "refactor(runtime-tokio): SMA-515 build the interrupt terminal via terminal_event"
```

---

### Task 4: Refactor `runtime-temporal` — render as an event, extract the guard

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/runner.rs:258-286` (`run_streamed` body)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/runner.rs:289-309` (`synthetic_terminal_message` → `synthetic_terminal_event`)
- Create (in the same file, after the renamed helper): `fn append_synthetic_terminal`
- Modify: `crates/paigasus-helikon-runtime-temporal/src/runner.rs` → `mod tests` (port 4 tests, add 3)

**Interfaces:**
- Consumes: `RunInterrupt::terminal_event()` from Task 1. `RunInterrupt` and `AgentEvent` are both already imported at `runner.rs:29-31`.
- Produces: `fn synthetic_terminal_event(result: &Result<RunResult, RunError>) -> Option<AgentEvent>` and `fn append_synthetic_terminal(events: &mut Vec<AgentEvent>, event: Option<AgentEvent>)`, both crate-private to this module.

**Why the guard gets its own function.** `runner.rs:276-280` is not one statement — it is a `if let Some(message)` wrapping a `if !events.last().is_some_and(AgentEvent::is_terminal)`. That inner guard is SMA-422's fix for this runner (the comment at lines 270-275 exists only to explain it) and dropping it re-opens SMA-421: a run whose durable log already ends in `RunCompleted` but whose status mapped to `Err` would emit a second terminal. Nothing in the repo catches that today — the four helper tests never touch it, and `driver.rs:756`'s `terminal_wins_over_late_interrupt` tests the *driver*'s `Phase::Done` short-circuit, a different mechanism. Extracting it makes it directly testable, which Steps 1-2 then do.

- [ ] **Step 1: Write the failing guard tests**

In `crates/paigasus-helikon-runtime-temporal/src/runner.rs`, first extend the test module's import at line ~353:

```rust
    use paigasus_helikon_core::{AgentError, TokenUsage};
```

Then append these three tests inside `mod tests`:

```rust
    /// The SMA-421 regression guard: a durable log that already ends in a
    /// terminal must not gain a second one, even when the run's status mapped
    /// to `Err`. This test fails if the guard is dropped.
    #[test]
    fn guard_suppresses_a_second_terminal() {
        let mut events = vec![AgentEvent::RunCompleted {
            usage: TokenUsage::default(),
        }];
        append_synthetic_terminal(&mut events, Some(RunInterrupt::Cancelled.terminal_event()));
        assert_eq!(
            events.len(),
            1,
            "a log already ending in a terminal must not gain a second: {events:?}"
        );
        assert!(
            matches!(events[0], AgentEvent::RunCompleted { .. }),
            "the original terminal must survive untouched: {events:?}"
        );
    }

    #[test]
    fn guard_appends_when_the_log_has_no_terminal() {
        let mut events = vec![AgentEvent::TurnStarted { turn: 0 }];
        append_synthetic_terminal(&mut events, Some(RunInterrupt::TimedOut.terminal_event()));
        assert_eq!(
            events.len(),
            2,
            "a log with no terminal must receive the synthetic one: {events:?}"
        );
        assert_eq!(
            terminal_text(events.pop()).as_deref(),
            Some(RunInterrupt::TimedOut.terminal_message())
        );
    }

    #[test]
    fn no_event_appends_nothing() {
        let mut events = vec![AgentEvent::TurnStarted { turn: 0 }];
        append_synthetic_terminal(&mut events, None);
        assert_eq!(events.len(), 1, "None must be a no-op: {events:?}");

        let mut empty: Vec<AgentEvent> = Vec::new();
        append_synthetic_terminal(&mut empty, None);
        assert!(empty.is_empty(), "None must be a no-op on an empty log");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-temporal --lib runner::tests`
Expected: **compile error**, `cannot find function 'append_synthetic_terminal'` (and `terminal_text`, added in Step 4).

- [ ] **Step 3: Rewrite the rendering helper and add the append helper**

Replace `crates/paigasus-helikon-runtime-temporal/src/runner.rs:289-309` — the whole `synthetic_terminal_message` doc comment and function — with:

```rust
/// The terminal frame [`Runner::run_streamed`] synthesizes when a run ends
/// without one of its own, or `None` when the run succeeded.
///
/// Cancellation and timeout render through [`RunInterrupt::terminal_event`],
/// **not** through [`RunError`]'s `Display`. The two disagree — `RunError::Cancelled`
/// displays as `"cancelled"` while the canonical synthesized frame says
/// `"run cancelled"` — so going through `Display` here would make this runner emit
/// different text than `TokioRunner` for the same event. One rendering, one place
/// (SMA-422, SMA-515).
fn synthetic_terminal_event(result: &Result<RunResult, RunError>) -> Option<AgentEvent> {
    match result {
        Ok(_) => None,
        Err(RunError::Agent(err)) => Some(AgentEvent::RunFailed {
            error: err.to_string(),
        }),
        Err(RunError::Cancelled) => Some(RunInterrupt::Cancelled.terminal_event()),
        Err(RunError::Timeout) => Some(RunInterrupt::TimedOut.terminal_event()),
        // `RunError` is `#[non_exhaustive]` and foreign, so this arm is required.
        // Infrastructure failures have no canonical interrupt text; their own
        // message is the most informative thing available. Not a `RunInterrupt`,
        // so the frame is built here rather than in core.
        Err(other) => Some(AgentEvent::RunFailed {
            error: other.to_string(),
        }),
    }
}

/// Append `event` as the run's terminal frame, unless the durable event log
/// already ends in one.
///
/// The guard is load-bearing (SMA-422, closing SMA-421): a status that mapped to
/// `Err` while the event log ended in `RunCompleted` must not append a second
/// terminal. It tests for *any* terminal via [`AgentEvent::is_terminal`], not
/// just `RunFailed`.
fn append_synthetic_terminal(events: &mut Vec<AgentEvent>, event: Option<AgentEvent>) {
    if let Some(event) = event {
        if !events.last().is_some_and(AgentEvent::is_terminal) {
            events.push(event);
        }
    }
}
```

The nested `if` is copied verbatim from the original and passes clippy in that form today. If clippy nonetheless flags `collapsible_if` after the move, collapse it to a let-chain (`if let Some(event) = event && !events.last()…`) — the semantics are identical — rather than suppressing the lint.

- [ ] **Step 4: Add the test helper and port the four existing tests**

Add this helper inside `mod tests`, above the ported tests:

```rust
    /// Unwrap a synthesized terminal to its message, asserting the variant on
    /// the way through — something the old `-> Option<String>` signature made
    /// impossible.
    fn terminal_text(ev: Option<AgentEvent>) -> Option<String> {
        match ev {
            Some(AgentEvent::RunFailed { error }) => Some(error),
            Some(other) => panic!("synthesized terminal must be RunFailed, got {other:?}"),
            None => None,
        }
    }
```

Then replace the four existing tests at `runner.rs:365-412` with these. Only the call expressions change; every assertion is preserved, including the `assert_ne!` that is the entire reason the helper exists:

```rust
    /// A cancelled run's synthesized terminal must carry the *canonical*
    /// interrupt text, so this runner and `TokioRunner` are indistinguishable
    /// to a stream consumer. Routing through `RunError`'s `Display` instead
    /// would silently emit "cancelled" here and "run cancelled" there.
    #[test]
    fn cancelled_renders_the_canonical_interrupt_message() {
        let message = terminal_text(synthetic_terminal_event(&Err(RunError::Cancelled)));
        assert_eq!(
            message.as_deref(),
            Some(RunInterrupt::Cancelled.terminal_message())
        );
        assert_eq!(message.as_deref(), Some("run cancelled"));
        assert_ne!(
            message,
            Some(RunError::Cancelled.to_string()),
            "must not fall through to RunError's Display, which says \"cancelled\""
        );
    }

    #[test]
    fn timed_out_renders_the_canonical_interrupt_message() {
        assert_eq!(
            terminal_text(synthetic_terminal_event(&Err(RunError::Timeout))).as_deref(),
            Some(RunInterrupt::TimedOut.terminal_message())
        );
    }

    #[test]
    fn success_synthesizes_no_terminal_event() {
        assert_eq!(
            terminal_text(synthetic_terminal_event(&Ok(RunResult::default()))),
            None,
            "a completed run already carries its own terminal"
        );
    }

    /// An agent failure keeps its own structured message rather than being
    /// flattened into an interrupt's canonical text.
    #[test]
    fn agent_failure_keeps_its_own_message() {
        let message = terminal_text(synthetic_terminal_event(&Err(RunError::Agent(
            AgentError::MaxTurnsExceeded(3),
        ))))
        .expect("an agent failure always has a message");
        assert!(
            message.contains('3'),
            "expected the AgentError's own text, got {message:?}"
        );
    }
```

`success_synthesizes_no_terminal_message` is renamed to `…_no_terminal_event` because the helper no longer returns a message. The other three names stay — they are about the message text, which is still what they assert.

- [ ] **Step 5: Update the `run_streamed` body and retarget the stranded identifiers**

Replace `crates/paigasus-helikon-runtime-temporal/src/runner.rs:260-280` with:

```rust
        let failure = FailureSlot::new();
        let terminal = synthetic_terminal_event(&result);
        // Move the structured error into the slot only for an agent failure;
        // the event itself was already rendered above.
        if let Err(RunError::Agent(err)) = result {
            failure.set(err);
        }

        // `collect()` only reads the failure slot once it observes a terminal
        // `RunFailed` in the stream. The durable event log already carries one
        // for `AgentFailed` runs; synthesize one for the terminal states that do
        // not (cancellation/timeout/infra), so a failed run never collects as
        // `Ok`. `append_synthetic_terminal` owns the guard that stops this from
        // appending a *second* terminal (SMA-422).
        append_synthetic_terminal(&mut events, terminal);
```

That retargets all three stranded `message`-flavoured spots: the binding at line 261, the comment at 262-263, and the comment at 270-275. Leave the `let (mut events, result) = …` line above and the `Ok(RunResultStreaming::with_failure(…))` below untouched.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-runtime-temporal --lib`
Expected: PASS — the 4 ported tests plus the 3 new guard tests, alongside the rest of the lib's unit tests. The live-Temporal integration tests are env-gated and will loud-skip; that is expected and is not a failure.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-runtime-temporal/src/runner.rs
git commit -m "refactor(runtime-temporal): SMA-515 render the synthetic terminal as an event"
```

---

## Final verification

Run the complete CI gate set from the worktree root before opening the PR. Do **not** substitute per-crate runs for the workspace test command — a per-crate run resolves a different feature graph and has produced false greens in this workspace before.

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-features --all-targets -- -D warnings`
- [ ] `cargo test --workspace --all-features`
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
- [ ] `DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh`
- [ ] `mdbook build docs/book`
- [ ] `git log --oneline origin/main..HEAD` — confirm 6 commits (spec, plan, and one per implementation task), no version bumps, no `chore(release)`.
- [ ] `git diff origin/main --stat` — confirm exactly 6 files changed: the spec, the plan, `crates/paigasus-helikon-core/src/runner.rs`, `crates/paigasus-helikon-runtime-tokio/src/lib.rs`, `crates/paigasus-helikon-runtime-temporal/src/runner.rs`, and `docs/book/src/concepts/core-primitives.md`. No `Cargo.toml` and no `CHANGELOG.md` may appear.

**Known local-environment caveat:** on macOS, a `NATIVE_ROOTS`-flavoured failure burst in the `bedrock` provider tests tracks the checkout path, not the code. This plan runs inside a worktree; if that burst appears, confirm it also reproduces on a clean `origin/main` checkout before treating it as a regression from this branch.

**PR title** (governs release-plz attribution; satisfies both `pr-title.yml` rules):

```
feat(core): SMA-515 add terminal_event and From<RunInterrupt> for RunError
```
