# SMA-804 Turn-Boundary Checkpoint Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an optional `TurnCheckpoint` callback on `RunContext`. The `LlmAgent` loop calls it between turns, and it can continue the run, inject user content, or stop the run cleanly.

**Architecture:** A new `checkpoint` module in `paigasus-helikon-core` defines the trait and its types. The `LlmAgent::run` driver (`core/src/agent.rs`) detects the boundary batch from `transition()`, which is `ExecutingTools` followed by `CallModel`. It calls the callback before it handles `TurnStarted { n }`. `transition()` does not change. Two new `AgentEvent` variants carry the result: `UserMessageInjected` (not terminal) and `RunStopped` (terminal). Phase B connects `RunStopped` to SMA-803's `StopReason`.

**Tech Stack:** Rust 1.94, `async_trait`, `async_stream`, `tokio`, `insta` snapshots, `tracing`.

**Spec:** `docs/superpowers/specs/2026-11-18-sma-804-turn-boundary-checkpoint-design.md`. Read it with this plan. The section numbers below (§5.3 and others) refer to it.

## Global Constraints

- Work in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-804-turn-boundary-checkpoint`, on the branch `feature/sma-804-turn-boundary-checkpoint-a-runcontext`. Use worktree-absolute paths for every Write and Edit.
- Do not run `git checkout`, `git switch`, `git reset`, `git rebase` or `git stash` unless a task tells you to. Another session (SMA-803) works in a sibling worktree.
- Run every cargo command in the foreground, and wait for it to finish. Do not end your turn while a command runs.
- Commit format: `<type>(<scope>): SMA-804 <lowercase verb> …`, with the trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Use the scopes `core`, `runtime`, `docs`. Run `cargo fmt --all` before each commit.
- **No hand version bumps.** Every touched crate is already released, so release-plz bumps core and the dependents in one release PR (pure-auto, proven in SMA-418 / PR #80). Do not edit any `version =` or CHANGELOG.
- All new public types are `#[non_exhaustive]`. No existing variant gets a new field (G6).
- Every new public item has a `///` doc. Public docs must not intra-doc-link to `pub(crate)` items (`Interceptors`, `user_text_of`), or the `docs` gate fails.
- New insta snapshots are committed as `.snap` files. Do not leave `.snap.new` files.
- Tracing targets follow the `paigasus::core::agent` pattern. The `tests/workspace-lints` tracing-target check must pass.

## Review Focus

These failure modes follow from the spec, but no spec test covers them. Each one has a test in the task that owns the code:

1. **Parallel tool calls in one turn.** The checkpoint runs exactly once, after *both* results (Task 3, `checkpoint_runs_once_after_parallel_results`).
2. **An `Inject` with only an image part.** The guardrails get `""` and pass, and the image is recorded and sent (Task 3, `inject_image_only_passes_guardrails_and_is_sent`).
3. **Several boundaries in one run.** An `Inject` at boundary 1 and a `Stop` at boundary 2. The second request holds the first injection, the second `TurnBoundary` has `completed_turn = 1`, and `usage` covers three model calls (Task 4, `inject_then_stop_across_two_boundaries`).
4. **A run with no tool calls.** The checkpoint is never called (Task 3, `checkpoint_not_called_without_tool_calls`).
5. **An `Inject` inside a handoff child.** The recorder stores it after the child's `ToolReturned`, and the parent's history order stays valid (Task 4, `handoff_child_inject_is_recorded_in_order`).

---

## Phase A: independent of SMA-803

### Task 1: Checkpoint types and the `RunContext` field

**Files:**
- Create: `crates/paigasus-helikon-core/src/checkpoint.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs` (module + re-exports)
- Modify: `crates/paigasus-helikon-core/src/context.rs` (field ~`:55-117`, `new` `:101`, `handoff_child` `:291`, `subagent_child` `:320`, builders near `with_approval_handler` `:387`, `runcontext_tests` `:~660+`)
- Test: `crates/paigasus-helikon-core/tests/object_safety.rs`

