# SMA-864: no tool calls after a `Length` or `ContentFilter` finish — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The agent loop does not run tool calls from a model turn that a token limit, a content filter, or an early stream end cut, and it fails the run with one typed error.

**Architecture:** A guard arm in the pure `transition` function (reaches the local and the Temporal driver). A mapping in the local driver turns a reassembly error on a cut turn into the same typed error. A final sweep fixes provider stop-reason mappings and makes the accumulator refuse tool calls when no `Finish` came.

**Tech Stack:** Rust (MSRV 1.94), `thiserror`, `tokio` tests, the workspace mock model/tool in `crates/paigasus-helikon-core/tests/common/mod.rs`.

**Spec:** `docs/superpowers/specs/2026-11-26-sma-864-no-tool-calls-after-length-design.md` (read it; this plan argues from it).

## Global Constraints

- Work only in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon-sma-864`. Use worktree-absolute paths. Do not run `git checkout`, `git switch`, `git reset`, `git rebase` or any command that moves HEAD or another branch.
- Run every cargo command in the foreground and wait for it to end. Do not background it.
- Commit messages: `<type>(<scope>): SMA-864 <lowercase subject>`. Allowed scopes here: `core`, `runtime-temporal`, `providers`, `providers-anthropic`, `providers-openai`, `docs`, `specs`, `plan`. There is no `providers-bedrock`/`providers-litellm`/`providers-gemini` scope; use `providers`.
- End each commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Stage files by explicit path. Never `git add -A` or `git add .` (`.env` and `.claude/` are not ignored).
- Before each commit: `cargo fmt --all` and `cargo clippy --workspace --all-features --all-targets -- -D warnings` must be clean.
- Every new public item gets a `///` doc comment (`missing_docs`, `-D warnings` docs gate). A public doc comment must not intra-doc-link a private item.
- Error variant name and shape (exact): `AgentError::ToolCallsNotRun { finish_reason: Option<FinishReason>, tool_names: Vec<String> }`, with `#[non_exhaustive]` on the variant.
- Blocked finish reasons (exact): `FinishReason::Length` and `FinishReason::ContentFilter`. All other reasons, including `Stop` and `Other(_)`, keep today's behavior.
- Name cap in the `Display` text: the same rule as `format_blank_names` in `crates/paigasus-helikon-core/src/model.rs` (8 names, 64 characters each).

## Review Focus

1. A cut turn with assistant text **and** a handoff call: no `ApplyingHandoff`, the text still arrives as `MessageOutput` before `RunFailed` → Task 2, test `length_with_text_and_handoff_fails_without_handoff`.
2. A reassembly error on a turn that ended with `Stop` (for example two blank-id calls) must keep `AgentError::Other`, not become `ToolCallsNotRun` → Task 3, test `reassembly_error_under_stop_keeps_other`.
3. A `Length` turn with **no** tool call still completes with its text (no regression of the non-goal) → Task 2, test `length_without_tool_calls_still_completes`.
4. A text-only stream with no `Finish` still completes with `Stop` after the Phase 2 rule → Task 7, test `no_finish_text_only_still_stop`.
5. A tool name longer than 64 characters that has multi-byte characters: the cap must not split a UTF-8 character → Task 1, test `display_caps_multibyte_name_safely`.

---

## Phase 1 — the rule (spec §2-§8)

### Task 1: The `ToolCallsNotRun` error variant

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (the `AgentError` enum, ~line 1731)
- Modify: `crates/paigasus-helikon-core/src/model.rs` (`format_blank_names`, ~line 784: make it reusable at `pub(crate)` scope, or add a shared `pub(crate) fn cap_names(names: &[String]) -> String`)
- Test: a `#[cfg(test)]` module in `crates/paigasus-helikon-core/src/agent.rs` (code outside core cannot build a `#[non_exhaustive]` variant)

**Interfaces:**
- Produces: `AgentError::ToolCallsNotRun { finish_reason: Option<FinishReason>, tool_names: Vec<String> }`, `#[non_exhaustive]`. Construction is only possible inside core. Tests outside core match it with `{ finish_reason, tool_names, .. }`.
- Produces: `pub(crate) fn cap_names(names: &[String]) -> String` (or the renamed existing helper) in `model.rs`.

