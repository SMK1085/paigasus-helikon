# SMA-515 — `RunInterrupt` ergonomics: `terminal_event()` and `From<RunInterrupt> for RunError`

**Status:** design (revised after adversarial challenge)
**Date:** 2026-08-16
**Linear:** [SMA-515](https://linear.app/smaschek/issue/SMA-515/runinterrupt-ergonomics-add-terminal-event-and-fromruninterrupt-for)
**Branch:** `feature/sma-515-runinterrupt-ergonomics-add-terminal_event-and`
**Follows:** SMA-422 (PR #193) — hoisted the terminal-vs-cancel precedence rule into core.

## Problem

SMA-422 moved the runner-boundary precedence rule into `paigasus-helikon-core`:
`AgentEvent::is_terminal()`, `RunInterrupt { Cancelled, TimedOut }` with `run_error()`
and `terminal_message()`, and `effective_interrupt(interrupt, saw_terminal)`. Two
additive gaps were noted in that PR's final review and deferred.

1. **Runners still assemble the synthesized terminal frame by hand.** `terminal_message()`
   returns text, so every streaming runner writes the wrapper itself:

   ```rust
   AgentEvent::RunFailed { error: i.terminal_message().to_owned() }
   ```

   That exact line is `crates/paigasus-helikon-runtime-tokio/src/lib.rs:229`. In
   `crates/paigasus-helikon-runtime-temporal/src/runner.rs` the same job is split in two:
   `terminal_message().to_owned()` in the `Cancelled` and `Timeout` arms of
   `synthetic_terminal_message` (line 298), and the `AgentEvent::RunFailed { … }` wrapper
   in the `run_streamed` body (line 278). Core owns the *text* but not the *frame*, so the
   last hand-rolled piece still lives in each runner.

2. **The canonical interrupt → error mapping is inherent-only.** `run_error()` exists and is
   the documented name, but there is no `From`, so `.into()` at a consumer's call site does
   not reach it. The realistic shape is a third-party `Runner` implementation writing
   `return Err(i.into());` after `effective_interrupt` hands it an interrupt that won.

Neither is a correctness gap. The rule itself is already single-homed and tested. This is
ergonomics, and the ticket is Low priority.

> **Note on the `?` framing.** The ticket motivates `From` with "`?`/`.into()` call sites".
> `?` is not actually reachable today: nothing in the workspace produces a
> `Result<_, RunInterrupt>` — `effective_interrupt` returns `Option<RunInterrupt>`
> (`runner.rs:562`), and interrupts otherwise originate from a `select!` arm or an
> `InterruptHandle`. `.into()` is the real consumer, and the doctest reflects that rather
> than the ticket's wording.

## Goals

- Add `RunInterrupt::terminal_event(self) -> AgentEvent` to core.
- Add `impl From<RunInterrupt> for RunError`, delegating to `run_error()`.
- Migrate every in-repo site that hand-rolls the **interrupt** terminal frame, so neither
  helper ships without a consumer.
- Keep the public rendering behaviour byte-identical. This PR changes no message text and
  no event a consumer observes.

## Non-goals

- **Non-interrupt `AgentEvent::RunFailed` construction stays as it is.** Hand-rolled
  `RunFailed` frames exist across the workspace for infrastructure and protocol errors —
  `runtime-axum/src/registry.rs:85`, `runtime-actix/src/registry.rs:84`,
  `runtime-agentcore/src/ws.rs` (×4), `.../invoke.rs:326`, `.../a2a/rpc.rs:568`,
  `.../agui/ws.rs:298`, `.../agui/sse.rs:173`, `runtime-temporal/src/driver.rs:313`. None of
  those is an interrupt frame; none has a `RunInterrupt` to render from. They are out of
  scope, and the goal above is deliberately scoped to "interrupt terminal frame" so this
  does not read as a licence to sweep six crates.
- **SMA-516** — a runner-level test for `runtime-temporal`'s `run_streamed`. Related,
  separately tracked, still out of scope. This PR does, however, cover the double-terminal
  guard at unit level (see *Testing*), so that guard is not left waiting on SMA-516.
- Changing `effective_interrupt`, `AgentEvent::is_terminal`, or any message literal.
- Migrating existing `run_error()` call sites to `From`. See *Design §3*.

## Design

### 1. `RunInterrupt::terminal_event`

```rust
impl RunInterrupt {
    /// The terminal [`crate::AgentEvent`] a streaming runner yields when this
    /// interrupt wins — see [`effective_interrupt`].
    ///
    /// Renders through [`RunInterrupt::terminal_message`] so a runner never
    /// assembles the frame itself and every runner emits identical bytes for the
    /// same interrupt.
    #[must_use]
    pub fn terminal_event(self) -> AgentEvent {
        AgentEvent::RunFailed {
            error: self.terminal_message().to_owned(),
        }
    }
}
```

Takes `self` by value, matching `run_error()` and `terminal_message()` (`RunInterrupt` is
`Copy`). `#[must_use]`, matching its siblings.

**It delegates rather than matching.** There is deliberately no second `match self` here.
`terminal_message()` already has a wildcard-free match, so a future `RunInterrupt` variant
breaks compilation in exactly one place instead of two. That mirrors the reasoning already
written into `run_error()`'s body comment (`runner.rs:514-516`).

`terminal_message()`'s own rustdoc gains one sentence pointing at `terminal_event()` as the
thing to call when you need the whole frame rather than just the text. Both stay public:
`terminal_message()` is not deprecated — the asymmetry test and the book both refer to the
text in its own right — but a runner author should reach for the frame.

### 2. `impl From<RunInterrupt> for RunError`

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
/// assert!(matches!(
///     report(Some(RunInterrupt::TimedOut), false),
///     Err(RunError::Timeout)
/// ));
/// assert!(report(Some(RunInterrupt::TimedOut), true).is_ok());
/// ```
impl From<RunInterrupt> for RunError {
    fn from(interrupt: RunInterrupt) -> Self {
        interrupt.run_error()
    }
}
```

Delegating, not duplicating the match — same single-point-of-truth reason as above, and
what the ticket asks for explicitly.

The doc and doctest sit on the **impl block**, not on `fn from`: rustdoc renders `From`
impls under a collapsed "Trait Implementations" heading, and an example one further click
down is an example nobody reads.

The doctest is load-bearing, not decoration. This impl has **no in-repo consumer** (see
§3), and its whole purpose is the out-of-crate call site — so the doctest *is* that call
site, in the shape a third-party `Runner` would write it, compiled and run by the `test`
gate. It also exercises the precedence interaction, which is the only context in which the
conversion is ever reached.

### 3. Call-site migration

| Site | Change |
|---|---|
| `runtime-tokio/src/lib.rs:229` (`run_streamed`) | `yield AgentEvent::RunFailed { error: i.terminal_message().to_owned() };` → `yield i.terminal_event();` |
| `runtime-temporal/src/runner.rs:298` | `synthetic_terminal_message(&Result<…>) -> Option<String>` → `synthetic_terminal_event(&Result<…>) -> Option<AgentEvent>` |
| `runtime-temporal/src/runner.rs:276-280` (`run_streamed` body) | the guarded push moves into a new `append_synthetic_terminal` helper — **guard preserved verbatim** |

The temporal rendering helper becomes:

```rust
fn synthetic_terminal_event(result: &Result<RunResult, RunError>) -> Option<AgentEvent> {
    match result {
        Ok(_) => None,
        Err(RunError::Agent(err)) => Some(AgentEvent::RunFailed { error: err.to_string() }),
        Err(RunError::Cancelled) => Some(RunInterrupt::Cancelled.terminal_event()),
        Err(RunError::Timeout) => Some(RunInterrupt::TimedOut.terminal_event()),
        // `RunError` is `#[non_exhaustive]` and foreign, so this arm is required.
        Err(other) => Some(AgentEvent::RunFailed { error: other.to_string() }),
    }
}
```

Two of the five arms now route through core. The other two `RunFailed` constructions stay
hand-rolled **by necessity** — an `AgentError` and an infrastructure `RunError` are not
`RunInterrupt`s and have no canonical interrupt text, which is exactly what the existing
doc comment explains. What the refactor buys is that the *runner body* no longer builds a
frame; all synthesis is confined to this one helper.

#### The double-terminal guard — must survive intact

`runner.rs:276-280` is **not** a bare push. It is two nested statements, and the inner one
is SMA-422's fix for this runner (the six-line comment at `runner.rs:270-275` exists only to
explain it):

```rust
// BEFORE
if let Some(message) = terminal_message {
    if !events.last().is_some_and(AgentEvent::is_terminal) {
        events.push(AgentEvent::RunFailed { error: message });
    }
}
```

Dropping that inner guard would re-open SMA-421 for `TemporalRunner`: a run whose durable
event log already ends in `RunCompleted` but whose status mapped to `Err` would emit a
second terminal. Nothing in the repo currently catches that — the four helper tests at
`runner.rs:365-412` never touch the guard, and `driver.rs:756`'s
`terminal_wins_over_late_interrupt` tests the *driver*'s `Phase::Done` short-circuit, a
different mechanism.

The guard therefore moves into a named, unit-testable helper rather than being retyped
inline:

```rust
// AFTER — in the runner body
append_synthetic_terminal(&mut events, synthetic_terminal_event(&result));

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

This is the one place the PR *improves* on a straight port: the guard goes from untested to
directly unit-tested (see *Testing*), without waiting on SMA-516.

**Identifier and comment retargeting.** The rename strands several `message`-flavoured
names in `run_streamed`: the binding at `runner.rs:261`, the comment at `runner.rs:262-263`
("the message itself was already taken above"), and the comment at `runner.rs:270-275`
("synthesize one for the terminal states that do not"). All of them retarget to the event
vocabulary; the helper's own doc comment (which explains the deliberate
`terminal_message`-vs-`Display` asymmetry) stays, retargeted to the new name and return
type.

**Existing `run_error()` call sites are not migrated.** `runtime-tokio/src/lib.rs:169` keeps
`.map(RunInterrupt::run_error)`: the explicit method name reads better as a function
reference than `.map(RunError::from)`, and the ticket states `run_error()` remains the
documented name. `From` is for consumers' `.into()`, not a replacement at sites that already
name the mapping.

### 4. Behavioural equivalence

`terminal_event()` produces exactly the frame the migrated sites produced before, so no
consumer-visible bytes change. The temporal refactor is likewise pure restructuring, and
"pure" here means all four of: the same `Option` inhabitance per arm, the same message text
per arm, the same `AgentEvent` variant (`RunFailed`), and **the same double-terminal guard
semantics**. Anything else that changes is a bug in this PR, and the tests below are written
to catch that rather than to re-describe the new shape.

## Testing

### `crates/paigasus-helikon-core/src/runner.rs` → existing `interrupt_tests` module

A shared fixture plus four tests, mirroring the house idiom already in
`agent.rs`'s `terminal_tests` (`every_variant()` + a distinct-discriminant count test +
wildcard-free `classify`):