**Interfaces:**
- Produces (crate root re-exports):
  - `pub struct TurnBoundary { pub completed_turn: u32, pub next_turn: u32, pub agent: String, pub depth: u32 }` (`#[non_exhaustive]`, `Debug, Clone, PartialEq, Eq`)
  - `TurnBoundary::new(completed_turn: u32, next_turn: u32, agent: impl Into<String>, depth: u32) -> Self`
  - `pub enum CheckpointDecision { Continue, Inject(Vec<ContentPart>), Stop { reason: String } }` (`#[non_exhaustive]`, `Debug, Clone, PartialEq`)
  - `#[async_trait] pub trait TurnCheckpoint<Ctx>: Send + Sync where Ctx: Send + Sync + 'static { async fn at_boundary(&self, ctx: &RunContext<Ctx>, boundary: &TurnBoundary) -> CheckpointDecision; }`
  - `RunContext::with_turn_checkpoint(self, cp: Arc<dyn TurnCheckpoint<Ctx>>) -> Self`, `RunContext::without_turn_checkpoint(self) -> Self`, `RunContext::turn_checkpoint(&self) -> Option<&Arc<dyn TurnCheckpoint<Ctx>>>`

- [ ] **Step 1: Write the failing tests.**
  - In `context.rs` `runcontext_tests`:
    - `turn_checkpoint_defaults_to_none`: `RunContext::ephemeral(())` has `turn_checkpoint().is_none()`.
    - `with_and_without_turn_checkpoint`: after `with_turn_checkpoint(cp)`, the getter returns an `Arc` that is `Arc::ptr_eq` to `cp`. After `without_turn_checkpoint()`, it is `None`.
    - `handoff_child_shares_turn_checkpoint`: `Arc::ptr_eq` holds on `handoff_child()`.
    - `subagent_child_drops_turn_checkpoint`: `subagent_child().turn_checkpoint().is_none()`.
  - In `tests/object_safety.rs`: `TurnCheckpoint<()>` is object-safe. Follow the file's pattern for `PermissionPolicy<()>`.
- [ ] **Step 2: Run the tests and confirm they fail.** Command: `cargo test -p paigasus-helikon-core --lib runcontext_tests && cargo test -p paigasus-helikon-core --test object_safety`. Expected: compile errors (the unknown items).
- [ ] **Step 3: Implement the types.** Put them in `checkpoint.rs`, using the same `#[async_trait]` form as `PermissionPolicy<Ctx>` (`permission.rs:60`). The trait doc holds the seven rules of spec §5.1, plus the guardrail-stop signal of §5.4 and the image/audio limit. Add the private field `turn_checkpoint: Option<Arc<dyn TurnCheckpoint<Ctx>>>`, set to `None` in `new`. Copy it in `handoff_child`, set it to `None` in `subagent_child`, and add the builders and the getter. Do not carry it in `to_tool_context`.
- [ ] **Step 4: Run the tests and confirm they pass.** Use the commands from Step 2. Also run `cargo test -p paigasus-helikon-core --doc` to check that the `RunContext` doctest still compiles.
- [ ] **Step 5: Commit.** Message: `feat(core): SMA-804 add the turn checkpoint trait and runcontext field`.

### Task 2: The two new events and the recorder arm

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (`AgentEvent` `:356`, the "Seventeen variants" doc `:348`, `is_terminal` `:498`, `terminal_tests` `:1461-1600`)
- Modify: `crates/paigasus-helikon-core/src/session.rs` (`SessionRecorder::observe` `:~460`, `recorder_tests` `:~620`)
- Test: `crates/paigasus-helikon-core/tests/serde_roundtrip.rs` and new snapshots in `tests/snapshots/`
- Test: `crates/paigasus-helikon-core/tests/transition_unit.rs`

**Interfaces:**
- Produces: `AgentEvent::UserMessageInjected { content: Vec<ContentPart> }` and `AgentEvent::RunStopped { reason: String, usage: TokenUsage }`. Their serde tags are `user_message_injected` and `run_stopped`. `is_terminal()` is true for `RunStopped` and false for `UserMessageInjected`.

