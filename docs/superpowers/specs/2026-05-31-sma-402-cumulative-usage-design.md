# SMA-402 — `RunResult.usage` reports cumulative token usage across all turns

**Status:** Design (approved)
**Issue:** [SMA-402](https://linear.app/smaschek/issue/SMA-402)
**Branch:** `feature/sma-402-runresultusage-reports-last-turn-token-usage-not-cumulative`
**Date:** 2026-05-31
**References:** Discovered during [SMA-322](https://linear.app/smaschek/issue/SMA-322) (PR #51), to which this is `relatedTo`.

## 1. Summary

`RunResult.usage` — and the values feeding it, `AgentEvent::RunCompleted { usage }`
and `LoopState`'s `FinalOutput { usage }` — report only the **last turn's** token
usage, not the cumulative total across all turns of a multi-turn run. For any run with
more than one model turn this under-counts input/output/cached/reasoning/total tokens,
which corrupts cost / billing / metrics consumers.

The root cause is **two parallel accumulators that disagree**:

1. The OTel `invoke_agent` run span (added in SMA-322) accumulates correctly — the
   driver `+=`'s each turn's final usage into private `run_input_tokens` /
   `run_output_tokens` counters and records them at `RunCompleted`.
2. The `RunResult.usage` path does **not** accumulate. The driver puts each turn's
   per-turn `usage` into `TransitionInput::ModelResponse { usage }`; `transition`
   passes that single-turn value straight through to `RunCompleted { usage }` /
   `FinalOutput { usage }`; and `RunResultStreaming::collect` **assigns** (not adds)
   it to `RunResult.usage`. So `RunResult.usage` = the final turn only.

The fix makes the loop carry a single cumulative total **inside `LoopState`** (the
pure, resumable-by-construction state machine), and collapses the OTel span accumulator
into reading that same total — so the run span and `RunResult` can no longer diverge.

Within-turn semantics are already correct and unchanged: each turn contributes its
**last-retained** `Usage` snapshot (Anthropic emits incremental updates; the driver
retains the last, never sums within a turn). This change only adds **cross-turn**
summing.

## 2. Scope

### In scope

* Carry a running `usage: TokenUsage` total in the four driveable, non-terminal
  `LoopState` variants and thread it through `transition`.
* Make `RunCompleted.usage` and `FinalOutput.usage` carry the cumulative total at every
  terminal arm.
* Delete the driver's parallel `run_input_tokens` / `run_output_tokens` i64 counters;
  source the run span's `gen_ai.usage.*` from the now-cumulative `RunCompleted.usage`.
* Accumulate all five `TokenUsage` fields (input, output, cached_input, reasoning,
  total) via the existing `TokenUsage::add`.
* Tests: a pure multi-step `transition` accumulation test, an end-to-end multi-turn
  regression test, and structured-output coverage (a structured run is inherently ≥ 2
  turns).
* Doc comments: document the new state fields; the existing "aggregated across the run"
  docs become true rather than aspirational.

### Out of scope (YAGNI)

* No changes to the shapes or signatures of `TokenUsage`, `RunResult`, `RunConfig`,
  `TransitionInput`, `TransitionCtx`, or any public method.
* No partial-usage surfacing on **failed** runs (a failed run produces no `RunResult`;
  the `Failed` variant carries no usage field).
* No per-turn usage event (the per-turn value already lands on the chat span).

## 3. Design

### 3.1 `loop_state.rs` — running total carried in state

Add `usage: TokenUsage` to the four driveable, non-terminal variants. Semantics: the
**cumulative total of all turns completed before entering this state**.

```
LoopState::CallingModel    { turn, usage }
LoopState::ExecutingTools  { calls, turn, usage }
LoopState::Finalizing      { turn, usage }
LoopState::RepairingOutput { turn, usage }
```

`Done(FinalOutput)` already holds `usage` → it becomes the grand total. `Failed` and the
not-driveable variants (`ApplyingHandoff`, `Compacting`, `NeedsApproval`) get **no**
field.

`transition` threads it. Let `prior = state.usage` and `total = prior + resp.usage`
(via `TokenUsage::add`):

| Current state + input | Next state / outcome | usage carried |
| --- | --- | --- |
| `CallingModel` + `Start` | `CallingModel{turn:0}` or `Finalizing` | `default()` (prior = 0) |
| `CallingModel` + `ModelResponse`(tool calls) | `ExecutingTools` | `total` |
| `ExecutingTools` + `ToolResults` | `CallingModel{turn+1}` | `state.usage` (tools add no tokens) |
| `CallingModel` + `ModelResponse`(no tools, no output) | terminal `RunCompleted` / `Done` | `total` |
| `CallingModel` + `ModelResponse`(no tools, output set) | `Finalizing{turn+1}` | `total` |
| `Finalizing` + `ModelResponse`(Ok) | terminal `RunCompleted` / `Done` | `total` |
| `Finalizing` + `ModelResponse`(Err) | `RepairingOutput` | `total` |
| `RepairingOutput` + `ModelResponse`(Ok) | terminal `RunCompleted` / `Done` | `total` |
| `RepairingOutput` + `ModelResponse`(Err) | `Failed` | — (not surfaced) |
| max-turns guards | `Failed` | — (not surfaced) |

Match arms that previously bound `{ turn }` now bind `{ turn, usage: prior }` to read
the carried total. The finalizing turn and the one repair turn are real model calls, so
their usage folds into the running total exactly like any other turn — this is why a
structured-output run (unconstrained turn + finalizing turn) is the cheapest case that
exercises cross-turn summing with zero tools.

Every new `usage` field gets a `///` doc comment (`missing_docs` is `-D warnings`).

### 3.2 `agent.rs` — single accumulator, parallel i64s deleted

* Seed the initial state as `CallingModel { turn: 0, usage: TokenUsage::default() }`.
* **Delete** `run_input_tokens` / `run_output_tokens` and their `+=` lines.
* In the events loop, change the `RunCompleted { .. }` arm to `RunCompleted { usage }`
  and record the **run span** `gen_ai.usage.input_tokens` / `output_tokens` from that
  now-cumulative usage.
* The **per-turn chat span** recording is unchanged — it correctly records the
  per-turn `usage` snapshot the driver builds from `latest_usage`.

Consequence: the run span and `RunResult.usage` both derive from the same
`RunCompleted.usage`, so the divergence flagged in the ticket is eliminated by
construction, not by keeping two accumulators in sync.

### 3.3 `runner.rs` — no change

Exactly one `RunCompleted` is emitted per **successful** run (the three terminal success
arms are mutually exclusive; failed runs emit `RunFailed`). Because that single event now
carries the cumulative total, `collect` / `collect_typed` doing `usage = *u` (assign) is
already correct. Only a doc clarification may be added.

### 3.4 Docs

`RunResult.usage`, `RunCompleted.usage`, and `FinalOutput.usage` already document
"aggregated across the run / all turns"; the fix makes those statements **true**. Add a
short "last-retained per turn, summed across turns" clarifier where it aids the reader,
and document the four new state fields.

## 4. Testing

* **`transition_unit.rs` (pure):** add `usage: TokenUsage::default()` to existing
  constructions and `..` to exhaustive `assert_matches!` patterns that don't assert
  usage. Add a new test driving
  `CallingModel{0}` →(tool call, `U1`)→ `ExecutingTools{U1}` →(results)→
  `CallingModel{1, U1}` →(text, `U2`)→ and asserting the terminal `Done` /
  `RunCompleted.usage == U1 + U2`.
* **`loop_happy_path.rs` (end-to-end, the ticket's regression):** extend
  `multi_turn_with_tool_call` (or add a sibling) to script `ModelEvent::Usage` in both
  turns and assert `result.usage == U1 + U2` across **all five fields**. Uses the
  existing `MockModel` scripted-events harness.
* **`structured_output.rs`:** assert the cumulative covers the unconstrained +
  finalizing turns (the tool-free multi-turn path).
* **`otel_spans.rs` (facade):** the existing multi-turn run-span tests must remain green
  — the run span is still the cross-turn sum, now sourced from the cumulative event.
  Run them to confirm no regression.

## 5. Release mechanics

A pure `fix(core):` change. Adding fields to `LoopState` struct variants is
source-breaking only for an external **exhaustive** match/constructor; the only
consumers are in-crate (the `agent.rs` driver and the `core` test suite). The
durable-runner crates that would reuse `transition` are docstring-only stubs at `0.0.0`.

`release-plz` patch-bumps `paigasus-helikon-core` through its **normal** flow, and its
`dependencies_update` cascade bumps the facade. The manual core-bump / facade-bump
ritual in `CLAUDE.md` applies only when an ascending stub uses same-PR core API — this
is not that case — so **no manual ritual is required**. To be confirmed during planning.

## 6. Risks & mitigations

* **Off-by-one / double-count in accumulation.** Mitigated by the single, explicit
  contract (`state.usage` = turns completed *before* this state; `total = prior +
  resp.usage` folds the current turn exactly once) and by the pure multi-step
  `transition` test plus the end-to-end assertion.
* **Silent OTel span regression.** Mitigated by keeping `otel_spans.rs` as a gate — the
  run-span values must be unchanged after re-sourcing from `RunCompleted.usage`.
* **Test churn from the new field.** Bounded to the `core` crate; mechanical
  (`usage: TokenUsage::default()` in constructions, `..` in non-asserting patterns).
