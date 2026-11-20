# SMA-810 Handoff Transfer Result Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every handoff transfer call a tool result directly after the handoff, repair the projection of sessions stored before this change, remove the SMA-803 repeated-id deny, and give each AG-UI tool call occurrence its own id.

**Architecture:** The parent agent emits one `ToolOutputItem` per transfer call after `HandoffItem` and after the target run started (`agent.rs`). `project()` gets a read-time repair that moves only late transfer results of old logs and rewrites their cancel text (`session.rs`). The AG-UI `EventMapper` tracks call occurrences instead of raw provider ids (`agui/map.rs`).

**Tech Stack:** Rust 2024 workspace, tokio, `futures`, `serde_json`, `tracing`. Tests: `cargo test`, mock models (`MockModel`, `RequestLog`, `RecordingModel`), `MemorySession`.

**Spec:** `docs/superpowers/specs/2026-11-20-sma-810-handoff-transfer-result-design.md` (read it with this plan; section names below refer to it).

## Global Constraints

- Work only in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-810` on branch `feature/sma-810-handoff-transfer-call-has-no-tool-result-so`. Never run `git checkout`, `git switch`, `git reset`, `git stash` or any command that moves HEAD or another branch.
- Work synchronously. Run every `cargo` command in the foreground and wait for its result. Do not end your turn while a build or test runs.
- MSRV is `1.94`. Do not add dependencies.
- Before each commit: `cargo fmt --all` and `cargo clippy --workspace --all-features --all-targets -- -D warnings` must be clean.
- Every new `pub` item needs a `///` doc comment. A `pub` doc must not intra-doc-link a `pub(crate)` or private item (the `-D warnings` docs gate fails).
- `tracing` calls use `target: "paigasus::core::session"` (core session code) with `target:` (colon), never `target =`.
- Commit messages: `<type>(<scope>): SMA-810 <lowercase subject>`. Allowed scopes used here: `core`, `runtime-agentcore`, `runtime-tokio`, `docs`, `plan` (the allowlist is `scopeRegex` in `.versionrc`; `book` is not allowed). End every commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Stage files with explicit paths. Never `git add -A` or `git add .` (`.env` is not ignored).
- Result texts (exact copy from the spec):
  - first transfer call: `transferred to {target}`
  - each other transfer call of the same response: `transfer not performed: this response already transferred to {target}`
  - target `run()` returned `Err`: `transfer failed: {target} did not start`
  - synthetic cancel (existing, now one `const`): `tool call did not complete (run cancelled/timed out)`
- AG-UI alias format: `{id}~{n}`, smallest `n` ≥ 2 not used in the run.

## Review Focus

1. **Two handoffs in one session across two runs** (run 1 hands off, run 2 hands off again): the repair must handle every `HandoffOccurred`, and a later run must not move the results of the earlier one again. Test in Task 4 (`two_handoff_runs_in_one_log_each_repair_once`).
2. **A handoff response with assistant text before the transfer call** (`AM, TC t, HO, …`): the scan includes the `AssistantMessage`, and the result goes after `TC t`, not after the text. Test in Task 4 (`text_before_the_transfer_call_keeps_the_result_after_the_call`).
3. **A custom handoff tool name** (not `transfer_to_*`): transfer calls are found by position, not by name. Test in Task 2 (`custom_handoff_tool_name_gets_a_result`).
4. **A nested handoff** (root → child → grandchild): the grandchild's transfer result is re-yielded through the root, and each level has its own result with its own target name. Test in Task 2 (`nested_handoff_gives_each_transfer_call_its_own_result`).
5. **A new-layout session read again many times**: the repair is a no-op on it; a second and third run produce the same projection prefix. Test in Task 4 (`repair_is_a_no_op_on_a_new_layout_log`).

---

### Task 1: Round rule in the pairing oracles (T12)

The later tasks rely on the stricter oracle, so it comes first.

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/deferred_resume.rs` (`assert_tool_calls_paired`, ~line 579)
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs` (`assert_tool_calls_paired`, ~line 427)