- [ ] **Step 1: Write the failing tests.**
  - `terminal_tests`: add both variants to `classify` (no wildcard) and to `every_variant()`. Increase the variant count by 2. The terminal-count test expects one more terminal than the count on the base branch.
  - `serde_roundtrip.rs`: add `agent_event_user_message_injected` and `agent_event_run_stopped`. Each round-trips one value and checks an insta snapshot.
  - `recorder_tests`: `observe_user_message_injected_records_user_message`. After a `ToolCallItem`, a `ToolOutputItem` and then a `UserMessageInjected { [Text "hi"] }`, `drain()` returns `[ToolCalled, ToolReturned, UserMessage { [Text "hi"] }]`. Then `project()` of that log ends with `Item::UserMessage { [Text "hi"] }`.
  - `transition_unit.rs`: `tool_results_batch_ends_with_turn_started`. From `ExecutingTools { turn: 0 }` with 2 outcomes and `max_turns = 5`, the events are `[ToolOutputItem, ToolOutputItem, TurnStarted { turn: 1 }]` in this order, and `next_action` is `CallModel`.
- [ ] **Step 2: Run the tests and confirm they fail.** Command: `cargo test -p paigasus-helikon-core --lib terminal_tests && cargo test -p paigasus-helikon-core --lib recorder_tests && cargo test -p paigasus-helikon-core --test serde_roundtrip --test transition_unit`. Expected: compile errors for the new variants. The `transition_unit` test passes already. It locks the existing order.
- [ ] **Step 3: Implement.** Add the variants and docs (state the commit rule on `UserMessageInjected`), the `is_terminal` arm and its doc, and the "N variants" doc. In `observe`, map `UserMessageInjected { content }` to `SessionEvent::user_message(content.clone())`. `RunStopped` needs no recorder arm. Make every other exhaustive match in core compile, and fix the other workspace crates only to the point where they compile (Task 7 adds their real arms). Accept the snapshots with `cargo insta accept` if it is installed. Otherwise, rename each reviewed `.snap.new` to `.snap`.
- [ ] **Step 4: Run the tests and confirm they pass.** Use the commands from Step 2, plus `cargo build --workspace --all-features`.
- [ ] **Step 5: Commit.** Message: `feat(core): SMA-804 add user-message-injected and run-stopped events`.

### Task 3: The boundary hook in the driver: `Continue` and `Inject`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (the event loop `:~857-905`, the `next_action` binding, and `AgentError` `:1157`)
- Create: `crates/paigasus-helikon-core/tests/turn_checkpoint.rs` (with `#[path = "common/mod.rs"] mod common;`)

**Interfaces:**
- Consumes: Task 1 types, Task 2 events, `Interceptors::run_input_guardrails(&self, text: &str) -> Option<(GuardrailKind, Value)>` (`control.rs:69`).
- Produces: `AgentError::InvalidInjection { reason: String }` (the error text names the bad part kind). It also produces the test helper `ScriptedCheckpoint` in `tests/turn_checkpoint.rs`, which Tasks 4 and 5 extend. `ScriptedCheckpoint::new(Vec<CheckpointDecision>) -> Arc<Self>` pops one decision per call (`Continue` when it is empty). `ScriptedCheckpoint::boundaries(&self) -> Vec<TurnBoundary>` returns the recorded boundaries.

**Mechanism (spec §5.2, decided).** In the `for ev in events` loop, the driver handles `TurnStarted { turn }`. The batch is a boundary batch if both of these are true:
- `loop_state` (still the state before the transition) is `ExecutingTools { turn: t, .. }`,
- `next_action` is `CallModel`.

