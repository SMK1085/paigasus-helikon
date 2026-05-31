# SMA-402 Cumulative Token Usage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `RunResult.usage` (and `AgentEvent::RunCompleted.usage` / `FinalOutput.usage`) report the cumulative token total across **all** model turns, not just the last turn.

**Architecture:** Carry a running `usage: TokenUsage` total inside the four driveable `LoopState` variants and fold each turn's final usage into it in the pure `transition` function. Collapse the driver's separate OTel-span i64 counters into reading the now-cumulative `RunCompleted.usage`, so the run span and `RunResult` can no longer disagree.

**Tech Stack:** Rust, `cargo`, `tokio`, `insta` (snapshots), the in-repo `MockModel`/`MockTool` test harness (`crates/paigasus-helikon-core/tests/common/mod.rs`).

**Spec:** `docs/superpowers/specs/2026-05-31-sma-402-cumulative-usage-design.md`

**Branch:** `feature/sma-402-runresultusage-reports-last-turn-token-usage-not-cumulative` (already created and checked out).

**Key constraints (verified against source):**
- `TokenUsage` and `FinalOutput` are `#[non_exhaustive]` → in **external test crates** you may **not** build them with a struct literal (E0639). Build `TokenUsage` via `TokenUsage::default()` then assign public fields; assert usage **field-by-field** rather than constructing an expected struct.
- The four `LoopState` variants are **not** marked `#[non_exhaustive]` (per spec §3.5) so the external `transition_unit.rs` can keep constructing them.
- `TokenUsage::add(&mut self, other)` already sums all five fields (`input_tokens`, `output_tokens`, `cached_input_tokens`, `reasoning_tokens`, `total_tokens`) and is `Copy`.
- Adding the field is a **breaking** public-API change → minor bump `0.2.4 → 0.3.0`, signalled by the `fix(core)!:` **PR title** (Task 6). Do **not** hand-edit the version.

---

## File Structure

| File | Responsibility | Change |
| --- | --- | --- |
| `crates/paigasus-helikon-core/src/loop_state.rs` | Pure state machine | Add `usage` field to 4 variants; thread cumulative total through `transition`; add `accumulate` helper |
| `crates/paigasus-helikon-core/src/agent.rs` | Async loop driver | Seed initial state with `usage`; delete the two i64 span counters; source run-span usage from cumulative `RunCompleted.usage` |
| `crates/paigasus-helikon-core/src/model.rs` | `Model` trait / `ModelEvent` | Doc-only: codify the last-wins `Usage` contract |
| `crates/paigasus-helikon-core/tests/loop_happy_path.rs` | End-to-end loop tests | New `multi_turn_usage_is_cumulative` regression test |
| `crates/paigasus-helikon-core/tests/transition_unit.rs` | Pure `transition` tests | Fix existing constructions/patterns for the new field; add `usage_accumulates_across_turns` lock test |
| `crates/paigasus-helikon-core/tests/structured_output.rs` | Structured-output tests | New `structured_run_usage_is_cumulative` test |

---

## Task 1: Make `RunResult.usage` cumulative

