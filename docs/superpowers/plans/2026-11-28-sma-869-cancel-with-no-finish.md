# SMA-869 Cancelled Run Ends As Cancelled — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A run whose `RunContext` cancel token fired ends with
`RunError::Cancelled` through a runner, and never `Ok(Completed)` with cut or
earlier-turn text.
A cancel after the `Finish` of the final text turn still ends `Completed`.
A terminal event that already happened wins.

**Architecture:** Add "token fired → return with no further event" checks in
the `LlmAgent` loop driver (`agent.rs`), and the `AgentAsTool` guard rule
("token fired and the child had no terminal event → no terminal event") in the
four workflow agents and the MCP agent server. The runners already turn "no
terminal event + token fired" into `Cancelled`, so no runner changes.

**Tech Stack:** Rust 2024, tokio, `async_stream`, `tokio_util::CancellationToken`.

**Spec:** `docs/superpowers/specs/2026-11-28-sma-869-cancel-with-no-finish-design.md`
(read it first; goal numbers below refer to it).

## Global Constraints

- No public API change: no new `pub` item, no changed signature, no change to
  `transition` or `crates/paigasus-helikon-core/src/loop_state.rs`.
- No change in any `paigasus-helikon-runtime-*` `src/` file.
- Each check is exactly: `if ctx.cancel().is_cancelled() { <yield what the
  step says>; return; }`. No new `AgentEvent`, no `RunFailed`, no span field.
- Do not hand-bump any crate version or edit any CHANGELOG. release-plz does it.
- Commit prefix: `fix(core): SMA-869 …`, `fix(mcp): SMA-869 …`,
  `test(runtime-tokio): SMA-869 …`, `docs(docs): SMA-869 …`, `test(core): SMA-869 …`. Check each scope
  against `.versionrc` before you commit.
- Before each commit: `cargo fmt --all` and
  `cargo clippy --workspace --all-features --all-targets -- -D warnings`.
- Work in the foreground. Do not background a cargo command. Do not end a task
  before its test command reports a final status.
- Do not run any git command that moves `HEAD` or a branch (`checkout`,
  `switch`, `reset`, `rebase`, `merge`). Stage files by explicit path; never
  `git add -A` or `git add .`.
- Test rule from the spec: each behaviour test fails on the unpatched code.
  Fire the **run** token (a captured clone of the token given to
  `RunContext::with_cancel`, or `ctx.cancel()` inside a hook). Do not use
  `ToolContext::cancel()` to fire it: that is a child token. The model or tool
  must end on the token **without a pending poll**, so the agent-side check
  decides and not the `cancelled()` branch of `controlled`.

## Review Focus

1. A text turn with no `Finish` and **no** cancel (OpenAI-compatible servers)
   must still complete — Task 1 has the regression guard.
2. A cancel after `Finish(Stop)` of the final turn must still give
   `Ok(Completed)` with the full text (goal 12) — Task 1, with a mutation check.
3. A workflow child that ends with no terminal event and **no** cancel must
   keep today's behaviour — Task 4 has a regression guard.
4. `run_streamed` must give the same result as `run` for a cut text turn —
   Task 1 tests both.
5. A cancelled resume must close every deferral (no open call left in the
   session) — Task 3 asserts `open_ids(...).is_empty()`.

---

### Task 1: Model-call checks in the loop driver

Covers goals 1, 5, 10, 11, 12 (checks 3–6 of the spec).

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (the `NextAction::CallModel` arm, about lines 1430–1562)
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/common/mod.rs` (add the model helper)
- Create: `crates/paigasus-helikon-runtime-tokio/tests/cancel_boundaries.rs`
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/run_control.rs:63-87` (`prefired_cancel_still_completes_ready_run`)

**Interfaces:**
- Produces (test helper, in `tests/common/mod.rs`):

```rust
/// One step of a `CancelScriptModel` turn.
pub enum Step {
    /// Yield this event, unless the token already fired.
    Emit(ModelEvent),
    /// Fire the run token. Later steps see a fired token.
    Cancel,
    /// Yield this error.
    Fail(ModelError),
}

/// A model that obeys its token: before each step it checks the token and
/// ends the stream (no `Finish`) when the token fired. One script per
/// `invoke`; every `invoke` increments `calls`.
pub struct CancelScriptModel { /* scripts, token, calls: AtomicUsize, invoke_error: Option<ModelError> */ }

impl CancelScriptModel {
    pub fn new(token: CancellationToken, scripts: Vec<Vec<Step>>) -> Arc<Self>;
    /// `invoke` fires the token, then returns `Err(e)`.
    pub fn invoke_error_after_cancel(token: CancellationToken, e: ModelError) -> Arc<Self>;
    /// The number of `invoke` calls so far.
    pub fn calls(&self) -> usize;
}
```