Then, if `ctx.turn_checkpoint()` is `Some`, the driver runs the following steps before the span and the `OnTurnStart` code:
1. Build `TurnBoundary::new(t, *turn, agent_name.clone(), ctx.agent_depth())`.
2. Await `at_boundary` in an `agent.turn_checkpoint` span (child of `run_span`). The span records a `decision` attribute (`continue` / `inject` / `stop`).
3. Act on the decision:
   - `Continue`: proceed.
   - `Inject(parts)` with `parts.is_empty()`: proceed, as `Continue`.
   - `Inject(parts)` with a `ToolUse`, `ToolResult` or `Reasoning` part: `failure.set(AgentError::InvalidInjection { .. })`, `run_span` `otel.status_code = ERROR`, yield `RunFailed { error }`, `return`.
   - `Inject(parts)` that is valid: join the `Text` parts (the same rule as `user_text_of`), then call `run_input_guardrails`. A tripwire goes to the Task 4 path. On pass: yield `UserMessageInjected { content: parts.clone() }`, push `Item::UserMessage { content: parts.clone() }` to `conversation`, and push the same item to the `request.messages` of `next_action` (bind it as `let mut`).
   - `Stop`, and a guardrail tripwire: in this task, both go to one internal stop path. For now, that path fails the run with the existing `AgentError::NotImplemented { feature: "turn checkpoint stop" }`: it sets `failure`, yields `RunFailed` and returns. Task 4 replaces the body of this path. Do not use `todo!()` or `unimplemented!()`.

- [ ] **Step 1: Write the failing tests** in `tests/turn_checkpoint.rs`. Use `MockModel::with_scripts`, `MockTool::new`, `noop_run_context`, and a `RecordingModel` that captures each `ModelRequest.messages` (copy the pattern from `injection.rs`). The scripts: turn 0 makes one tool call (`ModelEvent::ToolCallDelta` + `Finish { ToolCalls }`), and turn 1 gives the final text.
  - `continue_matches_no_checkpoint_stream`: the event kinds with `[Continue]` are equal to the event kinds with no checkpoint. `boundaries() == [TurnBoundary::new(0, 1, "agent", 0)]`.
  - `inject_reaches_next_request_after_tool_result`: with `[Inject([Text "news"])]`, request #2 ends with `[…, ToolResult, UserMessage([Text "news"])]`. The events contain exactly `ToolOutputItem`, `UserMessageInjected`, `TurnStarted { 1 }` as consecutive kinds.
  - `inject_empty_is_continue`: `[Inject(vec![])]` gives no `UserMessageInjected`, and request #2 has no extra item.
  - `inject_tool_part_fails_before_any_record`: `[Inject([ToolResult { .. }])]`. The last event is `RunFailed`. There is no `UserMessageInjected`. `RunResultStreaming::with_failure` gives `AgentError::InvalidInjection`. The model was called once.
  - `checkpoint_not_called_at_max_turns`: with `RunConfig` `max_turns = 1`, `boundaries()` is empty and the error is `MaxTurnsExceeded(1)`.
  - `checkpoint_not_called_for_finalizing_turn`: an agent with `output_type` and one tool turn. `boundaries().len() == 1`.
  - `inject_then_on_turn_start_deny_keeps_commit`: a hook denies `OnTurnStart` for turn 1. The events contain `UserMessageInjected` before `RunFailed`, and the error is `HookDenied`.
  - Review Focus 1, `checkpoint_runs_once_after_parallel_results`: turn 0 has 2 tool calls. `boundaries().len() == 1`, and both `ToolOutputItem` events come before `UserMessageInjected`.
  - Review Focus 2, `inject_image_only_passes_guardrails_and_is_sent`: an input guardrail that records its text input. With `[Inject([Image { Url }])]`, the guardrail got `""` for the injection, and request #2 ends with the image message.
  - Review Focus 4, `checkpoint_not_called_without_tool_calls`: a one-turn text answer gives `boundaries()` empty.
- [ ] **Step 2: Run the tests and confirm they fail.** Command: `cargo test -p paigasus-helikon-core --test turn_checkpoint`. Expected: the boundary tests fail, because the checkpoint is never called.
- [ ] **Step 3: Implement the mechanism above** and `AgentError::InvalidInjection`.
- [ ] **Step 4: Run the tests and confirm they pass.** Command: `cargo test -p paigasus-helikon-core`. Every existing core test also passes.
- [ ] **Step 5: Commit.** Message: `feat(core): SMA-804 call the turn checkpoint at the boundary and inject user content`.

### Task 4: `Stop`, the guardrail stop and handoffs

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (the boundary code from Task 3, the `Terminate` arm `:1035-1047`, the handoff re-yield `:1126-1137`)
- Test: `crates/paigasus-helikon-core/tests/turn_checkpoint.rs`

