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
not. The same rule is expressed across **four crates and six sites, in three
different shapes** (a scanning gate, a structural gate, and three synthesis gates):

| Site | Shape of the rule |
|---|---|
| `runtime-tokio/src/lib.rs:41,182–191` | Scans collected events for a terminal, gates `Outcome::Cancelled/TimedOut` on `!saw_terminal` |
| `runtime-tokio/src/lib.rs:243–255` | Synthesis half: only synthesize a terminal when `!saw_terminal` |
| `runtime-temporal/src/driver.rs:340` | **Structural**: `interrupt()` returns the cached `Phase::Done` outcome if there is one — terminal wins with no event scan |
| `runtime-temporal/src/runner.rs:276–280` | Synthesis half, as "don't push `RunFailed` if `events.last()` already is one" |
| `runtime-axum/src/event_log.rs:20` | Literal copy-paste of tokio's `is_terminal` |
| `runtime-axum/src/registry.rs:62` | Synthesis half, as `synthetic_terminal_frame(saw_terminal)` with CWE-209 public strings |
| `runtime-agentcore` | Holds no copy. Delegates to the wrapped runner; documents the contract in prose (`invoke.rs:216`) |

Two consequences for the design:

1. `is_terminal` is genuine copy-paste duplication (tokio + axum) and should collapse
   to one definition.
2. A "controlled stream → result" combinator — the ticket's second suggested shape —
   fits **tokio and nothing else**. Temporal's interrupt arrives from a durable
   Temporal timer inside a workflow, not a `tokio::select!`; axum has no cancel
   boundary of its own. Such a combinator would have exactly one consumer.

## Approach

**Hoist the *decision*, not the control flow.** Core owns a small, pure, well-tested
vocabulary for the rule. Each runner keeps the control-flow shape that suits its
execution model and calls the same decision. This fits all four sites and forces
nothing.

Two alternatives were considered and rejected:

- **Decision + shared synthesis helpers.** The synthesis half has genuinely different
  policy at each site — tokio keys on the interrupt, temporal on the failure result,
  axum on stream-ended-without-terminal with its own CWE-209-safe public strings. A
  shared helper would have to be parameterised until it stopped carrying meaning.
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

`AgentEvent` is `#[non_exhaustive]`, so `matches!` (which already carries an implicit
wildcard) is the correct construction.

### `RunInterrupt` and the rule — `crates/paigasus-helikon-core/src/runner.rs`

Placed next to `RunResultStreaming`, as the ticket specifies. `core/src/lib.rs`
carries `pub use runner::*`, so both are re-exported automatically.

```rust
/// Why a runner's control boundary aborted a run before its natural end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunInterrupt {
    /// The run's `CancellationToken` fired.
    Cancelled,
    /// The run exceeded `RunConfig::timeout`.
    TimedOut,
}

impl RunInterrupt {
    /// The `RunError` this interrupt surfaces at the runner boundary.
    #[must_use]
    pub fn run_error(self) -> RunError {
        match self {
            Self::Cancelled => RunError::Cancelled,
            Self::TimedOut => RunError::Timeout,
        }
    }

    /// Canonical `error` text for a synthesized terminal `RunFailed` frame.
    #[must_use]
    pub fn terminal_message(self) -> &'static str {
        match self {
            Self::Cancelled => "run cancelled",
            Self::TimedOut => "run timed out",
        }
    }
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

### Why `Option<RunInterrupt>` and not `Option<RunError>`

An earlier sketch had the resolver return `Option<RunError>`. That serves
`Runner::run` but not `run_streamed`, which needs the *interrupt* in order to render
a message — so it would require either a second near-duplicate resolver or an
`.is_some()` check followed by re-deriving the interrupt. Returning
`Option<RunInterrupt>` gives **one rule with two renderings** (`run_error()` and
`terminal_message()`) and one function fewer.

### Why `RunInterrupt` is *not* `#[non_exhaustive]`

54 types in core carry `#[non_exhaustive]`, including `RunError` and `AgentEvent`.
`RunInterrupt` deliberately does not.

Every consumer that *maps* the enum rather than merely rendering it — today,
`DurableDriver::interrupt`'s `RunInterrupt → RunStatusPayload` match — would be
forced to add a wildcard arm. A wildcard arm in this code path is exactly the silent
fail-open that SMA-421 exists to prevent, and `interrupt()` runs inside a Temporal
workflow, where panicking on the unreachable arm is not an acceptable escape hatch
either (a workflow task failure retries indefinitely).