**Interfaces:**
- Produces: `assert_tool_calls_paired(messages: &[Item])` in both files, with the round rule added. Signature unchanged.

- [ ] **Step 1: Add the round rule to both helpers.** Track `seen_result_since_call: bool`. On `Item::ToolCall`: if a `ToolResult` came after the last `ToolCall` and `outstanding` is not empty, panic with `"call {call_id} starts a new round while {outstanding:?} wait for a result: {messages:?}"`. Keep every existing check (repeat while waiting, result with no open call, user/assistant message while calls are outstanding, nothing outstanding at the end).

- [ ] **Step 2: Add a self-test for the oracle** in `deferred_resume.rs`:

```rust
#[test]
#[should_panic(expected = "starts a new round")]
fn pairing_oracle_rejects_a_call_after_a_result_while_a_call_waits() {
    // challenge Case 1 shape: t has no result before the second round
    let call = |id: &str| Item::ToolCall { call_id: id.into(), name: "x".into(), args: serde_json::json!({}) };
    let result = |id: &str| Item::ToolResult { call_id: id.into(), content: vec![] };
    assert_tool_calls_paired(&[call("t"), call("a"), result("a"), call("b"), result("b"), result("t")]);
}
```

- [ ] **Step 3: Run** `cargo test -p paigasus-helikon-core --test deferred_resume` and `cargo test -p paigasus-helikon-runtime-tokio --test deferred`.
Expected: the new test passes. Existing tests: all pass. If an existing test now fails, stop and report it with its output; do not weaken the rule (it is then a real invalid request that the old oracle missed).

- [ ] **Step 4: Commit** `test(core): SMA-810 add the round rule to the tool pairing oracles` (both files).

---

### Task 2: Emit the transfer results (G1, T1-T4)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (`NextAction::Handoff` arm ~1610-1700; comment at `transfer_call_ids` ~1236)
- Create: `crates/paigasus-helikon-core/tests/handoff_transfer_result.rs`

**Interfaces:**
- Produces: private `fn transfer_results(call_ids: &[String], target: &str, started: bool) -> Vec<crate::Item>` in `agent.rs`. `started == true`: first id gets `transferred to {target}`, others `transfer not performed: this response already transferred to {target}`. `started == false`: every id gets `transfer failed: {target} did not start`. Each item is `Item::ToolResult` with one `ContentPart::Text`.

- [ ] **Step 1: Write the failing tests** in `tests/handoff_transfer_result.rs` (use `MockModel`, `MemorySession`, `SessionRecorder` as `deferred_resume.rs` does; copy the small helpers you need):
  - `transfer_result_follows_handoff_item`: triage → budgeting. Assert the event kinds subsequence `ToolCallItem(t1)`, `HandoffItem`, `ToolOutputItem(t1)`, `AgentUpdated`; the output text is `transferred to budgeting`. Drain the recorder: `ToolReturned(t1)` is the event directly after `HandoffOccurred`, and no event has the cancel text. `assert_tool_calls_paired(&project(&log).messages)` holds, and the item after `ToolCall t1` is `ToolResult t1`.
  - `two_transfer_calls_get_two_results_in_call_order`: one response with calls `t1 → transfer_to_budgeting`, `t2 → transfer_to_budgeting`. Outputs in order: `t1: transferred to budgeting`, `t2: transfer not performed: this response already transferred to budgeting`.
  - `no_result_for_a_handoff_that_did_not_occur`: four cases, each asserts no `ToolOutputItem` for the transfer call and no output text starting with `transferred`: an `OnHandoff` hook that denies; `max_agent_depth` exceeded (see `handoff_cycle_hits_depth_guard` in `tests/handoff.rs`); an unknown target (a handoff whose agent name does not match, as in `handoff.rs`); a `SwarmAgent` with `max_handoffs(0)` whose drained log is exactly `[user, tool_called(call-1), tool_returned(call-1, <cancel text>)]` with no `handoff_occurred`.
  - `target_run_error_gives_transfer_failed`: a target agent whose `run()` returns `Err` (a test `Agent` impl). Output: `transfer failed: budgeting did not start`, then `RunFailed`; no `AgentUpdated { agent: "budgeting" }`.
  - `custom_handoff_tool_name_gets_a_result` (Review Focus 3).
  - `nested_handoff_gives_each_transfer_call_its_own_result` (Review Focus 4): root → mid → leaf; outputs `t1: transferred to mid`, `t2: transferred to leaf`, each directly after its own `HandoffItem`.

- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-core --test handoff_transfer_result`. Expected: FAIL (no `ToolOutputItem` for the transfer calls).

- [ ] **Step 3: Implement in the `NextAction::Handoff` arm.** New order after the `OnHandoff` check: yield `HandoffItem`; build `input`; `let started = target_agent.run(child, input).await;`; yield one `ToolOutputItem` per item of `transfer_results(&transfer_call_ids, &target, started.is_ok())`; on `Err` set the failure and yield `RunFailed` as today (no `AgentUpdated`); on `Ok` yield `AgentUpdated` and re-yield the child stream as today. In this task keep the existing `ctx.handoff_child().with_open_call_ids(..)` call, but pass `transfer_call_ids.clone()` so the ids stay available for `transfer_results` (Task 3 deletes that call). Do not push the results into `conversation`. Update the comment at `transfer_call_ids`: the ids are used for the results that the parent emits after the handoff.

- [ ] **Step 4: Run** `cargo test -p paigasus-helikon-core` (whole crate). Expected: the new tests pass. Existing tests that assert an exact event sequence for a handoff (for example `tests/loop_happy_path.rs`, `tests/handoff.rs`, `tests/swarm.rs`) may need the new `ToolOutputItem` in their expected list; update only those expectations, and list each changed test in the commit body.

- [ ] **Step 5: Commit** `fix(core): SMA-810 emit a tool result for each handoff transfer call`. The body names behaviour change (1) from the spec "Release and docs".

---

### Task 3: Remove the repeated-id deny (G2, T5, T6, tokio test)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (`Authorization::Defer` arm ~830-848; `with_open_call_ids` call in the Handoff arm)
- Modify: `crates/paigasus-helikon-core/src/context.rs` (field `open_call_ids` ~114, init ~209, accessor ~375-383, copies ~446/480, test ~937; rustdoc of `with_deferred_tool_calls` ~348-353)
- Modify: `crates/paigasus-helikon-core/tests/deferred_resume.rs` (`handoff_resume_runs_the_call_on_the_child_with_the_child_conversation` ~850)
- Modify: `crates/paigasus-helikon-core/tests/handoff_transfer_result.rs`
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs` (`deferral_with_the_call_id_of_an_open_transfer_call_is_denied` ~1345)

**Interfaces:**
- Consumes: Task 2's emission. Removes `RunContext::open_call_ids()` and `with_open_call_ids()` (both `pub(crate)`).

- [ ] **Step 1: Write the failing tests.**
  - tokio: rename the test to `deferral_with_the_call_id_of_a_transfer_call_defers` and assert: `StopReason::Deferred`, `open_ids(&session) == ["fc_0"]`, labels contain `tool_returned(fc_0, transferred to budgeting)` before `tool_deferred(fc_0)`, `pay.calls()` empty. Then resume with `resolve_all(.., ToolResolution::Approved)`: the run completes, `pay.calls().len() == 1`, and `assert_tool_calls_paired` holds on every child request.
  - core, in `handoff_transfer_result.rs`, `three_levels_with_one_call_id_defer_and_resume` (T6): `scripted_handoff_session`-style chain `["triage", "budgeting", "savings"]` with `transfer_turn("fc_0", "budgeting")`, `transfer_turn("fc_0", "savings")` and a leaf turn that calls `b` with id `fc_0`. (a) The labels are exactly `user, tool_called(fc_0), handoff, tool_returned(fc_0, transferred to budgeting), tool_called(fc_0), handoff, tool_returned(fc_0, transferred to savings), tool_called(fc_0), tool_deferred(fc_0)` with no cancel text. (b) A resume that approves runs `b` once, and `assert_tool_calls_paired` holds on every model request. (c) A resume with a renamed root, and one with a missing middle agent, each deny `fc_0`, emit no `ToolOutputItem` with a `transferred` text, and leave exactly one stored result per transfer call (count results whose text starts with `transferred`).
  - core, extend `handoff_resume_runs_the_call_on_the_child_with_the_child_conversation` (T5): `assert_no_output(&events, "t1")` and `stored_results(&session, "t1") == 1`. Add `resume_route_exits_add_no_transfer_result`: on a new-layout log, the resume-route depth limit and a target `run` error each emit no new `t1` output.