**Interfaces:**
- Consumes: Tasks 1-3.
- Produces: the run ends with `RunStopped { reason, usage }` and then `OnRunComplete`. Phase B (Task 9) maps this to `StopReason`.

**Mechanism.**
- On `Stop { reason }`, or on a guardrail tripwire for injected content:
  1. On a tripwire, first yield `GuardrailTriggered { kind, info }`, and use `reason = "input guardrail tripwire"`.
  2. Take `usage` from the `ExecutingTools { usage, .. }` state before the transition.
  3. Record `gen_ai.usage.*` on `run_span`, and record the stop reason (attribute `paigasus.stop_reason`). Do not set `otel.status_code = ERROR`.
  4. Yield `RunStopped { reason, usage }`.
  5. Set `next_action = NextAction::Terminate`, and `break` out of the event loop, so that `TurnStarted { n }` is not handled or yielded.
  6. The existing `Terminate` arm then fires `OnRunComplete`. `loop_state` is not `Failed`, so the `FailureSlot` stays empty.
- Handoff re-yield: add the arm `RunStopped { reason, usage }`. It adds `parent_usage`, records the span usage, and yields `RunStopped { reason, usage: total }`. This is the same as the `RunCompleted` arm.

- [ ] **Step 1: Write the failing tests.**
  - `stop_ends_cleanly_at_boundary`: `[Stop { "operator" }]`. The last event is `RunStopped { reason: "operator", usage }`, with `usage` equal to turn 0's usage. There is no `TurnStarted { 1 }` and no `RunFailed`. The model was called once. `with_failure(..).collect()` is `Ok`, and the slot is empty. A recording hook sees `OnRunComplete` exactly once. A `SessionRecorder` over the events gives a `ToolReturned` for every `ToolCalled`.
  - `guardrail_tripwire_on_injection_stops`: a guardrail that trips on the text "bad". With `[Inject([Text "bad"])]`, the event order ends with `GuardrailTriggered`, `RunStopped { reason: "input guardrail tripwire", .. }`. There is no `UserMessageInjected`. The model was called once, and the recorder output has no `UserMessage` with "bad".
  - `guardrail_error_on_injection_stops`: a guardrail that returns `Err` gives the same result as the previous test.
  - `handoff_child_stop_ends_whole_run`: a parent that hands off to a child. The child makes one tool turn, and then the checkpoint (inherited) returns `Stop`. The boundary is `TurnBoundary::new(0, 1, "child", 1)`. The last event is `RunStopped`, with `usage` equal to the parent's usage plus the child's usage. This test does not check the pairing of `ToolCalled` (SMA-810).
  - Review Focus 3, `inject_then_stop_across_two_boundaries`: three scripted model turns (tool, tool, text) and `[Inject([Text "a"]), Stop { "b" }]`. Request #2 holds the `UserMessage("a")`. `boundaries()[1].completed_turn == 1`. `RunStopped.usage` covers two model calls.
  - Review Focus 5, `handoff_child_inject_is_recorded_in_order`: the child gets `Inject([Text "x"])`. In the recorder output, the `UserMessage("x")` comes directly after the child's `ToolReturned`.
- [ ] **Step 2: Run the tests and confirm they fail.** Command: `cargo test -p paigasus-helikon-core --test turn_checkpoint`.
- [ ] **Step 3: Implement the mechanism above.**
- [ ] **Step 4: Run the tests and confirm they pass.** Command: `cargo test -p paigasus-helikon-core`.
- [ ] **Step 5: Commit.** Message: `feat(core): SMA-804 stop the run cleanly at a turn boundary`.

### Task 5: Scope tests (`AgentAsTool` and workflow sub-agents)

**Files:**
- Test: `crates/paigasus-helikon-core/tests/turn_checkpoint.rs`
- Test: `crates/paigasus-helikon-core/tests/subagent_propagation.rs`

**Interfaces:** Consumes: Tasks 1-4. Production code changes only if a test exposes a leak.