The stream is synchronous (`stream::iter`-like over steps via
`async_stream::stream!`), so it never leaves a pending poll. `Cancel` steps do
not yield. `Step::Fail` yields `Err` even after a cancel (the token check
applies to `Emit` only).

- [ ] **Step 1: Write the failing tests** in `cancel_boundaries.rs`
  (`#[path = "common/mod.rs"] mod common;` as the other files do). Use
  `text_agent`, `run_context_with_cancel`, `run_context_with_session_and_cancel`
  and `MemorySession`. Tests:

  - `cancel_during_text_stream_is_cancelled`: script
    `[Emit(TokenDelta "par"), Cancel, Emit(TokenDelta "tial"), Emit(Finish Stop)]`.
    `TokioRunner.run(...)` → `assert!(matches!(res, Err(RunError::Cancelled)))`.
  - `cancel_during_text_stream_is_cancelled_streamed`: same script through
    `TokioRunner.run_streamed(...)`, then `.collect().await` →
    `Err(RunError::Cancelled)`. (Mirror how `tests/run_streamed.rs` reads the
    streamed result.)
  - `cancelled_text_turn_is_not_stored`: same script with a `MemorySession`.
    After the run, project the session (`paigasus_helikon_core::project`, as in
    `session_persistence.rs`) and assert that no `Item::Message` with role
    assistant contains the text `"par"`.
  - `model_invoke_error_after_cancel_is_cancelled`:
    `CancelScriptModel::invoke_error_after_cancel(token, ModelError::Unavailable(..))`
    (use whichever `ModelError` variant fits; read `ModelError`) →
    `Err(RunError::Cancelled)`.
  - `model_stream_error_after_cancel_is_cancelled`: script
    `[Emit(TokenDelta "a"), Cancel, Fail(ModelError::Other(anyhow!("reset")))]`
    → `Err(RunError::Cancelled)`.
  - `cancel_after_finish_still_completes` (goal 12): script
    `[Emit(TokenDelta "done"), Emit(Finish Stop), Cancel]` with a
    `MemorySession` → `Ok(r)`, `r.stop_reason == StopReason::Completed`,
    `r.final_output == "done"`, and the projected session holds an assistant
    message with `"done"`.
  - `text_turn_without_finish_and_without_cancel_completes` (regression guard,
    label it so in a doc comment): script `[Emit(TokenDelta "ok")]`, token
    never fired → `Ok`, `final_output == "ok"`.

- [ ] **Step 2: Replace `prefired_cancel_still_completes_ready_run`** in
  `run_control.rs` with `prefired_cancel_ends_cancelled_without_a_model_call`:
  pre-fired token, `CancelScriptModel::new(token, vec![vec![Emit(TokenDelta "hi"), Emit(Finish Stop)]])`
  → `Err(RunError::Cancelled)` and `model.calls() == 0`. Update the comment:
  a token that fired before the run starts is not "a terminal that already
  happened", so SMA-421 does not apply.

- [ ] **Step 3: Run the tests to verify they fail**

  Run: `cargo test -p paigasus-helikon-runtime-tokio --test cancel_boundaries --test run_control`
  Expected: every new test FAILS except `cancel_after_finish_still_completes`
  and `text_turn_without_finish_and_without_cancel_completes` (both pass on
  unpatched code; they are guards).