- [ ] **Step 2: Run** the tokio and core tests. Expected: the tokio test and (a) fail with `permission denied: deferral not possible: call id fc_0 repeats in this run`.

- [ ] **Step 3: Delete the deny and the open-call-id plumbing.** Delete the `open_call_ids` check in the `Defer` arm, the field, its init and copies, `open_call_ids()`, `with_open_call_ids()`, the call in the Handoff arm, and the test `handoff_child_keeps_open_call_ids_and_subagent_child_clears_them`. In the rustdoc of `with_deferred_tool_calls`, remove the text that says a repeated call id is denied.

- [ ] **Step 4: Run** `cargo test -p paigasus-helikon-core` and `cargo test -p paigasus-helikon-runtime-tokio`. Expected: PASS.

- [ ] **Step 5: Commit** `fix(core): SMA-810 let a handoff child defer a call that repeats a transfer call id`. The body names behaviour change (2).

---

### Task 4: Read-time repair in `project()` (G4, T10)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/session.rs` (`project` ~358; `cancelled_result` ~724; unit tests module)
- Modify: `crates/paigasus-helikon-core/src/deferral.rs` (`transfer_calls_before` ~648 uses the shared boundary scan)

**Interfaces:**
- Produces (in `session.rs`):
  - `pub(crate) const CANCELLED_TOOL_CALL_TEXT: &str = "tool call did not complete (run cancelled/timed out)";` used by `cancelled_result`.
  - `pub(crate) fn transfer_run_start(events: &[SessionEvent], handoff_at: usize) -> usize`: the index where the run of `ToolCalled`/`AssistantMessage` events directly before `handoff_at` starts. `transfer_calls_before` calls it.
  - private `fn repair_legacy_transfer_results(events: &[SessionEvent], messages: &mut Vec<Item>, sources: &[usize]) -> usize`: returns the number of moved results.

- [ ] **Step 1: Write the failing unit tests** in `session.rs` tests. Build each log by hand with `SessionEvent` constructors; `cancel(id)` = `tool_returned(id, CANCELLED_TOOL_CALL_TEXT)`. Assert the projected item sequence as `(kind, call_id, text)` tuples:
  - `legacy_transfer_result_moves_next_to_its_call`: `user, TC t, HO(triage→budgeting), AM(child), cancel(t)` → `user, TC t, TR t "transferred to budgeting", AM`.
  - `legacy_two_transfer_calls_move_in_call_order`: `user, TC t, TC u, HO, AM, cancel(t), cancel(u)` → `…TC t, TC u, TR t "transferred to budgeting", TR u "transfer not performed: this response already transferred to budgeting", AM`.
  - `legacy_child_tool_rounds` (Case 1): `user, TC t, HO, TC a, TR a, TC b, TR b, AM, cancel(t)` → `user, TC t, TR t, TC a, TR a, TC b, TR b, AM`.
  - `legacy_repeated_id_pairs_each_result_with_its_call` (Case 2): `user, TC fc_0(transfer), HO, TC fc_0(lookup), TR fc_0 "found", TC c1, cancel(c1), cancel(fc_0)` → the result after the transfer call is `transferred to budgeting`; the result after `lookup` is `found`. Also a three-level version that asserts `transferred to budgeting` and `transferred to savings` at their own positions.
  - `running_child_call_keeps_its_cancel`: `TC fc_0(T), HO, TC fc_0(C), cancel(fc_0), cancel(fc_0)` → T gets `transferred to …`, C keeps the cancel text.
  - `failed_target_run_keeps_the_cancel_text` (AG1): `user, TC t, HO, cancel(t)` projects unchanged.
  - `non_cancel_late_text_moves_and_keeps_its_text`.
  - `unchanged_logs`: no handoff; new layout (`TC t, HO, TR t "transferred to budgeting", AM`); the ParallelAgent interleaving `AM(A), TC fc_0(search), AM(B), TC fc_0(lookup), TR "search data", TR "lookup data"`.
  - `compaction_cases`: `Compacted` that removes the transfer call leaves the result in place; `user, AM, Compacted(2), user, TC t, HO, AM(child), cancel(t)` repairs after the summary; the message count before and after the repair is equal.
  - `two_handoff_runs_in_one_log_each_repair_once` (Review Focus 1), `text_before_the_transfer_call_keeps_the_result_after_the_call` (Review Focus 2), `repair_is_a_no_op_on_a_new_layout_log` (Review Focus 5).

- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-core --lib session`. Expected: the legacy tests FAIL; the unchanged-log tests pass.

- [ ] **Step 3: Implement.** In `project()`, keep `sources: Vec<usize>` next to `messages` (push the event index with each message; truncate it together with `messages` in the `Compacted` arm; the `Compacted` summary message gets the `Compacted` event index). After the loop, call `repair_legacy_transfer_results`, and if it returns `n > 0`, `tracing::debug!(target: "paigasus::core::session", moved = n, "repaired legacy handoff transfer results")`. The repair follows spec G4 steps 1-5 exactly: step 1 via `transfer_run_start`; step 2 is one forward pass over `events` (close the most recent open non-transfer call with the id, else the earliest open transfer call); step 4 moves a result only when at least one message lies between its target position and it, in one rebuild pass; step 5 rewrites only the exact `CANCELLED_TOOL_CALL_TEXT`, with `to` from that call's own `HandoffOccurred`. Change `transfer_calls_before` to use `transfer_run_start`. Update the rustdoc of `project` (the repair, and that the snapshot order no longer follows the event order one-to-one for old handoff logs).

- [ ] **Step 4: Run** `cargo test -p paigasus-helikon-core`. Expected: PASS.

- [ ] **Step 5: Commit** `fix(core): SMA-810 repair late transfer results when a session is projected`. The body names behaviour change (3).

---

### Task 5: Old-layout coverage and second-run tests (T7, T8, T9, T11, T13)

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/deferred_resume.rs`
- Modify: `crates/paigasus-helikon-core/src/deferral.rs` (tests module)
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs` (or a new `tests/handoff_session.rs` in runtime-tokio)

**Interfaces:**
- Consumes: Tasks 2-4.
- Produces: test helper `async fn with_late_transfer_results(session: &Arc<MemorySession>) -> Arc<MemorySession>` in `deferred_resume.rs`.

- [ ] **Step 1: Add `with_late_transfer_results`.** It removes each `ToolReturned` that directly follows a `HandoffOccurred` (and its sibling transfer results) and appends `tool_returned(id, "tool call did not complete (run cancelled/timed out)")` for each removed one at the end of the handoff run's events, in call order. The literal must equal `CANCELLED_TOOL_CALL_TEXT`; T11 below fails if it does not (the rewrite to `transferred to …` would not happen).

- [ ] **Step 2: Run the four "moves" tests on both layouts.** Keep the four existing tests (`renamed_root_on_a_recorded_log_moves_…`, `missing_middle_agent_on_a_recorded_log_moves_…`, `missing_target_moves_every_recorded_transfer_result_…`, `missing_middle_agent_moves_two_recorded_transfer_results_that_share_one_call_id`) on the new layout, and add a `legacy_` copy of each that wraps its session with `with_late_transfer_results`. Update the helper docs that say the recorder closes transfer calls at the end of the run (`text_then_call_b_session`, `open_transfer_session`, the comment near the old line 969).

- [ ] **Step 3: Add the custom-session test (T13).** `struct RawSnapshotSession(Arc<MemorySession>)` whose `snapshot()` returns the raw item order without `project()` (map each event 1:1; skip `HandoffOccurred`/`ToolDeferred`). On a legacy same-id log with no text, a renamed-root resume produces a model request where `assert_tool_calls_paired` holds and each result is directly after its true call. This keeps coverage of the `close_open_transfers` move branch.

- [ ] **Step 4: Add T8** in `deferral.rs` tests: `transfer_calls_to_next_in_the_new_layout` with the log `user, TC t, TC u, HO(root→mid), TR t, TR u, TC b, TD b` → `transfer_calls_to_next() == [transfer("t", 0), transfer("u", 0)]`.

- [ ] **Step 5: Add T9 and T11** in runtime-tokio: `second_run_after_a_handoff_sends_paired_history` (run 1: handoff run via `TokioRunner` on a `MemorySession`; run 2: a normal run of the root on the same session with a `RecordingModel`; the root request satisfies `assert_tool_calls_paired` and the item after the transfer call is its result `transferred to budgeting`). `second_run_on_a_legacy_session_sends_paired_history`: the same, but the session is rewritten to the old layout first (copy `with_late_transfer_results` into this file); the result after the transfer call reads `transferred to budgeting`.

- [ ] **Step 6: Run** `cargo test -p paigasus-helikon-core` and `cargo test -p paigasus-helikon-runtime-tokio`. Expected: PASS.

- [ ] **Step 7: Commit** `test(core): SMA-810 cover old and new handoff log layouts`.

---

### Task 6: AG-UI call occurrences (G5, T14)

**Files:**
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/agui/map.rs` (state fields ~80-120; `push` arms for `ToolCallDelta`, `ToolCallItem`, `ToolOutputItem`, `ApprovalRequested`; helpers `close_call`, `name_of`, `take_buffered`, `close_all`; module doc ~23-25; tests `assert_tool_calls_well_formed` ~620)