```rust
/// Every [`RunInterrupt`] variant. The `match` is deliberately exhaustive with
/// no wildcard arm, so adding a variant fails to compile *here* — one place —
/// until someone extends the fixture.
fn every_interrupt() -> Vec<RunInterrupt> {
    fn _exhaustive(i: RunInterrupt) {
        match i {
            RunInterrupt::Cancelled | RunInterrupt::TimedOut => {}
        }
    }
    vec![RunInterrupt::Cancelled, RunInterrupt::TimedOut]
}
```

1. `every_interrupt_covers_the_whole_enum` — assert the fixture holds 2 *distinct*
   discriminants (not `len() == 2`, which 2 copies of one variant would also satisfy).
2. `terminal_event_agrees_with_terminal_message` — for each variant, destructure and
   compare against the accessor:

   ```rust
   let AgentEvent::RunFailed { error } = i.terminal_event() else {
       panic!("terminal_event must be RunFailed, got {:?}", i.terminal_event())
   };
   assert_eq!(error, i.terminal_message());
   ```

   Destructuring is **required**, not stylistic: `AgentEvent` derives no `PartialEq`
   (`agent.rs:353`, no manual impl anywhere in core), so `assert_eq!` on whole events does
   not compile. Do **not** work around that by deriving `PartialEq` on `AgentEvent` — that is
   a gratuitous public-API change to a `#[non_exhaustive]` enum whose payloads
   (`Item`, `ContentPart`, `GuardrailKind`) would all be dragged along — nor by comparing
   `format!("{ev:?}")` strings.