**Files:**
- Test: `crates/paigasus-helikon-core/tests/loop_happy_path.rs`
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs`
- Modify: `crates/paigasus-helikon-core/src/agent.rs:620`, `:649-650`, `:689-692`, `:811-819`
- Modify: `crates/paigasus-helikon-core/tests/transition_unit.rs` (compile fixes)

- [ ] **Step 1: Write the failing end-to-end test**

Append to `crates/paigasus-helikon-core/tests/loop_happy_path.rs`:

```rust
#[tokio::test]
async fn multi_turn_usage_is_cumulative() {
    use common::MockTool;

    // Turn 0: tool call carrying usage U0; turn 1: final text carrying usage U1.
    // Each turn's TokenUsage = { input, output, cached, reasoning, total=input+output }.
    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "1".into(),
                name: Some("echo".into()),
                args_delta: "{\"msg\":\"hi\"}".into(),
            },
            ModelEvent::Usage {
                input_tokens: 100,
                output_tokens: 20,
                cached_input_tokens: Some(10),
                reasoning_tokens: Some(5),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            ModelEvent::TokenDelta {
                text: "done".into(),
            },
            ModelEvent::Usage {
                input_tokens: 200,
                output_tokens: 8,
                cached_input_tokens: Some(0),
                reasoning_tokens: Some(3),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]);
    let tool = MockTool::new("echo", serde_json::json!("ok"));
    let mut agent = build_agent(model);
    agent.tools = vec![tool.clone() as std::sync::Arc<dyn paigasus_helikon_core::Tool<()>>];

    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("go"))
        .await
        .expect("agent.run should succeed");
    let result = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect("collect");

    // Cumulative across both turns (NOT the last turn only).
    assert_eq!(result.usage.input_tokens, 300, "input must sum 100 + 200");
    assert_eq!(result.usage.output_tokens, 28, "output must sum 20 + 8");
    assert_eq!(result.usage.cached_input_tokens, 10, "cached must sum 10 + 0");
    assert_eq!(result.usage.reasoning_tokens, 8, "reasoning must sum 5 + 3");
    assert_eq!(result.usage.total_tokens, 328, "total must sum 120 + 208");
}
```

- [ ] **Step 2: Run the test to verify it fails (RED)**

Run: `cargo test -p paigasus-helikon-core --test loop_happy_path multi_turn_usage_is_cumulative`
Expected: FAIL — current code reports only the last turn, so `input_tokens` is `200` not `300` (assertion `input must sum 100 + 200` fails).

- [ ] **Step 3: Add the `usage` field to the four driveable `LoopState` variants**

In `crates/paigasus-helikon-core/src/loop_state.rs`, edit each variant. `CallingModel`:

```rust
    /// About to call the model for turn `turn`.
    CallingModel {
        /// Zero-indexed turn counter.
        turn: u32,
        /// Cumulative token usage of all turns completed *before* this state
        /// (SMA-402). Folded forward on every transition; the terminal
        /// `Done` / `RunCompleted` carry the run's grand total.
        usage: TokenUsage,
    },
```

`ExecutingTools`:

```rust
    ExecutingTools {
        /// The tool calls to execute concurrently.
        calls: Vec<ToolCallRequest>,
        /// The turn that produced these calls.
        turn: u32,
        /// Cumulative token usage of all turns completed before this state
        /// (SMA-402); carried forward unchanged across tool execution.
        usage: TokenUsage,
    },
```

`Finalizing`:

```rust
    Finalizing {
        /// The turn index that produced this finalizing request.
        turn: u32,
        /// Cumulative token usage of all turns completed before this state (SMA-402).
        usage: TokenUsage,
    },
```

`RepairingOutput`:

```rust
    RepairingOutput {
        /// The turn index of the finalizing turn being repaired.
        turn: u32,
        /// Cumulative token usage of all turns completed before this state (SMA-402).
        usage: TokenUsage,
    },
