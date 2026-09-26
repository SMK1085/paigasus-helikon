# SMA-516: cover `run_streamed`'s terminal synthesis with runner-level tests

- **Linear:** [SMA-516](https://linear.app/smaschek/issue/SMA-516)
- **Crate:** `paigasus-helikon-runtime-temporal`
- **Kind:** test coverage plus a private refactor. No public API change.

## Problem

`TemporalRunner::run_streamed` turns the durable run's `(events, result)` pair
into a `RunResultStreaming`. The only test of that assembly is
`tests/temporal_live.rs`. That test is env-gated and runs only in the
signal-only `temporal-it` job. Two defects lived in this code, and review found
both of them, not tests (see the ticket: `cb7dbf6`, `da442b0`).

## What already exists (the ticket predates it)

SMA-515 (PR #195) already split two pure helpers out of `run_streamed`, with
unit tests in `src/runner.rs`:

| Helper | Covers | Tests |
|---|---|---|
| `synthetic_terminal_event(&result)` | which frame, which text | 4 tests (cancel, timeout, success, agent failure) |
| `append_synthetic_terminal(&mut events, event)` | the `is_terminal` duplicate guard | `guard_suppresses_a_second_terminal`, `guard_appends_when_the_log_has_no_terminal`, `no_event_appends_nothing` |

The guard cases in the ticket's list are therefore already pinned. This spec
covers only what is still untested.

## Remaining gap

These lines in `run_streamed` have no cheap test:

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
2. a failed run does not collect as `Ok`;
3. the helpers are composed in the correct order, with the correct stream
   constructor (`with_failure`, not `new`).

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
(`synthesize_terminal(&mut events, result, &FailureSlot)`). The reason: the
unit under test must include the `with_failure` call and the `collect()` read.
A helper that returns the finished `RunResultStreaming` puts the whole path
under test. A helper that takes a caller-owned slot leaves the
`with_failure` wiring outside the test.

The doc comment on `run_streamed` and the explanatory comments move with the
code. The behavior does not change.

### Tests

Add `#[tokio::test]` tests in the existing `mod tests`. They call
`into_streaming(..).collect().await` and, where the frame count matters, drain
`events` directly. `tokio` with `macros` is already a dev-dependency.

| # | Input log | Input result | Expected |
|---|---|---|---|
| T1 | `[TurnStarted, RunFailed{"<agent text>"}]` (realistic: the driver writes `RunFailed` itself) | `Err(Agent(MaxTurnsExceeded(3)))` | `collect()` is `Err(RunError::Agent(AgentError::MaxTurnsExceeded(3)))`; exactly one terminal |
| T2 | `[TurnStarted]` (defensive: no terminal in log) | `Err(Agent(MaxTurnsExceeded(3)))` | one synthetic `RunFailed`; `collect()` is `Err(RunError::Agent(MaxTurnsExceeded(3)))` |
| T3 | `[TurnStarted]` | `Err(Cancelled)` | exactly one terminal, `RunFailed { error: "run cancelled" }`; `collect()` is `Err`, not `Ok` |
| T4 | `[TurnStarted]` | `Err(Timeout)` | one `RunFailed` with `RunInterrupt::TimedOut.terminal_message()`; `collect()` is `Err` |
| T5 | `[]` (infra failure: `run_inner` returns no events) | `Err(Other("temporal workflow failed: …"))` | one `RunFailed` carrying the infra text; `collect()` is `Err`, not `Ok` |
| T6 | `[MessageOutput(assistant "hi"), RunCompleted]` | `Ok(..)` | no frame appended; `collect()` is `Ok` with `final_output == "hi"` |
| T7 | `[RunCompleted]` | `Err(Cancelled)` | see "Decided: the log wins" below |

For cancel, timeout, and infra results the slot stays empty, so `collect()`
returns `RunError::Other(<frame text>)`. The tests assert `Err` and the text,
not a variant, for these cases. That matches `TokioRunner`, which also leaves
the slot empty for an interrupt.

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
comment states that `run()` and `collect()` differ only in this state, and
names the driver test that makes it unreachable. No production change.

### Mutation checks

Each mutation must make at least one new test fail. Then restore the code.
The plan records the failing test names for each mutation.

| Mutation in `into_streaming` | Must fail |
|---|---|
| Remove `failure.set(err)` | T1, T2 |
| Use `RunResultStreaming::new` instead of `with_failure` | T1, T2 |
| Remove the `append_synthetic_terminal` call | T2, T3, T4, T5 |
| Append unconditionally (bypass the guard) | T1, T7 |
| Pass `None` as the synthetic event | T2, T3, T4, T5 |

The guard mutation inside `append_synthetic_terminal` is already covered by
the SMA-515 tests and is not repeated here.

## Out of scope

- Promoting `temporal-it` to a required context. The ticket calls it
  complementary. It needs a flake-rate measurement first.
- Any change to `collect()` in core, or to the driver.
- Book and README edits. This is an internal refactor plus tests, with no
  change to public API, usage, or the crate roster. That is a deliberate
  decision under the CLAUDE.md docs rules.

## Release impact

The change touches `src/runner.rs`, a packaged file. release-plz therefore
gives `paigasus-helikon-runtime-temporal` a patch bump and cascades to the
facade, even for a `test`/`refactor` commit. That is expected and harmless. No
hand-bump of any version.

## Acceptance

- `into_streaming` exists; `run_streamed` only calls `run_inner` and it.
- T1 to T7 pass, and each mutation above fails at least one of them.
- `cargo fmt`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`,
  and `cargo test -p paigasus-helikon-runtime-temporal --all-features` are green.