3. `terminal_event_is_terminal` — assert `terminal_event().is_terminal()` for each variant.
   This ties the two core primitives together: a synthesized frame that `effective_interrupt`
   gates on must be classified terminal by the very predicate runners use to compute
   `saw_terminal`. Test 2 pins the variant, so this is an independent assertion, not a
   restatement.
4. `from_delegates_to_run_error` — for each variant, assert
   `std::mem::discriminant(&RunError::from(i)) == std::mem::discriminant(&i.run_error())`.
   `RunError` is not `PartialEq` either (its `Agent`/`Other` payloads are not), and `Display`
   would compare rendered text rather than the mapping; `discriminant` compares exactly what
   the delegation is responsible for.

All assertions are **derived from the existing accessors**, never restated literals — the
literals are already pinned by `terminal_message_rendering` (`runner.rs:734`), and restating
them here would duplicate a fixture rather than test anything.

*Honest scope of the exhaustiveness guarantee:* the wildcard-free match forces an **explicit
decision** in one place when a variant is added. It cannot force the author to extend the
returned `vec!` — no Rust construct short of a macro can. That is the same guarantee
`agent.rs`'s idiom provides, and the count test in item 1 is the second nudge.

### `crates/paigasus-helikon-runtime-temporal/src/runner.rs` → existing test module