Adding a third interrupt kind is by definition a semantic change every runner must
handle. It should break their builds loudly. This is a considered deviation from
workspace convention, not an oversight.

## Call-site migrations

### `paigasus-helikon-runtime-tokio`

- Delete the file-local `Outcome` enum and `is_terminal` helper.
- `controlled` commits `Option<RunInterrupt>` (cell defaults to `None`, replacing
  `Outcome::Completed`). `OutcomeHandle` becomes an `Option<RunInterrupt>` read
  handle and is renamed `InterruptHandle`; its binding at the two call sites is
  renamed `outcome` → `interrupt` to match.
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

### `paigasus-helikon-runtime-temporal`

- Replace the crate-local `InterruptKind` with core's `RunInterrupt` at its three
  sites (`driver.rs:87` definition, `driver.rs:340` match, `workflow.rs:277–278`).
- `Phase::Done` short-circuit in `interrupt()` is unchanged — it is the same rule in
  a structural shape, and it must keep returning the cached outcome (which carries
  `Completed(FinalOutput)`, a payload no `Option<RunError>` resolver could express).
- **Breaking change** to the temporal crate's public API: `driver` is a `pub mod`, so
  `InterruptKind` is public. On a `0.x` crate release-plz treats this as a minor bump.

### `paigasus-helikon-runtime-axum`

- Delete `event_log::is_terminal` and its `pub(crate)` import sites; the five call
  sites (`event_log.rs:107,205,221`, `handlers/events.rs:111`, `handlers/runs.rs:417`)
  use `AgentEvent::is_terminal`.
- `RunHandle::synthetic_terminal_frame` is unchanged: its CWE-209 public strings are
  transport policy, not the precedence rule.

### `paigasus-helikon-runtime-agentcore`

No change. It holds no copy of the rule; its prose reference to `Runner::run`'s
cancellation-precedence contract stays accurate.

## Testing

| Where | What |
|---|---|
| core | Truth table for `effective_interrupt` — all four of `{None, Some(Cancelled), Some(TimedOut)} × {saw_terminal, !saw_terminal}`. |
| core | `run_error` and `terminal_message` mappings for both variants. |
| core | Exhaustiveness guard for `is_terminal`, in the style of the existing `non_terminal_is_exactly_the_complement_of_is_terminal` test in `agentcore/src/a2a/types.rs`: an explicit classification of every `AgentEvent` variant that must agree with `is_terminal`, so a newly added terminal variant cannot be silently misclassified. |
| tokio | **Unchanged.** SMA-421's regression tests in `tests/run_control.rs` — `cancel_aborts_in_flight_run`, `timeout_returns_timeout`, `prefired_cancel_still_completes_ready_run`, `terminal_then_late_cancel_reports_completed` — are the behaviour guard. If they pass, the refactor is behaviour-preserving. |
| temporal | New parity test: drive a `DurableDriver` to `Phase::Done`, then assert `outcome.events.iter().any(AgentEvent::is_terminal)`. Pins the structural gate against the scanning gate so the two shapes cannot silently drift apart. |
| axum | **Unchanged.** Deleting the local `is_terminal` is compile-level proof of the swap. |

## Documentation

- `docs/book/src/concepts/core-primitives.md:17` — the `Runner<Ctx>` bullet gains a
  sentence on the cancellation-precedence rule and the `RunInterrupt` /
  `effective_interrupt` helpers. This is the page a custom-`Runner` author reads, and
  the surface is now public API.
- Crate `README.md`s — **no change**, a conscious call. Core's README describes its
  type surface at "…" granularity and no install, feature, or usage story moves. The
  other crates' public usage is unaffected.

## Release plumbing

Every crate touched is already published, so this is pure release-plz auto-flow:
**no manual version bumps**. The manual same-PR bump ritual applies only to a crate
ascending from `0.0.0`; performing one here would defeat `dependencies_update`'s
facade cascade.

## Non-goals

- Shared synthesis helpers for the "may I synthesize a terminal?" half of the rule.
- Any change to `runtime-agentcore`.
- Any behaviour change whatsoever. The SMA-421 regression tests must pass untouched.