- [ ] **Step 1: Write the tests.**
  - `agent_as_tool_sub_run_never_calls_checkpoint`: the parent calls an `AgentAsTool` whose inner agent makes one tool call and then answers. The parent gets one more turn. `boundaries()` holds only parent boundaries: every entry has `depth == 0` and `agent == "parent"`.
  - In `subagent_propagation.rs`: `agent_as_tool_sub_context_has_no_turn_checkpoint`. Follow the existing copy-site regression pattern. Use an inner agent that captures `ctx.turn_checkpoint().is_none()` from its `run` context.
  - `sequential_workflow_children_never_call_checkpoint`: a `SequentialAgent` root with one `LlmAgent` child that uses a tool. `boundaries()` is empty.
- [ ] **Step 2: Run the tests.** Command: `cargo test -p paigasus-helikon-core --test turn_checkpoint --test subagent_propagation`. Expected: PASS, because the scope comes from Task 1. If a test fails, fix the copy site and say so in the commit body.
- [ ] **Step 3: Commit.** Message: `test(core): SMA-804 lock the turn checkpoint scope`.

### Task 6: `runtime-tokio` persistence and cancellation tests

**Files:**
- Test: create `crates/paigasus-helikon-runtime-tokio/tests/turn_checkpoint.rs`. Use `tests/common/mod.rs` (`MockModel`, `PendingModel`, `CountingSession`, `run_context_with_session`, `run_context_with_cancel`).

**Interfaces:** Consumes: Tasks 1-4. No production change is expected in `runtime-tokio`.

- [ ] **Step 1: Write the tests.**
  - `injected_message_is_persisted_after_tool_result`: run with `TokioRunner::run` and a counting or recording session. The appended events are `…ToolCalled, ToolReturned, UserMessage(injected), AssistantMessage…`.
  - `run_streamed_stop_finalizes_session`: run with `run_streamed`, then `Stop`. The stream ends with `RunStopped`, and the session holds every tool result of turn 0.
  - `cancel_while_checkpoint_waits`: the checkpoint awaits a `Notify` that never fires, and the test cancels the token. The result is `RunError::Cancelled`. The session holds the tool results and no injected `UserMessage`.
  - `cancel_while_injection_guardrail_waits`: the checkpoint returns `Inject`, and a guardrail awaits forever. Cancel. The result is `Cancelled`, and the session has no injected `UserMessage` (the commit rule, before the commit).
- [ ] **Step 2: Run the tests.** Command: `cargo test -p paigasus-helikon-runtime-tokio --test turn_checkpoint`. Expected: PASS. If `run_streamed_stop_finalizes_session` fails, `is_terminal()` is missing `RunStopped`. Fix Task 2's arm.
- [ ] **Step 3: Commit.** Message: `test(runtime): SMA-804 cover checkpoint persistence and cancellation in tokio`.

### Task 7: Other runtimes: clear and warn, explicit arms, Temporal note

**Files:**
- Modify, to clear and warn (one private helper per crate, `fn restrict_unsupported_run_features<Ctx>(ctx: RunContext<Ctx>) -> RunContext<Ctx>`, called directly after each `state.context.build(..)`). SMA-803 creates a helper with this exact name and signature for its deferral flag. If SMA-803 is already on `main`, add the checkpoint clear inside the existing helper, and do not add a second call. If it is not, create the helper with only the checkpoint clear. At the Task 9 rebase, merge the two bodies into one helper with one call per site:
  - `crates/paigasus-helikon-runtime-axum/src/handlers/runs.rs:220` (helper in `src/context.rs`, doc on `ContextProvider` at `:71`)
  - `crates/paigasus-helikon-runtime-actix/src/handlers/runs.rs:257` (helper in `src/context.rs`, doc on `ContextProvider` at `:86`)
  - `crates/paigasus-helikon-runtime-agentcore/src/invoke.rs:165`, `ws.rs:203`, `agui/sse.rs:147`, `agui/ws.rs:267`, `a2a/rpc.rs:519` (one helper in the crate, for example in `src/error.rs` or a new `src/checkpoint.rs`)