**Ported (4).** The `synthetic_terminal_message` tests move to `synthetic_terminal_event`,
via a local helper that also asserts the event *type* — something the old `String` signature
made impossible, so this is a coverage gain rather than a like-for-like port:

```rust
fn terminal_text(ev: Option<AgentEvent>) -> Option<String> {
    match ev {
        Some(AgentEvent::RunFailed { error }) => Some(error),
        Some(other) => panic!("synthesized terminal must be RunFailed, got {other:?}"),
        None => None,
    }
}
```

`cancelled_renders_the_canonical_interrupt_message` keeps all three of its assertions,
including the `assert_ne!` against `RunError::Cancelled.to_string()` — that guard is the
reason the helper exists and must survive the refactor intact.

**New (3), covering `append_synthetic_terminal` — the guard the BLOCKER was about:**

5. `guard_suppresses_a_second_terminal` — an `events` vec ending in
   `AgentEvent::RunCompleted` plus `Some(RunFailed)` leaves the vec unchanged. This is the
   SMA-421 regression test; it fails if the guard is dropped.
6. `guard_appends_when_the_log_has_no_terminal` — an `events` vec ending in a non-terminal
   variant gets the event appended.
7. `no_event_appends_nothing` — `None` is a no-op on both a terminal-ending and an
   empty vec.

### `crates/paigasus-helikon-runtime-tokio`

No new tests, and this is an evidenced call rather than a skip: `tests/run_streamed.rs:140`
and `:175` already assert `error == "run cancelled"` / `"run timed out"` on `events.last()`,
and `:290` (`terminal_then_late_cancel_no_synthetic_terminal`) covers the precedence branch.
The migrated line is a pure substitution — those tests pass unchanged, or it was not pure.

## Documentation

- **`crates/paigasus-helikon-core/src/runner.rs:122-123` — the normative contract.**
  `Runner::run_streamed`'s rustdoc currently instructs implementors: *"Gate the synthetic
  terminal on [`effective_interrupt`], and render it with [`RunInterrupt::terminal_message`]."*
  That paragraph is what third-party runner authors read on docs.rs — the audience
  `effective_interrupt`'s own doc names as its main one (`runner.rs:545-547`). Retarget it to
  `terminal_event`. Leaving it stale would have the canonical instruction still telling
  runner authors to hand-build the frame this PR exists to remove, and **no CI gate catches
  it** (`missing_docs` and `-D warnings` check presence and link validity, not staleness).
  `Runner::run`'s doc at `runner.rs:102-103` needs no change — it names `effective_interrupt`
  and `RunInterrupt` but not the rendering.
- **`docs/book/src/concepts/core-primitives.md:17`** names `run_error()` and
  `terminal_message()` in prose; extend it to name `terminal_event()` as the way a streaming
  runner produces the synthesized frame. Required by CLAUDE.md's same-PR book rule — this is
  a public-API addition.
- **No crate README change.** Checked, conscious call: `crates/paigasus-helikon-core/README.md`
  describes the crate at trait granularity and never enumerates `RunInterrupt`'s surface, so
  there is nothing there to drift. Facade and root READMEs are untouched — the crate roster
  and feature map do not change.

## Release mechanics

Purely additive on `paigasus-helikon-core`; `runtime-tokio` and `runtime-temporal` change
internals only. All three are already-released crates, so **no manual version bump and no
`chore(release)` commit belong in this PR.** release-plz assigns patch bumps from the merged
commit's touched paths and cascades the facade automatically; a manual bump would defeat
that cascade.

