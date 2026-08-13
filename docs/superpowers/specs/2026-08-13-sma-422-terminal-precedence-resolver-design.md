# SMA-422 — Hoist the terminal-vs-cancel precedence resolver into core

**Date:** 2026-08-13
**Linear:** [SMA-422](https://linear.app/smaschek/issue/SMA-422/hoist-the-terminal-vs-cancel-precedence-resolver-into-core-for-durable)
**Branch:** `feature/sma-422-hoist-the-terminal-vs-cancel-precedence-resolver-into-core`
**Type:** Pure refactor — no user-visible behaviour change.

## Background

SMA-421 fixed "a late cancel/timeout can override an already-terminal run" inside
`paigasus-helikon-runtime-tokio`: a genuine terminal event (`RunCompleted` /
`RunFailed`) wins, and a cancel/timeout overrides only when it aborted the run
in-flight before any terminal. The fix landed as a file-local `Outcome` enum, a
file-local `is_terminal` helper, and a `match outcome.get()` block in
`runtime-tokio/src/lib.rs`.

That precedence rule is runner-agnostic run semantics, not a Tokio detail. SMA-421
deliberately deferred hoisting it because the durable runners were 1-line stubs at
the time; the rule was documented in the `Runner` trait docs instead, and the hoist
was parked until 2+ real implementations existed to shape the abstraction against.

## What the tree actually looks like now

The ticket assumed the durable runners would each re-derive tokio's `match`. They do
not. The same rule is expressed across **five crates and nine sites, in three
different shapes** — a scanning gate, a structural gate, and four synthesis gates:

| Site | Shape of the rule |
|---|---|
| `runtime-tokio/src/lib.rs:41` | `is_terminal` — original definition |
| `runtime-tokio/src/lib.rs:182–191` | Scans collected events for a terminal, gates `Outcome::Cancelled/TimedOut` on `!saw_terminal` |
| `runtime-tokio/src/lib.rs:243–255` | Synthesis gate: only synthesize a terminal when `!saw_terminal` |
| `runtime-temporal/src/driver.rs:340` | **Structural**: `interrupt()` returns the cached `Phase::Done` outcome if there is one — terminal wins with no event scan |
| `runtime-temporal/src/runner.rs:276–280` | Synthesis gate, as "don't push `RunFailed` if `events.last()` already is one" |
| `runtime-temporal/src/error.rs:57–58` | Fourth copy of the `interrupt → RunError` rendering (`Cancelled → RunError::Cancelled`, `TimedOut → RunError::Timeout`) |
| `runtime-axum/src/event_log.rs:20` | Byte-identical copy of tokio's `is_terminal` |
| `runtime-axum/src/registry.rs:62` | Synthesis gate, as `synthetic_terminal_frame(saw_terminal)` with CWE-209 public strings |
| `runtime-actix/src/event_log.rs:20` | **Third** byte-identical copy of `is_terminal` |
| `runtime-actix/src/registry.rs:61–88` | Synthesis gate, mirroring axum's |
| `runtime-agentcore` | Holds no copy. Delegates to the wrapped runner; documents the contract in prose (`invoke.rs:216`) |

Three consequences for the design:

1. `is_terminal` is genuine copy-paste duplication in **three** crates (tokio, axum,
   actix) and should collapse to one definition. `runtime-actix` is a published
   `0.2.0` crate with its own facade feature (`crates/paigasus-helikon/Cargo.toml:47`)
   and is held to **byte-identical wire parity** with axum by
   `tests/runtime-http-conformance/tests/parity.rs`. Migrating one and not the other
   would create exactly the silent-drift hazard this ticket exists to remove.
2. The `interrupt → RunError` rendering already exists twice (tokio's `match`,
   temporal's `error.rs:57–58`). Introducing a canonical `run_error()` while leaving a
   copy two files away in a crate this PR already edits would be self-defeating.
3. A "controlled stream → result" combinator — the ticket's second suggested shape —
   fits **tokio and nothing else**. Temporal's interrupt arrives from a durable
   Temporal timer inside a workflow, not a `tokio::select!`; axum and actix have no
   cancel boundary of their own. Such a combinator would have exactly one consumer.

## Approach

**Hoist the *decision*, not the control flow.** Core owns a small, pure, well-tested
vocabulary for the rule. Each runner keeps the control-flow shape that suits its
execution model and calls the same decision. This fits all five crates and forces
nothing.

Two alternatives were considered and rejected:

- **Decision + shared synthesis helpers.** The synthesis gates have genuinely
  different policy at each site — tokio keys on the interrupt, temporal on the failure
  result, axum/actix on stream-ended-without-terminal with their own CWE-209-safe
  public strings. A shared helper would have to be parameterised until it stopped
  carrying meaning.
- **Full controlled-stream combinator.** One consumer, as above.

## Core surface

### `AgentEvent::is_terminal` — `crates/paigasus-helikon-core/src/agent.rs`

Placed with the type it interrogates.

```rust
impl AgentEvent {
    /// `true` for the two events that end a run: `RunCompleted` / `RunFailed`.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::RunCompleted { .. } | Self::RunFailed { .. })
    }
}
```

`matches!` is used so that a newly added variant defaults to *non*-terminal; the
exhaustiveness-guard test (below) is what makes that default loud rather than silent.

### `RunInterrupt` and the rule — `crates/paigasus-helikon-core/src/runner.rs`

Placed next to `RunResultStreaming`, as the ticket specifies. `core/src/lib.rs`
carries `pub use runner::*`, so both are re-exported automatically.

```rust
/// Why a runner's control boundary aborted a run before its natural end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunInterrupt {
    /// The run's `CancellationToken` fired.
    Cancelled,
    /// The run exceeded `RunConfig::timeout`.
    TimedOut,
}

impl RunInterrupt {
    /// The `RunError` this interrupt surfaces at the runner boundary.
    #[must_use]
    pub fn run_error(self) -> RunError { .. }

    /// Canonical `error` text for a synthesized terminal `RunFailed` frame.
    #[must_use]
    pub fn terminal_message(self) -> &'static str { .. }
}

/// **The precedence rule, in one place.**
///
/// `interrupt` is the boundary interrupt the runner observed (`None` if none
/// fired); `saw_terminal` is whether the run produced a genuine
/// `RunCompleted` / `RunFailed`. Returns the interrupt **only if it wins** —
/// i.e. it aborted the run in-flight, before any terminal event. A genuine
/// terminal always beats a late cancel/timeout.
#[must_use]
pub fn effective_interrupt(
    interrupt: Option<RunInterrupt>,
    saw_terminal: bool,
) -> Option<RunInterrupt> {
    if saw_terminal { None } else { interrupt }
}
```

Both `run_error` and `terminal_message` use a wildcard-free `match` on `Self`;
`#[non_exhaustive]` has no effect inside the defining crate, so adding a variant
breaks these two arms at compile time in core — which is the desired loudness.

`effective_interrupt` is a free function rather than an associated `RunInterrupt::`
method because it must accept the `None` (no interrupt fired) case uniformly; core
has precedent for free functions in the flat root namespace (`transition`,
`finalize_tool_output`).

### Why `Option<RunInterrupt>` and not `Option<RunError>`

An earlier sketch had the resolver return `Option<RunError>`. That serves
`Runner::run` but not `run_streamed`, which needs the *interrupt* in order to render
a message — so it would require either a second near-duplicate resolver or an
`.is_some()` check followed by re-deriving the interrupt. Returning
`Option<RunInterrupt>` gives **one rule with two renderings** (`run_error()` and
`terminal_message()`) and one function fewer.

### Why `effective_interrupt` is public API despite having one in-workspace caller

Temporal keeps its structural `Phase::Done` short-circuit; axum and actix have no
interrupt boundary; agentcore delegates. So `effective_interrupt` will have exactly
one caller in this workspace — `TokioRunner` — which is the same objection used above
to reject the combinator. The distinction is deliberate and must be stated rather than
assumed:

- The **combinator** would prescribe *control flow* (a `tokio::select!`-shaped async
  boundary) that no other execution model can adopt. It is unusable, not merely
  unused.
- `effective_interrupt` states a **rule** that every `Runner` must obey and that the
  trait docs already mandate in prose (`core/src/runner.rs:94–101`). Its audience is
  third-party `Runner` implementors, for whom "the rule, executable and tested" is the
  deliverable. In-workspace caller count is the wrong metric for a contract.

This justification goes in the doc comment, not just the spec.

### Why `RunInterrupt` **is** `#[non_exhaustive]`

An earlier draft of this spec argued the opposite — that `#[non_exhaustive]` would
force a wildcard arm on every consumer that maps the enum, and that a wildcard in this
code path is the silent fail-open SMA-421 exists to prevent. That argument does not
survive scrutiny and is **reversed**:

- There is exactly **one** match site in the workspace (`runtime-temporal/src/driver.rs:344–347`,
  `RunInterrupt → RunStatusPayload`). `workflow.rs:277–278` only *constructs* the
  enum, and construction is unaffected by `#[non_exhaustive]`.
- That one wildcard need not be silent or panicking. Mapping an unknown interrupt to
  `RunStatusPayload::AgentFailed(ErrorKindPayload::Other { message: "unhandled run
  interrupt: …" })` is total, loud, and replay-safe — an unknown interrupt genuinely
  *is* a failed run, so this is honest degradation rather than a mislabeled success.
- The cost of omitting it is severe and was understated. `paigasus-helikon-core` is a
  published `0.5.x` crate; adding a variant to a non-`#[non_exhaustive]` public enum is
  a breaking change, which release-plz bumps to **`0.6.0`** on a 0.x crate — breaking
  the `^0.5` requirement of every sibling crate *and* every external user, for a change
  that should be a patch.
- Every peer type in `runner.rs` is `#[non_exhaustive]` (`RunConfig:164`,
  `RunResult:226`, `TokenUsage:432`, `RunError:468`). Deviating on the dependency root
  is an irreversible commitment; matching convention is not.

## Call-site migrations

### `paigasus-helikon-runtime-tokio`

- Delete the file-local `Outcome` enum and `is_terminal` helper.
- `controlled` commits `Option<RunInterrupt>` (cell defaults to `None`, replacing
  `Outcome::Completed`). `OutcomeHandle` becomes an `Option<RunInterrupt>` read
  handle, renamed `InterruptHandle`; its binding at the two call sites is renamed
  `outcome` → `interrupt` to match.
- **`controlled`'s doc comment (`src/lib.rs:48–54`) must be rewritten.** It currently
  says "The outcome is committed *before* the terminating `None`"; with an `Option`
  cell defaulting to `None`, nothing is committed on the natural-completion path.
  New wording: "the interrupt, if any, is committed before the terminating `None`".
- `run`:
  ```rust
  match effective_interrupt(interrupt.get(), saw_terminal).map(RunInterrupt::run_error) {
      Some(err) => Err(err),
      None => collected,
  }
  ```
- `run_streamed`:
  ```rust
  if let Some(i) = effective_interrupt(interrupt.get(), saw_terminal) {
      finalize(&session, &recorder).await;
      yield AgentEvent::RunFailed { error: i.terminal_message().to_owned() };
  }
  ```
- The `saw_terminal` derivation, including `unwrap_or(true)` and its
  `Err(collect()) ⇔ a RunFailed was observed` comment, is preserved verbatim.
- **Knowingly preserved:** when `!saw_terminal` *and* no interrupt fired, `finalize`
  is never called (today's `Outcome::Completed => {}` arm). The `if let Some(i)`
  form preserves this exactly. It is the case `PUBLIC_RUN_NO_TERMINAL`
  (`runtime-axum/src/registry.rs:50`) exists to paper over; changing it is out of
  scope for a pure refactor.
- The in-line SMA-421 rationale comments (`src/lib.rs:174–181`, `:237–242`) are
  retained but re-pointed at the core helpers rather than restating the rule.

### `paigasus-helikon-runtime-temporal`

- Replace the crate-local `InterruptKind` with core's `RunInterrupt` at its **five**
  sites: the definition (`driver.rs:87`), the match (`driver.rs:340`), the test use
  (`driver.rs:722`), the import (`workflow.rs:51`), and the `select!` arms
  (`workflow.rs:277–278`).
- **Keep `InterruptKind` as a deprecated alias**, so this is not a breaking change:
  ```rust
  #[deprecated(note = "renamed; use `paigasus_helikon_core::RunInterrupt`")]
  pub type InterruptKind = paigasus_helikon_core::RunInterrupt;
  ```
  `driver` is a `pub mod` (`lib.rs:451`), so `InterruptKind` is public API of a
  published `0.3.1` crate. A type alias preserves the path *and* variant access
  (`InterruptKind::Cancelled` resolves through an alias since Rust 1.37) at zero cost.
  This matters more than usual: **release-plz runs no `cargo-semver-checks`** in this
  repo (no `semver_check` key in `release-plz.toml` or `release-plz.yml`), and
  `refactor` is `increment: None` in `.versionrc` — so a clean removal would ship as a
  *patch* bump, i.e. a semver violation published to crates.io. The alias removes the
  hazard entirely rather than relying on getting the bump right by hand.
- `driver.rs:344`'s match gains a total, non-panicking wildcard arm mapping to
  `RunStatusPayload::AgentFailed(ErrorKindPayload::Other { .. })`. Panicking is not an
  option here: `interrupt()` runs inside a Temporal workflow, where a workflow-task
  failure retries indefinitely.
- **Route `error.rs:57–58` through the canonical rendering** —
  `RunStatusPayload::Cancelled => Err(RunInterrupt::Cancelled.run_error())` and
  likewise for `TimedOut` — so `run_error()` is genuinely the single source of truth
  rather than a third copy. Its existing tests (`error.rs:212–235`) are unchanged and
  guard the routing.
- `Phase::Done` short-circuit in `interrupt()` is unchanged — it is the same rule in a
  structural shape, and it must keep returning the cached outcome (which carries
  `Completed(FinalOutput)`, a payload no `Option<RunError>` resolver could express).
- **Temporal replay compatibility:** `InterruptKind`/`RunInterrupt` never crosses a
  serialization boundary — only `RunStatusPayload` does (`payloads.rs:61–70`). The
  workflow's command sequence is therefore unchanged and in-flight replays are safe.
  Stated explicitly because it is the first question any reviewer of a workflow diff
  must answer.

### `paigasus-helikon-runtime-axum`

- Delete `event_log::is_terminal` and its `pub(crate)` import sites; the five call
  sites (`event_log.rs:107,205,221`, `handlers/events.rs:111`, `handlers/runs.rs:417`)
  use `AgentEvent::is_terminal`.
- `RunHandle::synthetic_terminal_frame` is unchanged: its CWE-209 public strings are
  transport policy, not the precedence rule.

### `paigasus-helikon-runtime-actix`

Mirrors axum verbatim — and must land in the same PR to preserve the byte-parity the
conformance suite asserts.

- Delete `event_log::is_terminal` and its `pub(crate)` import sites; the five call
  sites (`event_log.rs:107,205,221`, `handlers/runs.rs:496`, `handlers/events.rs:116`)
  use `AgentEvent::is_terminal`.
- `registry.rs:61–88`'s `synthetic_terminal_frame` is unchanged, for the same reason.

### `paigasus-helikon-runtime-agentcore`

No change. It holds no copy of the rule; its prose reference to `Runner::run`'s
cancellation-precedence contract stays accurate.

## Testing

| Where | What |
|---|---|
| core (`runner.rs`, new inline `#[cfg(test)] mod interrupt_tests`) | Truth table for `effective_interrupt` — all six of `{None, Some(Cancelled), Some(TimedOut)} × {saw_terminal, !saw_terminal}`. |
| core (same module) | `run_error` and `terminal_message` mappings for both variants. |
| core (`agent.rs`, new inline `#[cfg(test)] mod terminal_tests`) | Exhaustiveness guard for `is_terminal`, in the style of the existing `non_terminal_is_exactly_the_complement_of_is_terminal` in `agentcore/src/a2a/types.rs:405`: an explicit classification of all **17** `AgentEvent` variants that must agree with `is_terminal`, so a newly added terminal variant cannot be silently misclassified. |
| tokio `tests/run_control.rs` | **Unchanged** behaviour guard for `run`: `cancel_aborts_in_flight_run:20`, `timeout_returns_timeout:46`, `prefired_cancel_still_completes_ready_run:65`, `terminal_then_late_cancel_reports_completed:171`. |
| tokio `tests/run_streamed.rs` | **Unchanged** behaviour guard for `run_streamed` — the other half of the migration: `streamed_cancel_emits_terminal_runfailed:105` (also the only thing pinning the literal `"run cancelled"`, at :140) and `terminal_then_late_cancel_no_synthetic_terminal:255`. |
| tokio `tests/run_streamed.rs` | **New** `streamed_timeout_emits_terminal_runfailed`, asserting `error == "run timed out"`. Nothing currently pins the streamed *timeout* arm (`src/lib.rs:249–252`) — and the refactor rewrites exactly that block. The core `terminal_message` test catches a swapped string but not a mis-wired arm. |
| temporal (`driver.rs`'s existing `#[cfg(test)] mod tests`) | **New** `terminal_wins_over_late_interrupt`: drive a `DurableDriver` to `Finished`, *then* call `driver.interrupt(RunInterrupt::Cancelled)`, and assert the returned status is still `Completed(_)`. Cross-check the two gates in the same test: `assert_eq!(effective_interrupt(Some(RunInterrupt::Cancelled), outcome.events.iter().any(AgentEvent::is_terminal)), None)`. Add the `AgentFailed` variant via `apply_model_failure`. Must live inline — `Phase` and the `phase` field are private, so an integration test cannot observe `Phase::Done`. |
| axum, actix | **Unchanged.** Deleting each local `is_terminal` is compile-level proof of the swap; `tests/runtime-http-conformance/tests/parity.rs` guards the byte-parity between them. |

Note on what the temporal test replaces: an earlier draft proposed asserting
`Phase::Done ⟹ events end in a terminal`. That premise is true, but it is already
covered twice (`driver.rs:523`, `:757`) and — because it never calls `interrupt()` —
it would not have exercised the `Phase::Done` short-circuit at all. The
short-circuit is temporal's SMA-421-equivalent branch and is currently **untested**;
`interrupt_returns_partial_events:697` only interrupts mid-drive.

## Documentation

- `crates/paigasus-helikon-core/src/runner.rs:94–101` and `:117–119` — `Runner::run`
  and `Runner::run_streamed` today carry the authoritative *prose* statement of the
  precedence rule. Add intra-doc links to `[`RunInterrupt`]` and
  `[`effective_interrupt`]` so the prose and the executable rule cannot drift. Both
  targets are public, so this does not trip `rustdoc::private_intra_doc_links`.
- `docs/book/src/concepts/core-primitives.md:17` — the `Runner<Ctx>` bullet gains a
  sentence on the cancellation-precedence rule and the new helpers. This is the page a
  custom-`Runner` author reads, and the surface is now public API.
- `crates/paigasus-helikon-core/src/agent.rs:346` — the `AgentEvent` doc says
  "Fourteen variants"; there are **17**. Fixed as a drive-by, since this PR adds an
  `impl AgentEvent` block and a test enumerating every variant to that exact file, and
  no lint catches prose drift.
- Crate `README.md`s — **no change**, a conscious call. Core's README describes its
  type surface at "…" granularity and no install, feature, or usage story moves. The
  other crates' public usage is unaffected (the `InterruptKind` alias keeps temporal's
  path working).

## Release plumbing

No manual version bumps. The precise mechanism — not merely "every crate is already
published", which is also true of a stub ascend and would mislead:

1. No crate's `version` changes in this feature PR, so the merge commit's `release-plz
   release` job publishes nothing.
2. release-plz then opens a `chore: release` PR that bumps `paigasus-helikon-core` and
   its consumers **together**, and publishes them in dependency order.
3. `cargo publish --verify` for `runtime-tokio` / `-temporal` / `-axum` / `-actix`
   therefore resolves against the freshly published core, not a stale registry copy.

The same-PR manual-bump ritual (and its facade-cascade caveat) applies only to a crate
ascending from `0.0.0`; performing one here would *defeat* `dependencies_update`'s
cascade.

Because the `InterruptKind` alias keeps every public path intact, no crate in this PR
takes a breaking change — which matters given the verified absence of
`cargo-semver-checks` in the pipeline.

## Non-goals

- Shared synthesis helpers for the "may I synthesize a terminal?" half of the rule.
- Any change to `runtime-agentcore`.
- Closing the pre-existing `!saw_terminal` + no-interrupt finalize gap in
  `run_streamed` (noted above as knowingly preserved).
- Any behaviour change whatsoever. The SMA-421 regression tests must pass untouched.