- Modify, for explicit event arms where the event has a meaning: `runtime-agentcore/src/agui/map.rs` (`helikon.user_message_injected`, `helikon.run_stopped` CUSTOM frames), `cli/src/repl.rs` (print both events), `evals/src/evaluators/trajectory.rs`, `paigasus-helikon-mcp/src/server.rs`, and the axum/actix `dto.rs`, `event_log.rs`, `registry.rs`. In each file, decide whether the existing wildcard is correct. Add an arm only where the event changes the output. Write the result for each file in the commit body.
- Modify: `crates/paigasus-helikon-runtime-temporal/src/runner.rs:~184`. Add `tracing::warn!` when `ctx.turn_checkpoint().is_some()`, and a doc note on `TemporalRunner`.

For the checkpoint, the helper logs `tracing::warn!(target: "paigasus::runtime::<crate>", "turn checkpoint is not supported by this runtime; ignored")` and returns `ctx.without_turn_checkpoint()`. It logs only when a checkpoint was set. Match each crate's existing tracing-target style.

- [ ] **Step 1: Write the failing tests.** In each of axum, actix and agentcore, write `context_provider_checkpoint_never_fires`. A test `ContextProvider` sets a counting checkpoint, and the agent makes one tool turn over the crate's run endpoint. Use the crate's existing handler-test harness. The counter is 0, and the run status is `Completed`.
- [ ] **Step 2: Run the tests and confirm they fail.** Command: `cargo test -p paigasus-helikon-runtime-axum -p paigasus-helikon-runtime-actix -p paigasus-helikon-runtime-agentcore context_provider_checkpoint_never_fires`. Expected: FAIL (the counter is 1).
- [ ] **Step 3: Implement** the helpers, the calls, the docs, the arms and the Temporal warning.
- [ ] **Step 4: Run the tests and confirm they pass.** Commands: the command from Step 2, then `cargo test --workspace --all-features`.
- [ ] **Step 5: Commit.** Message: `feat(runtime): SMA-804 ignore the turn checkpoint in http and temporal runtimes`.

### Task 8: Provider wire check and documentation

**Files:**
- Test: the request translators of `providers-gemini` (`src/translate/request.rs:41-80`), `providers-anthropic`, `providers-openai` (chat and responses) and `providers-bedrock`. Add one unit test per translator.
- Modify: `docs/book/src/concepts/agent-loop.md`
- Check: `crates/paigasus-helikon-core/README.md`

- [ ] **Step 1: Write the translator tests.** The test `tool_result_then_user_message_request_shape` builds `[UserMessage, AssistantMessage, ToolCall, ToolResult, UserMessage([Text "news"])]` and checks the exact wire JSON. For Gemini, assert the current shape. Then compare it with the Gemini API rules for consecutive `user` contents (the Gemini API docs, or a keyless container proxy if one is available). If the API rejects it, merge the parts in the translator and state the source in the commit body. Also add `empty_text_part_is_dropped_or_rejected` for the Anthropic translator. If neither happens, report it. Do not change the behaviour without a source.
- [ ] **Step 2: Run the tests.** Command: `cargo test -p paigasus-helikon-providers-gemini -p paigasus-helikon-providers-anthropic -p paigasus-helikon-providers-openai -p paigasus-helikon-providers-bedrock tool_result_then_user_message`.
- [ ] **Step 3: Update the book.** Add `UserMessageInjected` and `RunStopped` to the event list in `agent-loop.md` (lines 56-66). Add the section "Turn-boundary checkpoint", with the content listed in spec §6.4, and a short Rust example that uses `ScriptedCheckpoint`-style code: a struct that implements `TurnCheckpoint<()>`. Add a line to the core README only if the README lists `RunContext` features.
- [ ] **Step 4: Verify.** Command: `mdbook build docs/book` (clean, linkcheck on). Then `npx markdownlint-cli2`.
- [ ] **Step 5: Commit.** Use `test(providers): SMA-804 pin the tool-result then user-message wire shape` and `docs(book): SMA-804 document the turn-boundary checkpoint` as separate commits.

## Phase B: after SMA-803

### Task 9: Connect `RunStopped` to `StopReason`