```

- [ ] **Step 4: Add the `accumulate` helper**

In `crates/paigasus-helikon-core/src/loop_state.rs`, add this free function next to the other helpers (e.g. directly above `fn constrained_settings`):

```rust
/// Fold one turn's final usage into the running cross-turn total (SMA-402).
/// `TokenUsage` is `Copy`; returns the new cumulative total.
fn accumulate(prior: TokenUsage, turn: TokenUsage) -> TokenUsage {
    let mut total = prior;
    total.add(turn);
    total
}
```

- [ ] **Step 5: Thread the total through every `transition` arm**

In `crates/paigasus-helikon-core/src/loop_state.rs`, apply each before→after. (Line numbers are pre-edit anchors.)

**5a. Max-turns guard (~line 211)** — bind-ignore the new field:

```rust
// before
        (LoopState::CallingModel { turn }, _) if *turn >= ctx.max_turns => TransitionOutcome {
// after
        (LoopState::CallingModel { turn, .. }, _) if *turn >= ctx.max_turns => TransitionOutcome {
```

**5b. Start arm (~line 220)** — carry the seed usage forward into both sub-branches:

```rust
// before
        (LoopState::CallingModel { turn }, TransitionInput::Start { .. })
            if *turn < ctx.max_turns =>
// after
        (LoopState::CallingModel { turn, usage: prior }, TransitionInput::Start { .. })
            if *turn < ctx.max_turns =>
```

```rust
// before  (~line 231, the `Some(out) if tools empty` branch)
                        next_state: LoopState::Finalizing { turn: *turn },
// after
                        next_state: LoopState::Finalizing { turn: *turn, usage: *prior },
```

```rust
// before  (~line 244, the `_` branch)
                        next_state: LoopState::CallingModel { turn: *turn },
// after
                        next_state: LoopState::CallingModel { turn: *turn, usage: *prior },
```

**5c. Tool-calls branch (~line 253)** — bind state usage as `prior`, the turn's usage as `usage`:

```rust
// before
        (LoopState::CallingModel { turn }, TransitionInput::ModelResponse { items, .. })
            if items.iter().any(|i| matches!(i, Item::ToolCall { .. })) =>
// after
        (LoopState::CallingModel { turn, usage: prior }, TransitionInput::ModelResponse { items, usage, .. })
            if items.iter().any(|i| matches!(i, Item::ToolCall { .. })) =>
```

```rust
// before  (~line 279)
                next_state: LoopState::ExecutingTools {
                    calls: calls.clone(),
                    turn: *turn,
                },
// after
                next_state: LoopState::ExecutingTools {
                    calls: calls.clone(),
                    turn: *turn,
                    usage: accumulate(*prior, usage),
                },
```

**5d. No-tool-calls branch (~line 290)** — bind `prior`, compute `total`, use it in both sub-branches:

```rust
// before
        (LoopState::CallingModel { turn }, TransitionInput::ModelResponse { items, usage, .. })
            if !items.iter().any(|i| matches!(i, Item::ToolCall { .. })) =>
        {
            let mut events: Vec<AgentEvent> = items
// after
        (LoopState::CallingModel { turn, usage: prior }, TransitionInput::ModelResponse { items, usage, .. })
            if !items.iter().any(|i| matches!(i, Item::ToolCall { .. })) =>
        {
            let total = accumulate(*prior, usage);
            let mut events: Vec<AgentEvent> = items
```

```rust
// before  (~line 317, the `Some(out)` finalizing branch)
                    TransitionOutcome {
                        next_state: LoopState::Finalizing {
                            turn: finalizing_turn,
                        },
// after
                    TransitionOutcome {
                        next_state: LoopState::Finalizing {
                            turn: finalizing_turn,
                            usage: total,
                        },
```

```rust
// before  (~line 326, the `None` terminal branch)
                    let content = last_assistant_content(&items);
                    events.push(AgentEvent::RunCompleted { usage });
                    TransitionOutcome {
                        next_state: LoopState::Done(FinalOutput { content, usage }),
// after
                    let content = last_assistant_content(&items);
                    events.push(AgentEvent::RunCompleted { usage: total });
                    TransitionOutcome {
                        next_state: LoopState::Done(FinalOutput { content, usage: total }),
```

**5e. ToolResults arm (~line 338)** — carry forward unchanged (tools add no tokens):

```rust
// before
        (LoopState::ExecutingTools { turn, .. }, TransitionInput::ToolResults { outcomes }) => {
// after
        (LoopState::ExecutingTools { turn, usage: prior, .. }, TransitionInput::ToolResults { outcomes }) => {
```

```rust
// before  (~line 369)
            TransitionOutcome {
                next_state: LoopState::CallingModel { turn: next_turn },
// after
            TransitionOutcome {
                next_state: LoopState::CallingModel { turn: next_turn, usage: *prior },
```

(The max-turns sub-branch inside this arm returns `Failed` — no usage change.)

**5f. Finalizing arm (~line 376)** — bind `prior`, compute `total` after the output-type guard:

```rust
// before
        (LoopState::Finalizing { turn }, TransitionInput::ModelResponse { items, usage, .. }) => {
            let Some(out) = ctx.output else {
// after
        (LoopState::Finalizing { turn, usage: prior }, TransitionInput::ModelResponse { items, usage, .. }) => {
            let Some(out) = ctx.output else {
```

Then immediately after the `};` that closes that `let Some(out) = ... else { ... };` guard, add:

```rust
            let total = accumulate(*prior, usage);
```

```rust
// before  (~line 407, the `Ok(())` branch)
                Ok(()) => {
                    events.push(AgentEvent::RunCompleted { usage });
                    TransitionOutcome {
                        next_state: LoopState::Done(FinalOutput { content, usage }),
// after
                Ok(()) => {
                    events.push(AgentEvent::RunCompleted { usage: total });
                    TransitionOutcome {
                        next_state: LoopState::Done(FinalOutput { content, usage: total }),
```

```rust
// before  (~line 427, the `Err(schema_errors)` branch)
                    TransitionOutcome {
                        next_state: LoopState::RepairingOutput { turn: *turn },
// after
                    TransitionOutcome {
                        next_state: LoopState::RepairingOutput { turn: *turn, usage: total },
```

**5g. RepairingOutput arm (~line 436)** — bind `prior`, compute `total` after the output-type guard:

```rust
// before
        (
            LoopState::RepairingOutput { .. },
            TransitionInput::ModelResponse { items, usage, .. },
        ) => {
            let Some(out) = ctx.output else {
// after
        (
            LoopState::RepairingOutput { usage: prior, .. },
            TransitionInput::ModelResponse { items, usage, .. },
        ) => {
            let Some(out) = ctx.output else {
```

Then immediately after that arm's `let Some(out) = ... else { ... };` guard, add:

```rust
            let total = accumulate(*prior, usage);
```

```rust
// before  (~line 468, the `Ok(())` branch)
                Ok(()) => {
                    events.push(AgentEvent::RunCompleted { usage });
                    TransitionOutcome {
                        next_state: LoopState::Done(FinalOutput { content, usage }),
// after
                Ok(()) => {
                    events.push(AgentEvent::RunCompleted { usage: total });
                    TransitionOutcome {
                        next_state: LoopState::Done(FinalOutput { content, usage: total }),
```

(The `Err` branch builds `Failed` — no usage change.)

- [ ] **Step 6: Update the driver in `agent.rs`**

**6a. Seed the initial state (line 620):**

```rust
// before
            let mut loop_state = crate::LoopState::CallingModel { turn: 0 };
// after
            let mut loop_state = crate::LoopState::CallingModel { turn: 0, usage: crate::TokenUsage::default() };
```

**6b. Delete the two i64 span counters (lines 649-650):**

```rust
// DELETE these two lines:
            let mut run_input_tokens: i64 = 0;
            let mut run_output_tokens: i64 = 0;
```

**6c. Source the run span from the cumulative event (lines 689-692):**

```rust
// before
                        crate::AgentEvent::RunCompleted { .. } => {
                            run_span.record("gen_ai.usage.input_tokens", run_input_tokens);
                            run_span.record("gen_ai.usage.output_tokens", run_output_tokens);
                        }
// after
                        crate::AgentEvent::RunCompleted { usage } => {
                            run_span.record("gen_ai.usage.input_tokens", usage.input_tokens as i64);
                            run_span.record("gen_ai.usage.output_tokens", usage.output_tokens as i64);
                        }
```

**6d. Remove the per-turn run-total accumulation + refresh the comment (lines 811-819):**

```rust
// before
                        let usage = latest_usage.unwrap_or_default();
                        // Record per-turn usage from the FINAL retained Usage snapshot.
                        // Providers such as Anthropic emit incremental Usage updates; the
                        // Model contract says retain the LAST, not sum within a turn.
                        // Run totals then accumulate each turn's final usage across turns.
                        chat_span.record("gen_ai.usage.input_tokens", usage.input_tokens as i64);
                        chat_span.record("gen_ai.usage.output_tokens", usage.output_tokens as i64);
                        run_input_tokens += usage.input_tokens as i64;
                        run_output_tokens += usage.output_tokens as i64;
                        tx_input = crate::TransitionInput::ModelResponse {
// after
                        let usage = latest_usage.unwrap_or_default();
                        // Per-turn chat span records the FINAL retained Usage snapshot
                        // (Anthropic emits incremental updates; retain the LAST, never sum
                        // within a turn). Cross-turn run totals now accumulate inside the
                        // state machine (SMA-402) and arrive on RunCompleted.usage.
                        chat_span.record("gen_ai.usage.input_tokens", usage.input_tokens as i64);
                        chat_span.record("gen_ai.usage.output_tokens", usage.output_tokens as i64);
                        tx_input = crate::TransitionInput::ModelResponse {
```

- [ ] **Step 7: Fix the existing pure tests so the test crate compiles**

In `crates/paigasus-helikon-core/tests/transition_unit.rs`:

Constructions — replace **all** occurrences of the bare `CallingModel { turn: 0 }` constructor (5 sites: lines 46, 66, 93, 349, 394):

```rust
// before
    let state = LoopState::CallingModel { turn: 0 };
// after
    let state = LoopState::CallingModel { turn: 0, usage: TokenUsage::default() };
```

Line 188 (`turn: max_turns`):

```rust
// before
    let state = LoopState::CallingModel { turn: max_turns };
// after
    let state = LoopState::CallingModel { turn: max_turns, usage: TokenUsage::default() };
```

The two `ExecutingTools` constructions (lines 152-155 and 289-292) — add the field:

```rust
// before
    let state = LoopState::ExecutingTools {
        calls: calls.clone(),
        turn: 0,
    };
// after
    let state = LoopState::ExecutingTools {
        calls: calls.clone(),
        turn: 0,
        usage: TokenUsage::default(),
    };
```

```rust
// before
    let state = LoopState::ExecutingTools {
        calls: calls.clone(),
        turn: max_turns - 1,
    };
// after
    let state = LoopState::ExecutingTools {
        calls: calls.clone(),
        turn: max_turns - 1,
        usage: TokenUsage::default(),
    };
```

Patterns — add `..` to the three exhaustive field-matches (lines 58, 121, 176):

```rust
// line 58 before:  assert_matches!(outcome.next_state, LoopState::CallingModel { turn: 0 });
// line 58 after:   assert_matches!(outcome.next_state, LoopState::CallingModel { turn: 0, .. });

// line 121 before: LoopState::ExecutingTools { ref calls, turn } => {
// line 121 after:  LoopState::ExecutingTools { ref calls, turn, .. } => {

// line 176 before: assert_matches!(outcome.next_state, LoopState::CallingModel { turn: 1 });
// line 176 after:  assert_matches!(outcome.next_state, LoopState::CallingModel { turn: 1, .. });
```

- [ ] **Step 8: Build and run the regression test (GREEN)**

Run: `cargo test -p paigasus-helikon-core --test loop_happy_path multi_turn_usage_is_cumulative`
Expected: PASS. If you get `E0027` (pattern missing field) or `E0063` (missing field in initializer) elsewhere, add `usage: TokenUsage::default()` to that construction or `..` to that pattern and re-run.

- [ ] **Step 9: Run the full core test suite for regressions**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS (existing tests script no `Usage` events, so their cumulative usage is `0` — assertions on event kinds/shapes are unchanged).

- [ ] **Step 10: Format and lint**

Run: `cargo fmt --all` then `cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings`
Expected: clean. (Watch for `clippy::needless_return` or an unused-variable warning if any `usage`/`prior` binding ends up unused — adjust to `..` if so.)

- [ ] **Step 11: Commit**

```bash
git add crates/paigasus-helikon-core/src/loop_state.rs \
        crates/paigasus-helikon-core/src/agent.rs \
        crates/paigasus-helikon-core/tests/loop_happy_path.rs \
        crates/paigasus-helikon-core/tests/transition_unit.rs
git commit -m "fix(core): SMA-402 accumulate token usage across turns in LoopState

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: Lock the cross-turn accumulation in the pure state machine

**Files:**
- Test: `crates/paigasus-helikon-core/tests/transition_unit.rs`

- [ ] **Step 1: Add the pure multi-step accumulation test**

Append to `crates/paigasus-helikon-core/tests/transition_unit.rs` (all imports already present at the top of the file):

```rust
/// SMA-402: the running usage total is carried forward and summed across
/// turns by `transition`, surfacing the cumulative total on `Done` /
/// `RunCompleted` — not the last turn only.
#[test]
fn usage_accumulates_across_turns() {
    // TokenUsage is #[non_exhaustive]: build via default + field assignment.
    let mut u0 = TokenUsage::default();
    u0.input_tokens = 100;
    u0.output_tokens = 20;
    u0.total_tokens = 120;

    let mut u1 = TokenUsage::default();
    u1.input_tokens = 200;
    u1.output_tokens = 8;
    u1.total_tokens = 208;

    let settings = ModelSettings::new();
    let conversation: Vec<Item> = vec![];

    // Turn 0: model emits a tool call (usage u0) → ExecutingTools carries u0.
    let assistant = Item::AssistantMessage {
        content: vec![ContentPart::Text {
            text: "calling".into(),
        }],
        agent: Some("test".into()),
    };
    let call = Item::ToolCall {
        call_id: "1".into(),
        name: "a".into(),
        args: serde_json::json!({}),
    };
    let state0 = LoopState::CallingModel {
        turn: 0,
        usage: TokenUsage::default(),
    };
    let out0 = transition(
        &state0,
        TransitionInput::ModelResponse {
            items: vec![assistant, call],
            usage: u0,
            finish_reason: FinishReason::ToolCalls,
        },
        &ctx_with(16, &conversation, &settings),
    );
    let exec_usage = match &out0.next_state {
        LoopState::ExecutingTools { usage, .. } => *usage,
        other => panic!("expected ExecutingTools, got {other:?}"),
    };
    assert_eq!(exec_usage.input_tokens, 100);
    assert_eq!(exec_usage.total_tokens, 120);

    // Tool results → CallingModel { turn: 1 } carries u0 forward unchanged.
    let out1 = transition(
        &out0.next_state,
        TransitionInput::ToolResults {
            outcomes: vec![ToolCallOutcome {
                call_id: "1".into(),
                result: Ok(vec![ContentPart::Text { text: "ok".into() }]),
            }],
        },
        &ctx_with(16, &conversation, &settings),
    );
    let call1_usage = match &out1.next_state {
        LoopState::CallingModel { turn: 1, usage } => *usage,
        other => panic!("expected CallingModel turn 1, got {other:?}"),
    };
    assert_eq!(call1_usage.input_tokens, 100, "tools add no tokens");

    // Turn 1: final text (usage u1) → Done with cumulative u0 + u1.
    let final_assistant = Item::AssistantMessage {
        content: vec![ContentPart::Text {
            text: "done".into(),
        }],
        agent: Some("test".into()),
    };
    let out2 = transition(
        &out1.next_state,
        TransitionInput::ModelResponse {
            items: vec![final_assistant],
            usage: u1,
            finish_reason: FinishReason::Stop,
        },
        &ctx_with(16, &conversation, &settings),
    );
    let final_usage = match &out2.next_state {
        LoopState::Done(fo) => fo.usage,
        other => panic!("expected Done, got {other:?}"),
    };
    assert_eq!(final_usage.input_tokens, 300);
    assert_eq!(final_usage.output_tokens, 28);
    assert_eq!(final_usage.total_tokens, 328);

    // The RunCompleted event carries the same cumulative total.
    match out2
        .events
        .iter()
        .find(|e| matches!(e, AgentEvent::RunCompleted { .. }))
    {
        Some(AgentEvent::RunCompleted { usage }) => {
            assert_eq!(usage.input_tokens, 300);
            assert_eq!(usage.total_tokens, 328);
        }
        _ => panic!("expected a RunCompleted event"),
    }
}
```

- [ ] **Step 2: Run the test (expect PASS)**

Run: `cargo test -p paigasus-helikon-core --test transition_unit usage_accumulates_across_turns`
Expected: PASS (this locks the Task 1 mechanism at the state-machine level).

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-core/tests/transition_unit.rs
git commit -m "test(core): SMA-402 lock cross-turn usage accumulation in transition

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: Cover cumulative usage on a structured-output run

**Files:**
- Test: `crates/paigasus-helikon-core/tests/structured_output.rs`

- [ ] **Step 1: Add the structured multi-turn usage test**

Append to `crates/paigasus-helikon-core/tests/structured_output.rs` (imports already present):

```rust
/// SMA-402: a structured run spans the unconstrained turn(s) + the constrained
/// finalizing turn; usage must sum across all of them, including the finalizing
/// turn. (Three turns here: tool call → unconstrained text → finalizing JSON.)
#[tokio::test]
async fn structured_run_usage_is_cumulative() {
    use common::MockTool;

    let tool = MockTool::new("fetch_panel", serde_json::json!({"blasts": 80}));
    let model = MockModel::with_scripts(vec![
        // turn 0: call the tool (usage)
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "c1".into(),
                name: Some("fetch_panel".into()),
                args_delta: "{}".into(),
            },
            ModelEvent::Usage {
                input_tokens: 50,
                output_tokens: 10,
                cached_input_tokens: Some(5),
                reasoning_tokens: Some(2),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        // turn 1: unconstrained free-text answer (usage)
        vec![
            ModelEvent::TokenDelta {
                text: "Based on the panel, AML.".into(),
            },
            ModelEvent::Usage {
                input_tokens: 60,
                output_tokens: 12,
                cached_input_tokens: Some(0),
                reasoning_tokens: Some(4),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
        // turn 2: constrained finalizing turn → structured JSON (usage)
        vec![
            ModelEvent::TokenDelta {
                text: "{\"subtype\":\"AML\",\"confidence\":88}".into(),
            },
            ModelEvent::Usage {
                input_tokens: 70,
                output_tokens: 6,
                cached_input_tokens: Some(0),
                reasoning_tokens: Some(0),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]);

    let agent = LlmAgent::builder::<()>()
        .name("classifier")
        .shared_model(model)
        .instructions("Classify the sample.")
        .shared_tool(tool.clone())
        .output_type::<LeukemiaSubtypeAnalysis>()
        .build();

    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("sample"))
        .await
        .expect("run starts");
    let result = RunResultStreaming::new(stream)
        .collect_typed::<LeukemiaSubtypeAnalysis>()
        .await
        .expect("collect_typed succeeds");

    // Sums: input 50+60+70=180, output 10+12+6=28, cached 5+0+0=5,
    // reasoning 2+4+0=6, total 60+72+76=208.
    assert_eq!(result.usage.input_tokens, 180);
    assert_eq!(result.usage.output_tokens, 28);
    assert_eq!(result.usage.cached_input_tokens, 5);
    assert_eq!(result.usage.reasoning_tokens, 6);
    assert_eq!(result.usage.total_tokens, 208);
}
```

- [ ] **Step 2: Run the test (expect PASS)**

Run: `cargo test -p paigasus-helikon-core --test structured_output structured_run_usage_is_cumulative`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-core/tests/structured_output.rs
git commit -m "test(core): SMA-402 cover cumulative usage on structured run

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 4: Codify the last-wins `Usage` contract in `Model` docs

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs:55-60`, `:188-194`

- [ ] **Step 1: Update the `Model::invoke` event-ordering contract (lines 55-60)**

```rust
// before
    /// **Event-ordering contract:**
    /// - `TokenDelta`, `ReasoningDelta`, and `ToolCallDelta` may interleave
    ///   freely while the model is generating.
    /// - `Usage` MAY appear anywhere; most providers emit one immediately
    ///   before `Finish` but Anthropic emits incremental updates.
    /// - `Finish` is the terminal event; nothing follows it.
// after
    /// **Event-ordering contract:**
    /// - `TokenDelta`, `ReasoningDelta`, and `ToolCallDelta` may interleave
    ///   freely while the model is generating.
    /// - `Usage` MAY appear anywhere; most providers emit one immediately
    ///   before `Finish`, while Anthropic emits cumulative-within-response
    ///   updates. Each `Usage` is a complete snapshot (last-wins): consumers
    ///   retain the last seen and never sum `Usage` events within a turn.
    ///   See [`ModelEvent::Usage`].
    /// - `Finish` is the terminal event; nothing follows it.
```

- [ ] **Step 2: Update the `ModelEvent::Usage` doc (lines 188-194)**

```rust
// before
    /// Token-usage snapshot emitted by the provider.
    ///
    /// **Ordering contract** (per [`Model::invoke`] docs): a `Usage` MAY
    /// appear anywhere in the stream. `Finish` is always terminal.
    /// OpenAI emits one `Usage` immediately before `Finish`; Anthropic
    /// emits incremental usage updates. Consumers tracking final
    /// totals should retain the last `Usage` seen.
// after
    /// Token-usage snapshot emitted by the provider.
    ///
    /// **Ordering contract** (per [`Model::invoke`] docs): a `Usage` MAY
    /// appear anywhere in the stream. `Finish` is always terminal.
    /// OpenAI emits one `Usage` immediately before `Finish`; Anthropic emits
    /// updates that are **cumulative within the response** (each carries the
    /// running total, not a per-chunk delta).
    ///
    /// **Last-wins contract:** each `Usage` is a complete snapshot, so a
    /// consumer tracking a turn's total retains the **last** `Usage` seen and
    /// never sums `Usage` events *within* a turn. The agent loop then sums these
    /// per-turn finals **across** turns for the run total (SMA-402); a provider
    /// emitting true per-chunk deltas would violate this and under-count, so
    /// implementations MUST emit cumulative-within-turn usage.
```

- [ ] **Step 3: Verify docs build with warnings denied**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps`
Expected: builds clean (no broken intra-doc links from the new `[\`ModelEvent::Usage\`]` reference).

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/model.rs
git commit -m "docs(core): SMA-402 codify last-wins Usage contract on Model

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 5: Full CI-parity gate + OTel regression check

**Files:** none (verification only)

- [ ] **Step 1: Confirm the OTel run-span tests still pass (the regression gate)**

Run: `cargo test -p paigasus-helikon --all-features --test otel_spans`
Expected: PASS. The run span now reads the cumulative `RunCompleted.usage`, which equals the old per-turn sum — values are unchanged. (Pay attention to `run_span_usage_is_last_seen_not_summed_within_a_turn` and the multi-turn span test.)

- [ ] **Step 2: Run the full CI gate set locally (mirrors `.github/workflows/ci.yml`)**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```
Expected: all green. If `cargo fmt --all -- --check` reports diffs, run `cargo fmt --all`, then `git add -A && git commit -m "style(core): SMA-402 cargo fmt"` (only if there is anything to commit).

- [ ] **Step 3: Sanity-check the diff has no stray version edits**

Run: `git diff main --stat` and confirm **no** edits to any `Cargo.toml` `version` field or `Cargo.lock` (release-plz owns the bump — see Task 6).
Expected: only the six files from Tasks 1-4 plus the two spec/plan docs.

---

## Task 6: Open the PR with the breaking marker, verify the release bump

**Files:** none (PR + release verification)

- [ ] **Step 1: Push the branch**

```bash
git push -u origin feature/sma-402-runresultusage-reports-last-turn-token-usage-not-cumulative
```

- [ ] **Step 2: Open the PR with a breaking, conventional title**

The squashed-commit title is what release-plz parses, so it MUST carry the breaking `!`:

```bash
gh pr create \
  --title "fix(core)!: SMA-402 report cumulative token usage across all turns" \
  --body "Closes SMA-402. Carries the running token-usage total inside LoopState so RunResult.usage / RunCompleted.usage / FinalOutput.usage report the cumulative total across all turns; collapses the OTel run-span counters into the same cumulative value. BREAKING CHANGE: adds a \`usage\` field to four LoopState struct variants (external exhaustive matches must add \`..\`). See docs/superpowers/specs/2026-05-31-sma-402-cumulative-usage-design.md.

🤖 Generated with [Claude Code](https://claude.com/claude-code)"
```

Title rules to satisfy (verify before submit): full `type(scope):` prefix present (`fix(core)!:`), and the subject after `SMA-402 ` starts lowercase (`report …`) — both required by `pr-title.yml`.

- [ ] **Step 3: Confirm all required checks report and pass**

After CI runs, verify each required context has *reported* (not just that visible ones are green): `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`. Cross-reference `.github/rulesets/main-protection-checks.json` for the canonical list.

- [ ] **Step 4: After merge, verify release-plz proposes the minor bump**

On the release-plz PR, confirm it proposes `paigasus-helikon-core` `0.2.4 → 0.3.0` (breaking → minor on 0.x) and that its `dependencies_update` cascade bumps the facade `paigasus-helikon` (patch) with the refreshed `paigasus-helikon-core` dep req. If the facade is **not** bumped, follow `CLAUDE.md`'s facade-bump guidance. Do not hand-edit versions before this.

---

## Self-Review (completed during planning)

- **Spec coverage:** §3.1 → Task 1 steps 3-5; §3.2 → Task 1 step 6; §3.3 (no change) → covered by Task 1 step 9 + Task 5; §3.4 docs (incl. L2) → Task 1 (field docs) + Task 4; §3.5 (no variant `#[non_exhaustive]`) → reflected in Task 1 step 7 keeping construction working; §4 tests (pure, e2e with non-zero cached/reasoning, structured, otel gate) → Tasks 1-3, 5; §5 release → Task 6. No gaps.
- **Placeholder scan:** none — every code step carries complete code; every command has expected output.
- **Type consistency:** `accumulate(prior, turn) -> TokenUsage`, the `usage` field name, and the `usage: total` / `usage: *prior` constructions are consistent across all arms; assertions read public fields (no `TokenUsage`/`FinalOutput` struct literals in external test crates).