- [ ] **Step 4: Implement checks 3–6 in `agent.rs`**

  - Check 3: first statement of the `NextAction::CallModel { request }` arm,
    before `chat_span` is made: `if ctx.cancel().is_cancelled() { return; }`.
  - Check 4: in the `Err(e)` arm of `model.invoke(...)`: return with no event
    when the token fired, before the span records and `failure.set`.
  - Check 5: in the `Err(e)` arm of `model_stream.next()`: the same.
  - Check 6: after the `while let` loop, after `finish_seen` is read and before
    `acc.finish()`: `if !finish_seen && ctx.cancel().is_cancelled() { return; }`.
    Keep the existing check inside the `acc.finish()` `Err` branch.
  - Keep the comment style of the existing check at line 1512 ("A model MUST
    NOT emit `Finish` on cancellation…") — one short comment per check.

- [ ] **Step 5: Run the tests to verify they pass**

  Run: `cargo test -p paigasus-helikon-runtime-tokio --test cancel_boundaries --test run_control`
  Expected: PASS.

- [ ] **Step 6: Mutation check for goal 12.** Temporarily change check 6 to
  `if ctx.cancel().is_cancelled() { return; }` (drop `!finish_seen`). Run
  `cargo test -p paigasus-helikon-runtime-tokio --test cancel_boundaries cancel_after_finish_still_completes`.
  Expected: FAIL. Restore the condition and run it again: PASS. Report both
  results.

- [ ] **Step 7: Run the core and runtime-tokio suites**

  Run: `cargo test -p paigasus-helikon-core -p paigasus-helikon-runtime-tokio --all-features`
  Expected: PASS. (`trybuild_ui` can fail locally from a rustc mismatch; if it
  does, run it on the unpatched commit too and report whether it also fails
  there.)

- [ ] **Step 8: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs \
  crates/paigasus-helikon-runtime-tokio/tests/common/mod.rs \
  crates/paigasus-helikon-runtime-tokio/tests/cancel_boundaries.rs \
  crates/paigasus-helikon-runtime-tokio/tests/run_control.rs
git commit -m "fix(core): SMA-869 end a cancelled model turn with no terminal event"
```

---

### Task 2: Turn-boundary checks (checkpoint, tools, handoff)

Covers goals 2, 3, 4 (checks 2, 7, 8 of the spec).

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (the `TurnStarted` arm of the event loop, about line 1250; the `ExecuteTools` arm, about line 1563; the `Handoff` arm, about line 1631)
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/cancel_boundaries.rs`

**Interfaces:**
- Consumes: `CancelScriptModel`, `Step` from Task 1.

- [ ] **Step 1: Write the failing tests** in `cancel_boundaries.rs`. A tool
  helper in this file: `CountingTool { name, calls: AtomicUsize, cancel: Option<CancellationToken> }`
  that counts calls, fires `cancel` (when set) and returns `Ok` at once.

  - `cancel_between_turns_makes_no_second_model_call` (case 2): turn 1 is a
    tool call to `count` + `Finish(ToolCalls)`; turn 2 is `[Emit(TokenDelta "late"), Emit(Finish Stop)]`.
    Fire the run token from a `Hook` on `HookEvent::PostToolUse` (read
    `HookEvent` for the exact variant) through `ctx.cancel().cancel()`; build
    the context with `run_context_with_cancel_and_hooks`. Assert
    `Err(RunError::Cancelled)` and `model.calls() == 1`.
  - `cancel_between_turns_skips_the_turn_checkpoint`: as above, plus a
    `TurnCheckpoint` that counts `at_boundary` calls and returns
    `CheckpointDecision::Continue` (pattern: `tests/turn_checkpoint.rs:99`).
    Assert the count is 0.
  - `cancel_while_a_tool_runs_is_cancelled` (case 3): no hook; the tool fires
    a clone of the run token. Assert `Err(RunError::Cancelled)` and
    `model.calls() == 1`.
  - `cancel_after_tool_call_finish_runs_no_tool` (goal 4): turn 1 is
    `[Emit(ToolCallDelta count {}), Emit(Finish ToolCalls), Cancel]`. Assert
    `Err(RunError::Cancelled)` and the tool's call count is 0.
  - `cancel_after_transfer_finish_does_no_handoff`: an agent with one handoff
    target (build as `deferred.rs:646-665` does, with `.handoff(...)`); turn 1
    is `[Emit(ToolCallDelta transfer_to_<target> {}), Emit(Finish ToolCalls), Cancel]`.
    Through `run_streamed`, collect the events: assert no
    `AgentEvent::HandoffItem` and the result is `Err(RunError::Cancelled)`.

- [ ] **Step 2: Run the tests to verify they fail**

  Run: `cargo test -p paigasus-helikon-runtime-tokio --test cancel_boundaries`
  Expected: the five new tests FAIL. (`cancel_between_turns_*` and
  `cancel_while_a_tool_runs_*` fail on the Task 1 code too, because check 3
  stops only the model call; the checkpoint runs first. If one of them passes
  after Task 1, report it and keep it as a guard.)

- [ ] **Step 3: Implement checks 2, 7, 8 in `agent.rs`**

  - Check 2: first statement of the `crate::AgentEvent::TurnStarted { turn }`
    match arm in the `for ev in events` loop, before the checkpoint: return
    when the token fired. `TurnStarted` is not yielded.
  - Check 7: first statement of `NextAction::ExecuteTools { calls }`.
  - Check 8: first statement of `NextAction::Handoff`, before the
    `match loop_state`.

- [ ] **Step 4: Run the tests to verify they pass**

  Run: `cargo test -p paigasus-helikon-runtime-tokio --test cancel_boundaries`
  Expected: PASS.

- [ ] **Step 5: Run the core and runtime-tokio suites** (as Task 1 Step 7).

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs \
  crates/paigasus-helikon-runtime-tokio/tests/cancel_boundaries.rs
git commit -m "fix(core): SMA-869 start no turn, tool or handoff after a cancel"
```

---

### Task 3: Resume batch check

Covers goal 6 (check 1 of the spec).

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (the resume block, about lines 1069–1195)
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs` (next to `cancelled_resume_closes_the_deferral`, about line 897)

**Interfaces:**
- Consumes: `not_run` (the `Vec<AgentEvent>` made at `agent.rs:950`) and
  `crate::resume_exec::close_open_transfers` — both exist.

- [ ] **Step 1: Write the failing test** `prefired_cancel_on_resume_runs_no_approved_call`
  in `deferred.rs`: `deferred_session(&["call_1"])`, `PayTool::new()`,
  `resolve_all(&session, ToolResolution::Approved)`, a pre-fired token on
  `ctx(session.clone(), true).with_cancel(token)`. Assert:
  `matches!(result, Err(RunError::Cancelled))`, `pay.calls().len() == 0`,
  `labels(&session).await[3..] == [format!("tool_returned(call_1, {NOT_RUN})"), "user_message(next)".to_owned()]`
  (check the order against what the recorder writes; if `user_message(next)`
  comes in another place, follow `cancelled_resume_closes_the_deferral`), and
  `open_ids(&session).await.is_empty()`.

- [ ] **Step 2: Run it to verify it fails**

  Run: `cargo test -p paigasus-helikon-runtime-tokio --test deferred prefired_cancel_on_resume_runs_no_approved_call`
  Expected: FAIL (`pay.calls().len()` is 1).

- [ ] **Step 3: Implement check 1 in `agent.rs`**

  - Local path: after the `closed` transfer results are yielded (after the
    `for item in closed` loop) and before `ctx.to_tool_context()` /
    `run_resolved_calls`: if the token fired, `for ev in not_run { yield ev; }`
    and return.
  - Handoff-routed path: before `yield AgentUpdated { agent: next.clone() }`:
    if the token fired, yield the `close_open_transfers(&plan, &mut conversation)`
    items as `ToolOutputItem`, then the `not_run` events, then return. This
    mirrors the `MaxAgentDepthExceeded` exit just above it, without `RunFailed`.

- [ ] **Step 4: Run the deferred suite**

  Run: `cargo test -p paigasus-helikon-runtime-tokio --test deferred`
  Expected: PASS, including `cancelled_resume_closes_the_deferral`.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs \
  crates/paigasus-helikon-runtime-tokio/tests/deferred.rs
git commit -m "fix(core): SMA-869 run no approved call on a cancelled resume"
```

---

### Task 4: Workflow agents pass a cancel through

Covers goal 8.

**Files:**
- Modify: `crates/paigasus-helikon-core/src/workflow.rs` (`SequentialAgent` about 204–243, `ParallelAgent` about 390–456, `LoopAgent` about 587–636)
- Modify: `crates/paigasus-helikon-core/src/graph.rs` (about 291–379)
- Modify: `crates/paigasus-helikon-core/tests/workflow_sequential.rs`, `workflow_parallel.rs`, `workflow_loop.rs`, `graph.rs`

**Interfaces:**
- Consumes: `AgentEvent::is_terminal(&self) -> bool` (`agent.rs:572`),
  `common::MockAgent::new(name, |ctx| -> Vec<AgentEvent>)`,
  `common::assistant_msg(agent, text) -> AgentEvent`.

- [ ] **Step 1: Write the failing tests**, one in each file. Each uses a
  pre-fired token (`RunContext::ephemeral(()).with_cancel(token)`) and children
  built with `MockAgent::new(name, |_| vec![assistant_msg(name, "partial")])`
  (no terminal event). Collect the raw stream with `StreamExt::collect::<Vec<_>>()`
  (not `RunResultStreaming`, which returns `Ok` for no terminal):

  - `workflow_sequential.rs::cancelled_child_ends_sequence_with_no_terminal`:
    two children; assert no event `is_terminal()`, and the second child did
    not run (no `assistant_msg` from it).
  - `workflow_parallel.rs::cancelled_branch_ends_parallel_with_no_terminal`:
    two branches; assert no event `is_terminal()`.
  - `workflow_loop.rs::cancelled_child_ends_loop_with_no_terminal`:
    `max_iterations` 3; assert no event `is_terminal()` and exactly one
    iteration ran.
  - `graph.rs::cancelled_node_ends_graph_with_no_terminal`: two nodes `a → b`;
    assert no event `is_terminal()` and `b` did not run.
  - Regression guard (label it), in `workflow_sequential.rs`:
    `child_without_terminal_and_without_cancel_keeps_running` — same two
    children, token not fired; assert today's behaviour (the last event is
    `RunCompleted` and both children ran).

- [ ] **Step 2: Run them to verify they fail**

  Run: `cargo test -p paigasus-helikon-core --test workflow_sequential --test workflow_parallel --test workflow_loop --test graph`
  Expected: the four `cancelled_*` tests FAIL; the guard PASSES.

- [ ] **Step 3: Implement the guard** in each agent: track
  `let mut saw_terminal = false;` per child, set it when `ev.is_terminal()`
  while the child stream drains. After the drain (after `OnSubagentStop` where
  the agent fires it): `if !saw_terminal && ctx.cancel().is_cancelled() { return; }`.
  For `ParallelAgent`, apply it once after every branch drained, if any branch
  had no terminal event. For `GraphAgent`, apply it after each node drains,
  before successors start. Add a one-line comment that names the
  `AgentAsTool` rule.

- [ ] **Step 4: Run the core suite**

  Run: `cargo test -p paigasus-helikon-core --all-features`
  Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/workflow.rs crates/paigasus-helikon-core/src/graph.rs \
  crates/paigasus-helikon-core/tests/workflow_sequential.rs crates/paigasus-helikon-core/tests/workflow_parallel.rs \
  crates/paigasus-helikon-core/tests/workflow_loop.rs crates/paigasus-helikon-core/tests/graph.rs
git commit -m "fix(core): SMA-869 end a workflow with no terminal event when a child was cancelled"
```

---

### Task 5: `AgentAsTool` test and MCP server guard

Covers goals 7 and 9.

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/agent_as_tool.rs` (next to `agent_as_tool_cancelled_sub_run_is_a_tool_error`, about line 231)
- Modify: `crates/paigasus-helikon-mcp/src/server.rs` (about lines 331–367)
- Modify: `crates/paigasus-helikon-mcp/tests/agent_server.rs`

- [ ] **Step 1: Write the `AgentAsTool` test**
  `agent_as_tool_cancelled_text_only_sub_run_is_a_tool_error`: copy the
  `CancelAfterCall` pattern (line 198) as a model that yields one
  `TokenDelta "part"`, fires the token and ends with no `Finish`. Assert the
  tool returns `Err` whose text contains `"cancelled"`.

- [ ] **Step 2: Run it.** `cargo test -p paigasus-helikon-core --test agent_as_tool`.
  Expected: PASS (Task 1 already fixed it). To prove that it pins the fix,
  temporarily remove check 6 from `agent.rs`, run the test (expected: FAIL,
  because the sub-run completes with `"part"`), then restore check 6 and run
  it again (expected: PASS). Report both results. Do not use `git stash`.

- [ ] **Step 3: Write the failing MCP test**
  `cancelled_run_without_terminal_is_error` in `agent_server.rs`: an `Agent<()>`
  whose `run` calls `ctx.cancel().cancel()` (the run token is a child of
  rmcp's request token, so the request stays open), yields
  `AgentEvent::MessageOutput` with assistant text `"partial"` (or
  `common`-style helper if the file has one), and ends with no terminal event.
  Assert `result.is_error == Some(true)` and the text contains `"cancelled"`.

- [ ] **Step 4: Run it to verify it fails**

  Run: `cargo test -p paigasus-helikon-mcp --all-features --test agent_server cancelled_run_without_terminal_is_error`
  Expected: FAIL (`is_error` is `None`/`false`).

- [ ] **Step 5: Implement the guard** in `call_tool`: in the `Ok(result)` arm
  of the final `match outcome`, when `cancel.is_cancelled()` and no event in
  `result.events` `is_terminal()`, return
  `CallToolResult::error(vec![ContentBlock::text("agent run was cancelled")])`.
  `cancel` is moved into `run_ctx`; keep a clone for this check.

- [ ] **Step 6: Run the MCP suite**

  Run: `cargo test -p paigasus-helikon-mcp --all-features`
  Expected: PASS, including `client_cancellation_aborts_the_run`.

- [ ] **Step 7: Commit** (two commits, so release-plz attributes each crate):

```bash
git add crates/paigasus-helikon-core/tests/agent_as_tool.rs
git commit -m "test(core): SMA-869 pin a cancelled text-only sub-run as a tool error"
git add crates/paigasus-helikon-mcp/src/server.rs crates/paigasus-helikon-mcp/tests/agent_server.rs
git commit -m "fix(mcp): SMA-869 return an error result for a cancelled agent run"
```

---

### Task 6: Documentation

**Files:**
- Modify: `docs/book/src/concepts/observability-evaluation.md:338-353`
- Modify: `docs/book/src/concepts/agent-loop.md:69` and `:149`
- Modify: `docs/book/src/concepts/runtimes.md:64`
- Modify: `crates/paigasus-helikon-evals/README.md:53`
- Check: the `Runner::run` rustdoc in `crates/paigasus-helikon-core/src/runner.rs`

Write in ASD-STE100 Simplified Technical English (short sentences, active
voice, approved words), like the surrounding pages.

- [ ] **Step 1: `observability-evaluation.md`.** Replace the paragraph
  "Cancellation is a *model*-boundary event…" with: a cancel ends the run.
  When the model stream ends with no `Finish` and the token fired, the loop
  emits no terminal event, so a runner reports `RunError::Cancelled`. No tool
  call of a cut turn runs. In the `MockModel` paragraph, keep the model-level
  facts, and add that through an agent a turn that starts after the cancel
  makes no `invoke`, so it uses no script.
- [ ] **Step 2: `evals/README.md:53`.** The same point about the script.
- [ ] **Step 3: `agent-loop.md:69`.** Keep the existing sentences; add that
  the loop ends with no terminal event when the token fired before a model
  call, a tool batch, a handoff or a resume batch, or while a model turn
  streamed with no `Finish`.
- [ ] **Step 4: `agent-loop.md:149`.** Replace "best-effort … a caller cannot
  assume …" with: a terminal event that already happened wins over a later
  cancel. Otherwise, through a runner, a fired token ends the run as
  `RunError::Cancelled`, also when it fired before the run started. Timeout
  stays best-effort as written.
- [ ] **Step 5: `runtimes.md:64`.** Say that the session keeps the complete
  items of the run, and that a cut model turn is not stored as a complete
  assistant message.
- [ ] **Step 6: `Runner::run` rustdoc.** If it says that a pre-fired token can
  still complete, change it to match Step 4. Otherwise no change.
- [ ] **Step 7: Verify**

  Run: `mdbook build docs/book` (if `mdbook` is installed; else report it as not run),
  `npx markdownlint-cli2`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`.
  Expected: all clean.

- [ ] **Step 8: Commit**

```bash
git add docs/book/src/concepts/observability-evaluation.md docs/book/src/concepts/agent-loop.md \
  docs/book/src/concepts/runtimes.md crates/paigasus-helikon-evals/README.md
# add crates/paigasus-helikon-core/src/runner.rs only if Step 6 changed it
git commit -m "docs(docs): SMA-869 describe how a cancel ends a run"
```

---

### Final check (after Task 6)

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-features --all-targets -- -D warnings`
- [ ] `cargo test --workspace --all-features` — report any failure with output;
  `trybuild_ui` per Task 1 Step 7.
- [ ] `git diff origin/main --stat` lists only the files of this plan, the
  spec and this plan.