**Interfaces:**
- Produces (private to the mapper):
  - `struct Occurrence { agui_id: String, named: bool, item_seen: bool }`
  - fields `current: HashMap<String, Occurrence>` (provider id → latest occurrence) and `used_ids: HashSet<String>`; `call_names`, `completed_calls`, `buffered_calls` and `open_call` are keyed by AG-UI id.
  - `fn occurrence_for_delta(&mut self, call_id: &str, has_name: bool) -> String`
  - `fn occurrence_for_item(&mut self, call_id: &str) -> String`
  - `fn agui_id_of(&self, call_id: &str) -> String` (current occurrence, else the provider id)

- [ ] **Step 1: Change the oracle `assert_tool_calls_well_formed`** as the spec G5 "Oracle" paragraph says: split `received` per occurrence by cutting at each `TurnStarted` and `HandoffItem` in the input events; occurrence `n` of `x` maps to `x` (n = 1) or `x~n`; emitted args per AG-UI id must equal received args per occurrence; each `TOOL_CALL_RESULT` uses the AG-UI id of the call its `ToolOutputItem` closes.

- [ ] **Step 2: Write the failing tests:**
  - `a_reused_call_id_in_a_later_turn_gets_its_own_span` (streamed and not streamed): turn 1 `fc_0` tool `a` + result, turn 2 `fc_0` tool `b` → second `START` has `toolCallId: "fc_0~2"`, `toolCallName: "b"`; its `RESULT` uses `fc_0~2`.
  - `a_handoff_child_reusing_ids_renders_its_own_calls`: parent turn 1 normal call `fc_1`; handoff response deltas `search fc_0`, `transfer fc_1` (only the transfer gets a `ToolCallItem`); `HandoffItem`; transfer result; child `TurnStarted`; child deltas+items `lookup fc_0`, `pay fc_1`. Assert: the transfer span and its result use `fc_1~2`; the child calls use `fc_0~2` and `fc_1~3` with names `lookup` and `pay`; no args merge.
  - the same with the transfer delta first (so `search` is buffered).
  - `parallel_branches_with_unique_ids_render_as_today`: branch A deltas for `call_a`, branch B `TurnStarted`, more A deltas, A item → one span for `call_a`, name kept, no second span. Compare against the frames of the same input on `main` (paste the expected frame list from a run of the unchanged mapper).
  - `an_alias_skips_an_emitted_provider_id`: the run emits provider id `x~2` for a real call, then reuses `x` twice → the second use of `x` gets `x~3`.
  - `approval_frame_uses_the_current_alias`.
  - `a_result_for_a_buffered_call_flushes_it_with_its_name`.
  - Keep `a_delta_revisiting_a_closed_id_keeps_its_args_and_its_name` and `an_item_for_an_already_closed_call_does_not_re_emit_its_span` green (continuation deltas have `name: None`, and a first item for an occurrence is not a new occurrence).

