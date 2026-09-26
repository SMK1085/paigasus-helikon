# SMA-516: cover `run_streamed`'s terminal synthesis with runner-level tests

- **Linear:** [SMA-516](https://linear.app/smaschek/issue/SMA-516)
- **Crate:** `paigasus-helikon-runtime-temporal`
- **Kind:** test coverage plus a private refactor. No public API change.

## Problem

`TemporalRunner::run_streamed` turns the durable run's `(events, result)` pair
into a `RunResultStreaming`. **No test calls `run_streamed`.** The ticket says
that `tests/temporal_live.rs` is its only test. That is not correct: the live
suite calls only `runner.run(..)`. So the baseline has zero coverage, in any
job. Two defects lived in this code, and review found both of them, not tests
(see the ticket: `cb7dbf6`, `da442b0`).

## What already exists (the ticket predates it)

SMA-515 (PR #195) already split two pure helpers out of `run_streamed`, with
unit tests in `src/runner.rs`:

| Helper | Covers | Tests |
|---|---|---|
| `synthetic_terminal_event(&result)` | which frame, which text | 4 tests (cancel, timeout, success, agent failure) |
| `append_synthetic_terminal(&mut events, event)` | the `is_terminal` duplicate guard | `guard_suppresses_a_second_terminal`, `guard_appends_when_the_log_has_no_terminal`, `no_event_appends_nothing` |

These tests kill the "remove the guard" mutation. They do **not** kill two
narrower guard mutations, because their logs have only one element (see the
mutation table). This spec covers those two as well.

## Remaining gap

These lines in `run_streamed` have no test:

```rust
let failure = FailureSlot::new();
let terminal = synthetic_terminal_event(&result);
if let Err(RunError::Agent(err)) = result {
    failure.set(err);
}
append_synthetic_terminal(&mut events, terminal);
Ok(RunResultStreaming::with_failure(stream::iter(events).boxed(), failure))
```

No test shows that:

1. the `FailureSlot` gets the `AgentError`, so `collect()` returns
   `RunError::Agent(..)` and not `RunError::Other(<string>)`;
2. the slot stays empty for cancel, timeout, and infra failures, so `collect()`
   returns `RunError::Other(<frame text>)`, as `TokioRunner` does;
3. a failed run does not collect as `Ok`;
4. the stream uses `with_failure`, not `new`.

The order of the two helper calls is not a gap. `synthetic_terminal_event`
borrows `result` before the `if let` moves it, and any other order does not
compile.

## Design

### Extract one pure helper

Move the assembly above into a private free function in `src/runner.rs`, next
to the two existing helpers:

```rust
fn into_streaming(
    mut events: Vec<AgentEvent>,
    result: Result<RunResult, RunError>,
) -> RunResultStreaming
```

`run_streamed` becomes a thin caller:

```rust
let (events, result) = self.run_inner(agent, ctx, input, config).await?;
Ok(into_streaming(events, result))
```

The name and signature differ from the ticket's suggestion
(`synthesize_terminal(&mut events, result, &FailureSlot)`). The unit under test
must include the `with_failure` call and the `collect()` read. A helper that
returns the finished `RunResultStreaming` puts the whole path under test. A
helper that takes a caller-owned slot leaves the `with_failure` wiring outside
the test.

**Alternative considered: a client seam.** `TemporalRunner` holds a concrete
`temporalio_client::Client`, so there is no injection point for a fake client.
A trait over `run_workflow` would allow a full runner-level test, including
session finalize on the streamed path. It changes the runner's structure for a
test-only gain, so this ticket does not do it. The live test below covers the
join between `run_inner` and `into_streaming` instead.

### Documentation

- The rustdoc on `run_streamed` **stays on `run_streamed`**. It is a public
  trait-impl method, and docs.rs shows its doc. Only the inline `//` comments
  move into `into_streaming`.
- Correct its second paragraph. Today it says that a failed run's error goes
  into the `FailureSlot` and that `collect` returns `RunError::Agent`. The new
  text: an agent failure goes into the slot and `collect` returns the typed
  `RunError::Agent`, as `TokioRunner` does. Cancel, timeout, and infra failures
  leave the slot empty, and `collect` returns `RunError::Other` with the
  terminal frame's text. Replace "guaranteed present" with "present whenever the
  run failed and its log did not already end in a terminal".
- The public doc must not link to the private `into_streaming`. The
  `rustdoc::private_intra_doc_links` lint fails the required `docs` gate.

### Unit tests

Add `#[tokio::test]` tests in the existing `mod tests`. `tokio` with `macros` is
already a dev-dependency.

**Test mechanics.** On `Err`, `collect()` drops the events, and the `failure`
field is private. `RunError` is not `Clone`. So:

- For each case, one small builder function returns the `(events, result)`
  input. For the cases that expect `Err` (T1 to T5), each test calls
  `into_streaming` twice. It drains the first handle's `events` to check the
  frames, and it calls `collect()` on the second handle. Never drain and
  collect the same handle: the stream is then empty, and `collect()` returns
  `Ok` for any input.
- For the cases that expect `Ok` (T6, T7), one handle is enough. The frames
  come from the collected `RunResult.events`.
- `AgentError`, `RunError`, and `AgentEvent` do not implement `PartialEq`.
  Patterns bind the payload literally, for example
  `Err(RunError::Agent(AgentError::MaxTurnsExceeded(3)))`, not
  `MaxTurnsExceeded(_)`.

**Frame assertions, for every case:**

- exactly one terminal: `frames.iter().filter(|e| e.is_terminal()).count() == 1`;
- the terminal is the last frame;
- the input prefix (for example `TurnStarted`) is still in place, in order.

| # | Input log | Input result | Expected |
|---|---|---|---|
| T1 | `[TurnStarted, RunFailed{"<agent text>"}]` (the driver writes `RunFailed` itself, as `apply_model_failure` does) | `Err(Agent(MaxTurnsExceeded(3)))` | log unchanged, one terminal; `collect()` is `Err(RunError::Agent(AgentError::MaxTurnsExceeded(3)))` |
| T2 | `[]` (reachable today: `workflow.rs` `unknown_agent_outcome` returns `AgentFailed` with no events; the driver's `Handoff` and unknown-`NextAction` arms also finish without a `RunFailed`) | `Err(Agent(AgentError::Other("no agent named 'x' is registered on this worker")))` | one synthetic `RunFailed` whose `error` equals the `AgentError`'s `Display` text exactly; `collect()` is `Err(RunError::Agent(AgentError::Other(_)))` with that same text |
| T3 | `[TurnStarted]` | `Err(Cancelled)` | last frame is `RunFailed { error: "run cancelled" }`; `collect()` is `Err(RunError::Other(e))` with `e.to_string() == "run cancelled"` |
| T4 | `[TurnStarted]` | `Err(Timeout)` | last frame is `RunFailed` with `RunInterrupt::TimedOut.terminal_message()`; `collect()` is `Err(RunError::Other(e))` with that exact text |
| T5 | `[]` (infra failure: `run_inner` returns no events) | `Err(Other("temporal workflow failed: boom"))` | one `RunFailed` with that exact text; `collect()` is `Err(RunError::Other(e))` with that exact text |
| T6 | `[MessageOutput(assistant "hi"), RunCompleted]` | `Ok(..)` | no frame appended; `collect()` is `Ok` with `final_output == "hi"` |
| T7 | `[TurnStarted, RunCompleted]` | `Err(Cancelled)` | see "Decided: the log wins" below |

T3 to T5 pin the `RunError::Other` variant on purpose. That is the parity with
`TokioRunner`, which also leaves the slot empty for an interrupt. If `collect()`
later returns `RunError::Cancelled` or `RunError::Timeout`, these tests must
change in the same PR, deliberately.

### Decided: the log wins (T7)

The ticket asks for two rules that conflict in one state. A log that ends in
`RunCompleted` with an `Err` result gets no second terminal (the SMA-421
guard). `collect()` then sees no `RunFailed`, does not read the slot, and
returns `Ok`. For the same outcome `Runner::run` returns `Err`.

This state is unreachable today. `DurableDriver::interrupt` returns the `Done`
outcome unchanged, so a log that ends in `RunCompleted` always maps to
`RunStatusPayload::Completed` and `Ok`. The driver test
`terminal_wins_over_late_interrupt` pins that behavior.

Sven chose to keep the current behavior (2026-09-26). T7 pins it: exactly one
terminal, the original `RunCompleted`, and `collect()` is `Ok`. The test
comment says that `run()` and `collect()` differ **in `Ok` versus `Err`** only
in this state, and names the driver test that makes it unreachable. (They
always differ in the `Err` variant for interrupts: `run()` returns
`RunError::Cancelled`, `collect()` returns `RunError::Other`.) No production
change.

### Live test

Add one streamed case to `tests/temporal_live.rs`. It is a streamed copy of
`model_failure_maps_to_typed_agent_error`: the same worker, agent, and
`FailingModel`, but it calls `runner.run_streamed(..)` and then `.collect()`.
It asserts `Err(RunError::Agent(_))` with a message that contains
`"connection lost"`. It is env-gated like its siblings and runs in the
signal-only `temporal-it` job, which also runs on `pull_request`. This is the
only test of the join between `run_inner` and `into_streaming`.

### Mutation checks

The plan records, for each mutation, the test names that failed. **Every test
listed for a mutation must fail**, not only one of them.

| Mutation | Must fail |
|---|---|
| Remove `failure.set(err)` | T1, T2 |
| Use `RunResultStreaming::new` instead of `with_failure` | T1, T2 |
| Set the slot for every `Err` (`Err(e) => failure.set(AgentError::Other(e.into()))`) | T3, T4, T5 |
| Remove the `append_synthetic_terminal` call | T2, T3, T4, T5 |
| Pass `None` as the synthetic event | T2, T3, T4, T5 |
| Append unconditionally (bypass the guard) | T1, T7 |
| In `append_synthetic_terminal`: test `RunCompleted` only, instead of `is_terminal` | T1 |
| In `append_synthetic_terminal`: read `events.first()` instead of `events.last()` | T1 |

The last two mutations survive all three SMA-515 guard tests. T1's
terminal-count assertion is the test that kills them.

## Out of scope

- Promoting `temporal-it` to a required context. The ticket calls it
  complementary. It needs a flake-rate measurement first.
- A client seam for `TemporalRunner` (see "Alternative considered").
- `collect_typed` coverage. Only core uses it today.
- Any change to `collect()` in core, or to the driver.
- Book and README edits. This is an internal refactor, a doc correction, and
  tests, with no change to public API, usage, or the crate roster. That is a
  deliberate decision under the CLAUDE.md docs rules.

## Release impact

The change touches `src/runner.rs`, a packaged file. release-plz therefore
gives `paigasus-helikon-runtime-temporal` a patch bump and cascades to the
facade, even for a `test`/`refactor` commit. That is expected and harmless. No
hand-bump of any version.

## Acceptance

- `into_streaming` exists; `run_streamed` only calls `run_inner` and it.
- The `run_streamed` rustdoc stays in place and has the corrected text.
- T1 to T7 pass, and for each mutation above every listed test fails.
- The live streamed case compiles and loud-skips locally without Temporal.
- The CI gates are green:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-features --all-targets -- -D warnings`
  - `cargo test --workspace --all-features`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