**Why CLAUDE.md's same-PR-core-bump caveat does not apply here.** That caveat warns that a
crate consuming core API added in the *same* PR deadlocks `cargo publish --verify` against
the stale registry core — and `runtime-tokio`/`runtime-temporal` do consume `terminal_event()`
from this same PR. It does not bite because the caveat is scoped to the **stub-ascend
ritual**, where the ascending crate bumps its own version and therefore publishes on its own
merge, ahead of core. Nothing here publishes on merge: all three crates are already released,
so release-plz bumps them in its own release PR and publishes dependency-ordered, core first.
Direct precedent: SMA-418 / PR #80, an already-released consumer of same-PR core API that
needed no manual bump.

No `cargo-semver-checks` risk: adding an inherent method and a `From` impl to an existing
type is non-breaking, and no `RunInterrupt` variant is added.

## Commit and PR title

Commit scopes `core`, `runtime-tokio`, and `runtime-temporal` are all in `.versionrc`'s
allowlist. The squashed PR title governs release-plz's attribution and must satisfy both
`pr-title.yml` rules (full Conventional Commits prefix; lowercase subject after `SMA-###`):

```
feat(core): SMA-515 add terminal_event and From<RunInterrupt> for RunError
```

## Verification

Full local CI gate set before opening the PR:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
mdbook build docs/book
```

`cargo test --workspace --all-features` is the exact gate, not a per-crate subset — a
per-crate run resolves a different feature graph and has produced false greens before. The
doctest on the `From` impl only runs under the `test` gate, so a green `cargo build` proves
nothing about it.

## Risks

| Risk | Mitigation |
|---|---|
| **Temporal refactor drops the `!events.last().is_some_and(AgentEvent::is_terminal)` guard → silent SMA-421 regression in untested code** | The guard moves into the named `append_synthetic_terminal` helper and gains three direct unit tests, the first of which fails if it is dropped. Previously untested at any level. |
| Temporal refactor changes synthesized text for some arm | The four ported tests assert per-arm text derived from the accessors, and now also assert the event variant; the `Cancelled` test's `assert_ne!` against `Display` is preserved. |
| `terminal_event()` and `terminal_message()` drift later | `terminal_event()` has no match of its own; it calls `terminal_message()`. Drift requires deleting the delegation. |
| A future `RunInterrupt` variant is added without covering the new API | Wildcard-free matches in `terminal_message()`, `run_error()`, and `every_interrupt()` all fail to compile, forcing an explicit decision (see the honesty note in *Testing*). |
| `From` impl ships unexercised | The doctest is its consumer, in the audience's real call-site shape, and runs in the `test` gate. |

## Alternatives considered and rejected

- **Leave `runtime-temporal` untouched; do the `-> Option<AgentEvent>` rename in SMA-516,
  where the runner-level test lands.** A real option, and the challenge round argued for it:
  `terminal_event()` already gets a consumer from tokio, temporal keeps two hand-rolled arms
  regardless, and this is the only half of the diff that can regress anything. **Rejected by
  explicit decision at the design gate** — the ticket names temporal as an "in spirit" site
  and confining all frame synthesis to one helper is the stated point of the change. The
  risk that motivated the objection is neutralized directly: the guard is extracted and
  unit-tested in this PR rather than deferred to SMA-516.
- **`impl From<RunInterrupt> for AgentEvent` for symmetry with the `RunError` conversion.**
  Rejected. `From` implies a total conversion between related types; an interrupt is not an
  event, and `RunFailed { error }` discards the variant, so the conversion is lossy and
  one-way. `let ev: AgentEvent = i.into()` also reads ambiguously at a call site — *which*
  event? The named `terminal_event()` says which. The error mapping earns its `From` because
  `RunError` genuinely is the interrupt's typed form at the runner boundary.
- **`Cow<'static, str>` for `AgentEvent::RunFailed`'s payload** to avoid the one `String`
  allocation `terminal_event()` makes from a `&'static str`. Out of scope and not planned:
  it is a breaking change to a public variant, and the cost is one allocation per run.

## Challenge round

Reviewed by the `spec-challenger` agent on 2026-08-16; verdict **APPROVE WITH CHANGES**.
Folded in: the double-terminal guard blocker (extraction + three tests + risk row), the
`AgentEvent`-is-not-`PartialEq` compile error, the stale `Runner::run_streamed` rustdoc, the
overstated "never assembles the frame" goal, the unsupported `?` motivation, the overstated
exhaustiveness guarantee, the stranded `message` identifiers, the missing release-caveat
rebuttal, doctest placement, and the PR-title pin. Declined: reverting the temporal migration
(decided at the design gate; risk neutralized instead) — recorded above under *Alternatives*.