- [ ] **Step 1: Write the failing tests** in that module:
  - `display_names_finish_reason_and_tools`: `ToolCallsNotRun { finish_reason: Some(FinishReason::Length), tool_names: vec!["search".into()] }.to_string()` contains `"Length"` and `"search"`.
  - `display_caps_long_name_lists`: 10 names → the text holds the first 8 names and not the 9th; a 100-character name appears cut to 64 characters.
  - `display_caps_multibyte_name_safely`: a name of 70 `"é"` characters does not panic and appears with at most 64 characters.
  - `display_for_missing_finish`: `finish_reason: None` → the text says that no finish reason came (exact words are the builder's call; assert on a stable substring that you choose, for example `"no finish reason"`).
- [ ] **Step 2:** Run `cargo test -p paigasus-helikon-core --lib display_` → FAIL (variant not defined).
- [ ] **Step 3:** Add the variant with its `///` doc (spec §3 wording: "The model turn did not end cleanly (`Length`, `ContentFilter`, or a stream with no `Finish`), so the loop did not run its tool calls."). Use a `#[error("...")]` that calls the cap helper; a `Some(r)` shows `{r:?}`, `None` shows the no-finish text.
- [ ] **Step 4:** Run the same command → PASS. Run `cargo test -p paigasus-helikon-core` → PASS (the existing `format_blank_names` tests still pass).
- [ ] **Step 5:** Commit `fix(core): SMA-864 add the ToolCallsNotRun agent error`.

### Task 2: The guard arm in `transition`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs` (a new arm after the max-turns arm and before the "Model produced tool calls" arm, ~line 286)
- Test: `crates/paigasus-helikon-core/tests/transition_unit.rs`

**Interfaces:**
- Consumes: `AgentError::ToolCallsNotRun` (Task 1).
- Produces: `transition(CallingModel, ModelResponse { finish_reason: Length | ContentFilter, items with ≥1 ToolCall })` → `next_state: LoopState::Failed(AgentError::ToolCallsNotRun { finish_reason: Some(r), tool_names })`, `next_action: NextAction::Terminate`, `conversation_appends: []`, events = `MessageOutput` per assistant item (item order), then `RunFailed { error: err.to_string() }`. No `ToolCallItem`.

- [ ] **Step 1: Write the failing tests** (use `ctx_with` and `assert_matches!` from the file; build a `TransitionCtx` with `handoffs: &[HandoffDef]` for test 4 as the existing handoff tests at ~line 652 do):
  1. `length_with_one_tool_call_fails_without_running_it` — the acceptance-criteria test. Assert the state matches `LoopState::Failed(AgentError::ToolCallsNotRun { finish_reason: Some(FinishReason::Length), ref tool_names, .. })` with `tool_names == ["search"]`; `NextAction::Terminate`; the last event is `RunFailed`; no event is `ToolCallItem`.
  2. `content_filter_with_one_tool_call_fails_without_running_it` — the same with `ContentFilter`.
  3. `length_with_three_tool_calls_runs_none` — calls with `call_id` `"a"`, `"b"`, `"c"` and names `"x"`, `"y"`, `"z"` → `tool_names == ["x","y","z"]`.
  4. `length_with_text_and_handoff_fails_without_handoff` — assistant text + a transfer call whose name matches a `HandoffDef` → state is `Failed`, not `ApplyingHandoff`; events are `[MessageOutput, RunFailed]` in that order.
  5. `length_without_tool_calls_still_completes` — text only + `Length` → `LoopState::Done`.
  6. `stop_with_tool_call_still_executes` — `Stop` + one call → `LoopState::ExecutingTools`.
  7. `other_function_call_with_tool_call_still_executes` — `FinishReason::Other("function_call".into())` + one call → `LoopState::ExecutingTools`.
- [ ] **Step 2:** Run `cargo test -p paigasus-helikon-core --test transition_unit` → tests 1-4 FAIL, tests 5-7 PASS.
- [ ] **Step 3:** Add the guard arm. Match `TransitionInput::ModelResponse { items, finish_reason: r @ (FinishReason::Length | FinishReason::ContentFilter), .. }` with a guard `items.iter().any(|i| matches!(i, Item::ToolCall { .. }))`. Collect `tool_names` from `Item::ToolCall { name, .. }` in item order.
- [ ] **Step 4:** Run the same command → all PASS. Run `cargo test -p paigasus-helikon-core` → PASS.
- [ ] **Step 5:** Commit `fix(core): SMA-864 do not run tool calls from a length or content-filter turn`.

### Task 3: The reassembly-error mapping in the local driver

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs` (`ModelTurnAccumulator`, ~line 680)
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (the `acc.finish()` error arm, ~line 1505)
- Test: `crates/paigasus-helikon-core/tests/tool_calls_not_run.rs`

**Interfaces:**
- Consumes: Task 1, Task 2.
- Produces (public, additive, each with `///`):
  - `ModelTurnAccumulator::finish_reason(&self) -> &FinishReason` — the reason of the last `Finish` seen (today's default `Stop` if none).
  - `ModelTurnAccumulator::tool_call_names(&self) -> Vec<String>` — the names of the accumulated tool calls in `call_id` order; a call with no name gives `""`.
  - Task 7 adds `finish_seen()`; do not add it here.
- Driver rule: read both accessors **before** `acc.finish()` consumes `acc`. On `Err(e)`: if the reason is `Length` or `ContentFilter`, set the failure slot to `ToolCallsNotRun { finish_reason: Some(reason), tool_names }` and yield `RunFailed { error: <that error>.to_string() }`; otherwise keep today's `AgentError::Other` and `RunFailed { error: e }`.

- [ ] **Step 1: Write the failing tests** (driver-level, with `common::MockModel::with_scripts`, `common::MockTool::new("search", json!({}))`, and `RunResultStreaming::new(stream).collect().await` as in `tests/failure_slot.rs:144`):
  1. `length_turn_with_empty_args_runs_no_tool` — script: `ToolCallDelta { call_id: "c1", name: Some("search"), args_delta: "" }`, `Finish { Length }`. Assert `tool.invocations().is_empty()` and the error matches `RunError::Agent(AgentError::ToolCallsNotRun { finish_reason: Some(FinishReason::Length), .. })`.
  2. `length_turn_with_partial_json_runs_no_tool` — the same with `args_delta: "{\"q\": \"ab"`.
  3. `reassembly_error_under_stop_keeps_other` — partial JSON + `Finish { Stop }` → `RunError::Agent(AgentError::Other(_))`.
  4. `cut_call_is_not_recorded_in_the_session` — test 1's run with a session (use the in-memory session the other core session tests use; find it with `grep -rn "InMemorySession\|SessionRecorder" crates/paigasus-helikon-core/tests`). After the run, the session holds no `tool_call` and no `tool_returned` record for `c1`. If test 1's script has assistant text, the assistant message is there.
- [ ] **Step 2:** Run `cargo test -p paigasus-helikon-core --test tool_calls_not_run` → test 1 PASS (Task 2 already covers it), tests 2 FAIL (gives `Other`), 3 PASS, 4 PASS or FAIL. Record which.
- [ ] **Step 3:** Add the two accessors and the driver rule.
- [ ] **Step 4:** Run the same command → all PASS. Run `cargo test -p paigasus-helikon-core` → PASS.
- [ ] **Step 5:** Commit `fix(core): SMA-864 map a cut-turn reassembly error to ToolCallsNotRun`.

### Task 4: The Temporal driver test

**Files:**
- Test: `crates/paigasus-helikon-runtime-temporal/src/driver.rs` (the `mod tests` at ~line 397; copy the `ModelTurn::new(.., FinishReason::ToolCalls)` setup at ~lines 568 and 651)

**Interfaces:**
- Consumes: Task 2 (no production change in this crate).

- [ ] **Step 1:** Write `length_turn_with_tool_call_finishes_failed_without_tools`: feed `apply_model` a `ModelTurnResult` of `ModelTurn::new(items_with_one_tool_call, usage, FinishReason::Length)`; loop `next_effect()` and assert it reaches `DriverEffect::Finished` with `RunStatusPayload::AgentFailed(ErrorKindPayload::Other { .. })`, and never yields `ExecuteTools`.
- [ ] **Step 2:** Run `cargo test -p paigasus-helikon-runtime-temporal --lib length_turn` → PASS (Task 2 is in place). Then temporarily comment out the Task 2 guard arm, rerun → FAIL, restore the arm, rerun → PASS. This proves that the test reaches the guard.
- [ ] **Step 3:** Commit `test(runtime-temporal): SMA-864 pin the cut-turn rule in the durable driver`.

### Task 5: Docs and upgrade notes

**Files:**
- Modify: `docs/book/src/concepts/agent-loop.md` (the "A typical run walks …" paragraph, ~line 49)
- Modify: `crates/paigasus-helikon-core/CHANGELOG.md` and `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md` (under `## [Unreleased]`, line 8, add `### Upgrade notes`)

- [ ] **Step 1:** Add to the book paragraph one or two sentences: a turn that ends with `Length` or `ContentFilter` and holds tool calls fails the run with `AgentError::ToolCallsNotRun`; no tool call or handoff from that turn runs.
- [ ] **Step 2:** Add the upgrade notes from spec §5 (three bullets: replay-breaking for in-flight Temporal runs with such a turn; drain or use a blue-green task queue first; the new failure instead of running the calls). Write them in short, plain sentences.
- [ ] **Step 3:** Check that release-plz keeps hand-written `[Unreleased]` text: `git log -p -S "### Upgrade notes" -- crates/paigasus-helikon-core/CHANGELOG.md | head -80` shows an earlier note that was added before a release and kept by the `chore: release` PR. Report what you found.
- [ ] **Step 4:** Run `mdbook build docs/book` (if `mdbook` is installed; else report that it is not) and `npx markdownlint-cli2` → clean.
- [ ] **Step 5:** Commit `docs(core): SMA-864 document the cut-turn rule and its upgrade notes`.

---

## Phase 2 — the final sweep (spec §10)

### Task 6: Provider stop-reason sweep

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/stream.rs` (`finish_or_error`, ~line 462)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/stream.rs` (~line 415; the test at ~lines 815-827)
- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs` (~line 747; the table test at ~lines 1008-1036)
- Review (change only if the sweep finds a value): `crates/paigasus-helikon-providers-gemini/src/stream.rs` (~line 102), `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` (~line 371), `crates/paigasus-helikon-providers-openai/src/backend/responses.rs` (~line 778)
- Modify: the spec, §10.1 (the sweep table)
- Modify: the `CHANGELOG.md` `[Unreleased]` of each provider crate that changes (an `### Upgrade notes` bullet)

**Interfaces:**
- Produces: Anthropic and Bedrock map `model_context_window_exceeded` → `FinishReason::Length`. LiteLLM maps `guardrail_intervened` → `FinishReason::ContentFilter`.

- [ ] **Step 1: Write or change the failing tests:** an Anthropic test that a `message_delta` with `stop_reason: "model_context_window_exceeded"` gives `Finish { Length }`; change the Bedrock test that pins `Other("model_context_window_exceeded")` to expect `Length`; add `("guardrail_intervened", FinishReason::ContentFilter)` to the LiteLLM table test and remove its old `Other` expectation.
- [ ] **Step 2:** Run `cargo test -p paigasus-helikon-providers-anthropic -p paigasus-helikon-providers-bedrock -p paigasus-helikon-providers-litellm` → the three tests FAIL.
- [ ] **Step 3:** Change the three mappings.
- [ ] **Step 4:** Sweep the other wire values. For each provider, list every stop-reason value the translator handles or that falls into its catch-all, from the vendor's own reference (Anthropic Messages `stop_reason`, Bedrock Converse `StopReason`, Gemini `FinishReason`, OpenAI Chat `finish_reason`, OpenAI Responses `incomplete_details.reason`, LiteLLM's mapped values). A value that clearly cuts the output becomes `Length`; a value that clearly is a filter, safety block or guardrail becomes `ContentFilter`; anything not clear stays. Write each decision as a row in the spec §10.1 table: value | provider | old | new | source. Add a test for each changed value.
- [ ] **Step 5:** Run the provider tests from Step 2 plus `-p paigasus-helikon-providers-gemini -p paigasus-helikon-providers-openai` → PASS.
- [ ] **Step 6:** Add the CHANGELOG upgrade-note bullets: "`<value>` now maps to `FinishReason::<X>` (was `Other`). A turn with tool calls and this stop reason now fails with `AgentError::ToolCallsNotRun`."
- [ ] **Step 7:** Commit `fix(providers): SMA-864 map cut and filtered stop reasons to length and content filter`.

### Task 7: A stream that ends with no `Finish`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs` (`ModelTurnAccumulator`: a `finish_seen: bool` field set in `observe` on `ModelEvent::Finish`; the check in `finish()`)
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (the Task 3 driver rule)
- Test: `crates/paigasus-helikon-core/src/model.rs` test module, `crates/paigasus-helikon-core/tests/tool_calls_not_run.rs`

**Interfaces:**
- Consumes: Task 3.
- Produces: `ModelTurnAccumulator::finish_seen(&self) -> bool` (public, `///`). `finish()` returns `Err(String)` when the turn has ≥1 tool call and `finish_seen` is `false`. The error text names the tool calls with the cap helper.
- Driver rule (extends Task 3): on `Err`, if `!finish_seen` → `ToolCallsNotRun { finish_reason: None, tool_names }`; else the Task 3 rule.

- [ ] **Step 1: Write the failing tests:**
  - `model.rs` unit `no_finish_with_tool_call_is_an_error`: observe one `ToolCallDelta` with full `"{}"` args, no `Finish` → `finish()` is `Err`.
  - `model.rs` unit `no_finish_text_only_still_stop`: observe one `TokenDelta`, no `Finish` → `Ok` with `finish_reason == Stop`.
  - `tests/tool_calls_not_run.rs` `stream_without_finish_runs_no_tool`: script `ToolCallDelta { "c1", Some("search"), "" }` and no `Finish` → `tool.invocations().is_empty()` and `RunError::Agent(AgentError::ToolCallsNotRun { finish_reason: None, .. })`.
- [ ] **Step 2:** Run `cargo test -p paigasus-helikon-core no_finish` and `cargo test -p paigasus-helikon-core stream_without_finish` (cargo takes one test filter per run) → the two tool-call tests FAIL.
- [ ] **Step 3:** Add `finish_seen`, the check in `finish()`, and the driver rule.
- [ ] **Step 4:** Run `cargo test --workspace --all-features`. Any test that now fails because its mock or provider sends tool calls with no `Finish`: fix the mock script (add the `Finish` it should have had) or, for a real provider, emit `Finish` where the stream contract requires it. List each changed site in the task report. Do **not** weaken the new rule to make a test pass.
- [ ] **Step 5:** Run `cargo test --workspace --all-features` → PASS.
- [ ] **Step 6:** Add a core CHANGELOG upgrade-note bullet: "A model stream that ends with no `Finish` event and holds tool calls now fails with `AgentError::ToolCallsNotRun { finish_reason: None, .. }`. A custom `Model` must emit `Finish`." Add one sentence to the book paragraph from Task 5.
- [ ] **Step 7:** Commit `fix(core): SMA-864 refuse tool calls from a stream with no finish event`.

### Task 8: Full gate

- [ ] **Step 1:** Run, in order, and record each result:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-features --all-targets -- -D warnings`
  - `cargo test --workspace --all-features`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
  - `cargo build -p paigasus-helikon --no-default-features`
  - `npx markdownlint-cli2`
  - `bash scripts/check-breaking-cascade.sh`
- [ ] **Step 2:** If a gate fails, fix the cause and commit with the type of the task that owns the code. Report every gate with its exit status.