**Precondition, chosen by the controller (not the implementer):**
- **Path 1 (expected):** SMA-803 is on `main`. The controller runs `git fetch origin && git rebase origin/main` in this worktree. The conflicts are in `AgentEvent`, `is_terminal`, `terminal_tests` and the handoff re-yield. Keep both sides. The variant count and the terminal count add up.
- **Path 2 (fallback, C1):** SMA-803 is not on `main` when Phase A is reviewed, and Sven says not to wait. Then this task also adds:
  - `#[non_exhaustive] #[derive(Debug, Clone, PartialEq)] pub enum StopReason { Completed, Stopped { reason: String } }` in `core/src/runner.rs`,
  - `pub stop_reason: StopReason` on `RunResult` (`Default` = `Completed`; add `impl Default for StopReason`),
  - `AgentError::Incomplete { stop_reason: StopReason }`.

  Use the exact names that SMA-803 uses. Ask the SMA-803 session (`paigasus-helikon-aa`) before you start, and tell it when you finish.
- **Both paths:** after the rebase, each runtime crate has exactly one `restrict_unsupported_run_features` helper, and each of the seven sites has exactly one call (`git grep -c restrict_unsupported_run_features`).

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs` (`collect` `:~312`, `collect_typed` `:~365`, `parse_final` `:248`, the `RunResult::final_output` doc)
- Test: `crates/paigasus-helikon-core/tests/turn_checkpoint.rs`, `tests/collect_typed.rs`

**Interfaces:**
- Consumes: SMA-803's `StopReason`, `RunResult::stop_reason`, `AgentError::Incomplete`.
- Produces: `collect()` maps `RunStopped { reason, usage }` to `stop_reason = StopReason::Stopped { reason }` and `usage`. `collect_typed()` returns `Err(AgentError::Incomplete { stop_reason: Stopped { .. } })`. `parse_final()` copies `stop_reason`.

- [ ] **Step 1: Write the failing tests.**
  - Extend `stop_ends_cleanly_at_boundary`: `result.stop_reason == StopReason::Stopped { reason: "operator".into() }`.
  - Extend `guardrail_tripwire_on_injection_stops` with the same check (`reason: "input guardrail tripwire"`).
  - `collect_typed.rs`, `stop_with_output_type_is_incomplete`: an `output_type` agent with one tool turn and `Stop`. No `Finalizing` model call happens (the model is called once). `collect_typed` gives `AgentError::Incomplete { stop_reason: Stopped { .. } }`.
  - `no_checkpoint_call_at_deferred_boundary`: use SMA-803's test setup for a deferred call (`with_deferred_tool_calls(true)` and a policy that returns `Defer`). The run ends `Deferred`, and `boundaries()` is empty. **Path 1 only.**
- [ ] **Step 2: Run the tests and confirm they fail.** Command: `cargo test -p paigasus-helikon-core --test turn_checkpoint --test collect_typed`.
- [ ] **Step 3: Implement** the `RunStopped` arm in `collect` and the rule in `collect_typed`, following SMA-803's `RunDeferred` code. Update the `final_output` doc: on a run that did not complete, the text did not pass the output guardrails.
- [ ] **Step 4: Run the tests and confirm they pass.** Command: `cargo test --workspace --all-features`.
- [ ] **Step 5: Commit.** Message: `feat(core): SMA-804 report a stopped run as stopreason stopped`.

### Task 10: The full CI gates

- [ ] **Step 1: Run every gate from `CLAUDE.md`, in the foreground, in this order:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-features --all-targets -- -D warnings
  cargo test --workspace --all-features
  cargo build --workspace --no-default-features
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
  DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
  npx markdownlint-cli2
  mdbook build docs/book
  ```
  Expected: every command exits 0. On macOS, the known `NATIVE_ROOTS` bedrock failures come from the checkout path, not from this change (memory). If they appear, re-run the bedrock tests from a scratchpad worktree to get a real signal.
- [ ] **Step 2: Check the diff against the spec.** `git diff origin/main --stat` touches only the files in this plan. There is no `dbg!`, no `todo!()` and no `println!` in non-test code.
- [ ] **Step 3:** Fix any failure in the task that owns the code, and commit there.