- [ ] **Step 3: Run** `cargo test -p paigasus-helikon-runtime-agentcore agui::map`. Expected: the new tests FAIL.

- [ ] **Step 4: Implement** the occurrence rule from spec G5 (new occurrence on a second name-carrying delta, on a second `ToolCallItem`, or when none exists; no reset at `TurnStarted`). `ToolOutputItem`: if the AG-UI id is in `buffered_calls`, flush it first, then emit `TOOL_CALL_RESULT` with `agui_id_of`. `ApprovalRequested`: build the custom frame, then replace its `call_id` value with `agui_id_of(call_id)`. Update the module doc and field docs for the occurrence rule.

- [ ] **Step 5: Run** `cargo test -p paigasus-helikon-runtime-agentcore`. Expected: PASS.

- [ ] **Step 6: Commit** `fix(runtime-agentcore): SMA-810 give each ag-ui tool call occurrence its own id`.

---

### Task 7: Docs, comments and full gates

**Files:**
- Modify: `docs/book/src/concepts/multi-agent-patterns.md` (~100-101: handoff event order)
- Modify: `docs/book/src/concepts/agent-loop.md` (only if it lists the handoff event order)
- Modify: `docs/book/src/concepts/permissions-guardrails-hooks.md` (~88: "Repeated call ids" paragraph)
- Modify: `docs/book/src/concepts/sessions.md` (or the page that describes `project`/snapshots; find it with `grep -rn "project(" docs/book/src`)
- Modify: `crates/paigasus-helikon-core/src/resume_exec.rs` (`close_open_transfers` doc), `crates/paigasus-helikon-core/src/deferral.rs` (`agent_view` doc; comment in `child_history_drops_the_recorder_result…`)

- [ ] **Step 1: Update the texts** listed in the spec "Wording updates": the handoff order is `HandoffItem`, the transfer results, then `AgentUpdated`; a deferral with a repeated call id now defers (say why in one sentence); the snapshot repairs late transfer results of sessions stored before this change; internal comments say the parent emits the result after the handoff and that an older session can still hold a late result. Write in plain, short sentences.

- [ ] **Step 2: Grep for stale text:** `grep -rn "repeats in this run\|closes them at the end of the\|closes each transfer call at the end\|get no result while the child" crates docs/book/src`. Expected: no match.

- [ ] **Step 3: Run every CI gate** from the repo root:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
npx markdownlint-cli2
mdbook build docs/book
```

Expected: all clean. If `cargo test --workspace --all-features` shows the macOS `NATIVE_ROOTS` bedrock failures (about 48, ~15 s), they depend on the checkout path, not the code: report them and confirm with a run of those tests on `main` in the same worktree path before you continue.

- [ ] **Step 4: Commit** `docs(docs): SMA-810 describe the handoff transfer result and the session repair` (book pages and the doc-comment files).
