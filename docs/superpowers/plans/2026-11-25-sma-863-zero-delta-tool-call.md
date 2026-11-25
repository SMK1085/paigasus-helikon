# SMA-863 zero-delta tool call — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The `anthropic` and `bedrock` stream translators emit one named `ToolCallDelta` for a tool call that has a start event and no input delta, at the block stop, with a safety net before `Finish(ToolCalls)`.

**Architecture:** Each translator keeps per-block state. The block-stop arm sends the name when no delta sent it. A terminal helper flushes unnamed tool blocks before `Finish(ToolCalls)`. Both `finish()` methods change from `Option<…>` to `Vec<Result<ModelEvent, ModelError>>`, so the flushed deltas and the terminal event can go out together.

**Tech Stack:** Rust 1.94, `serde_json`, `aws-sdk-bedrockruntime`, `tracing`.

**Spec:** `docs/superpowers/specs/2026-11-25-sma-863-zero-delta-tool-call-design.md`

## Global Constraints

- Worktree: `/Users/smaschek/dev/paigasus/paigasus-helikon/.worktrees/sma-863`. Run every command there. Never run `git checkout`, `git switch`, `git reset`, `git stash` or `git rebase`. Never `git add -A`; add explicit paths only.
- Work synchronously in the foreground. Do not background `cargo`. Do not end a task until each command has a final result.
- Commit message form: `fix(providers): SMA-863 <lowercase subject>` (or `test(providers): …` for a test-only commit). End with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Commits are signed via 1Password; if a commit fails with "failed to fill whole buffer", stop and report it. Do not bypass signing.
- No version bump, no CHANGELOG edit (release-plz does that), no README or mdBook edit.
- Zero-delta `args_delta` is `""` (bedrock always; anthropic unless the start `input` is a JSON object with at least one key, then `serde_json::to_string(&input)`).
- Log lines in new paths carry only the block index and `call_id`. Block-stop emit: `tracing::debug!`. Safety net: `tracing::warn!`. Targets: `"paigasus::anthropic::stream"` and `"paigasus::bedrock::stream"`.
- Each new test's doc comment states: `HAND-AUTHORED: no capture shows this wire shape. Derived from tests/provider-stream-conformance/fixtures/<anthropic|bedrock>/zero_arg_tool_call.raw (SMA-860) with the one empty input delta removed` — and for safety-net tests add `and the block stop removed`.
- Before each commit: `cargo fmt --all` and `cargo clippy -p <crate> --all-targets -- -D warnings` clean.

## Review Focus

1. Parallel calls where one has a delta and one has none: each name exactly once, the delta-less one at its own block stop. → Task 2 test 7 and Task 5 test 7.
2. Anthropic start `input` with keys and also input deltas: the deltas carry the arguments, the start input is not added a second time. → Task 2 test `start_input_is_ignored_when_deltas_follow`.
3. Anthropic start `input` that is not an object (`null`, a string, `[]`): `args_delta` is `""`. → Task 2 test `non_object_start_input_gives_blank_args`.
4. Bedrock input delta after its own `ContentBlockStop` (malformed): it still appends with `name: None`, no second name, because the stop does not remove the block. → Task 5 test `delta_after_block_stop_carries_no_name`.
5. A block flushed by the safety net and then a late block stop after `Finish`: nothing more is emitted. → Task 3 test `late_block_stop_after_net_flush_emits_nothing` and Task 6 test of the same name.

---

### Task 1: anthropic — `finish()` returns a `Vec` (no behaviour change)

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/stream.rs` (`MessageStop` arm ~L230-237, `finish()` ~L339-368, tests at ~L719, 769, 789, 805, 826, 829, 845, 882, 904, 1425)
- Modify: `crates/paigasus-helikon-providers-anthropic/src/model.rs:119-121`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/mod.rs:980, 1373`

**Interfaces:**
- Produces: `pub(crate) fn finish(&mut self) -> Vec<Result<ModelEvent, ModelError>>` (empty when nothing to flush, idempotent).
- Produces: `fn terminal_events(&mut self, reason: &str) -> Vec<Result<ModelEvent, ModelError>>` — sets `terminal_emitted = true` and returns `vec![self.finish_or_error(reason)]`. Both the `MessageStop` arm and `finish()` call it. Task 3 adds the net inside it.

- [ ] **Step 1:** Add `terminal_events`, route the `MessageStop` arm and `finish()` through it, change `finish()`'s return type. Keep the `tracing::warn!` in `finish()`. Rewrite `finish()`'s doc: "Returns an empty `Vec` when …" in place of "Returns `None` when …".
- [ ] **Step 2:** `model.rs`: replace `if let Some(terminal) = translator.finish() { yield terminal; }` with `for ev in translator.finish() { yield ev; }`.
- [ ] **Step 3:** Update every test and `translate/mod.rs` caller: `.is_none()` → `.is_empty()`; `match t.finish().expect(..)` → take the single element (`let v = t.finish(); assert_eq!(v.len(), 1); match v.into_iter().next().unwrap()`); `if let Some(last) = t.finish()` → `for last in t.finish()`; `out.extend(t.finish().map(Result::unwrap))` → `out.extend(t.finish().into_iter().map(Result::unwrap))`. Keep the idempotence assertion at ~L829.
- [ ] **Step 4:** Run `cargo test -p paigasus-helikon-providers-anthropic` — expected: all pass, same test count as before.
- [ ] **Step 5:** Commit `refactor(providers): SMA-863 return a vec from the anthropic stream finish` (`refactor` is in `.versionrc`, with no version increment).

### Task 2: anthropic — emit the name at `content_block_stop`

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/stream.rs` (`BlockState::ToolUse`, `ContentBlockStart` arm ~L122-138, `stop_block` `ToolUse` arm ~L289-299, tests)
- Modify: `crates/paigasus-helikon-providers-anthropic/src/sse.rs:72-74` (remove `#[allow(dead_code)]` from `input`)

**Interfaces:**
- Consumes: Task 1's `terminal_events`.
- Produces: `BlockState::ToolUse { call_id: String, name: String, name_emitted: bool, start_args: String, synthesized: bool }`.
- Produces: `fn start_args(input: &serde_json::Value) -> String` (free fn in `stream.rs`): `serde_json::to_string(input)` when `input` is an object with ≥1 key, else `String::new()`.
- Produces: `fn emit_unnamed(call_id: &str, name: &str, start_args: &str) -> ModelEvent` or an equivalent method — the one place that builds the zero-delta `ToolCallDelta { call_id, name: Some(name), args_delta: start_args }`. Task 3 reuses it.

- [ ] **Step 1: Write the failing tests** (in `mod tests`; add a helper `tool_head_with_input(id, name, input: serde_json::Value)` and `message_delta(reason: &str)` / `message_stop()` helpers if absent):
  1. `tool_use_without_input_json_delta_emits_name_at_block_stop`: `message_start`, `start(0, tool_head("tu_1","get_current_time"))`, `stop(0)`, `message_delta("tool_use")`, `MessageStop`. Assert the `ToolCallDelta`s are exactly `[("tu_1", Some("get_current_time"), "")]`, and the last event is `Finish { reason: ToolCalls }`, and the delta comes before it.
  2. `captured_empty_delta_still_gives_one_name`: as 1 with `json_delta(0, "")` before `stop(0)`. Exactly one `ToolCallDelta` with `name: Some(..)`.
  3. `block_stop_with_max_tokens_gives_named_call_then_length`: as 1 with `message_delta("max_tokens")`. One named delta, then `Finish { reason: Length }`.
  4. `synthesized_tool_without_delta_emits_no_tool_call_delta`: `MessageTranslator::new(true)`, `start(0, tool_head("tu_s", SYNTHESIZED_TOOL_NAME))`, `stop(0)`, `message_delta("tool_use")`, `MessageStop`. No `ToolCallDelta`, no `TokenDelta`, last event `Finish { reason: Stop }`.
  5. `start_input_with_keys_is_sent_as_args`: `tool_head_with_input("tu_1","search", json!({"q":"x"}))`, `stop(0)`. One delta with `args_delta == r#"{"q":"x"}"#`.
  6. `start_input_is_ignored_when_deltas_follow`: start input `{"q":"x"}`, then `json_delta(0, r#"{"q":"y"}"#)`, `stop(0)`. Concatenated `args_delta` of all deltas for `tu_1` is `{"q":"y"}`; one name.
  7. `parallel_calls_one_without_delta`: block 0 `tu_a` with `json_delta(0,"{}")`, block 1 `tu_b` with no delta, `stop(0)`, `stop(1)`. Names in order `["tu_a"→a, "tu_b"→b]`, each once.
  8. `non_object_start_input_gives_blank_args`: start input `json!(null)` and, in a second translator, `json!("x")`. Each delta has `args_delta == ""`.
  9. Rename `tool_use_without_input_json_delta_gets_no_position` → `tool_use_without_input_json_delta_gets_a_position`; expected `provider_data` becomes `[(block(0, "", "SIG_FAKE_1"), None), (AnthropicData::ToolUse { index: 1 }, Some("tu_1".to_owned()))]`.
- [ ] **Step 2:** Run `cargo test -p paigasus-helikon-providers-anthropic stream::tests` — expected: tests 1, 3, 5, 7, 8 and 9 FAIL; tests 2, 4 and 6 pass.
- [ ] **Step 3:** Implement: fill `start_args` and `synthesized` at `ContentBlockStart` (`synthesized = self.synthesizing_output && name == SYNTHESIZED_TOOL_NAME`). In `stop_block`'s `ToolUse` arm, when `!name_emitted && !synthesized`, push the zero-delta event and a `tracing::debug!(index, %call_id, "tool_use block closed with no input delta; emitting its name")`, then treat it as emitted for the position rule. Replace the `Some(index) != self.synthesized_tool_index` check in that arm with `!synthesized`.
- [ ] **Step 4:** Run `cargo test -p paigasus-helikon-providers-anthropic` — expected: all pass.
- [ ] **Step 5:** Run `cargo test -p paigasus-helikon-provider-stream-conformance` — expected: all pass.
- [ ] **Step 6:** Commit `fix(providers): SMA-863 emit the anthropic tool name at block stop when no input delta came`.

### Task 3: anthropic — safety net before `Finish(ToolCalls)`

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/stream.rs` (`terminal_events`, tests)

**Interfaces:**
- Consumes: Task 1 `terminal_events`, Task 2 `BlockState::ToolUse` fields and the zero-delta builder.

- [ ] **Step 1: Write the failing tests:**
  1. `net_names_unclosed_tool_block_on_message_stop`: `start(0, tool_head("tu_1","t"))`, `message_delta("tool_use")`, `MessageStop` (no `stop(0)`). Events: one named `ToolCallDelta` `("tu_1", "")`, then `Finish(ToolCalls)` last.
  2. `net_names_unclosed_tool_block_on_finish`: same without `MessageStop`; `t.finish()` returns exactly `[ToolCallDelta(tu_1, Some("t"), ""), Finish(ToolCalls)]`.
  3. `net_does_not_fire_for_other_stop_reasons`: unclosed block, `message_delta("max_tokens")`, `MessageStop` → no `ToolCallDelta`; `Finish(Length)`.
  4. `net_names_two_unclosed_blocks_in_index_order`: starts at index 2 (`tu_b`) then 1 (`tu_a`), no deltas, no stops → names emitted `tu_a` then `tu_b`, then `Finish`.
  5. `net_skips_a_block_that_already_has_its_name`: block 0 with `json_delta(0,"{}")` and no stop, block 1 with nothing → exactly one new named delta (`tu_1`'s block 1) from the net; total named deltas 2.
  6. `finish_twice_after_net_flush_is_empty`: test 2's stream, then a second `t.finish()` is empty.
  7. `late_block_stop_after_net_flush_emits_nothing`: test 1's stream, then `t.consume(stop(0))` gives `[]`.
- [ ] **Step 2:** Run the anthropic tests — expected: 1, 2, 4, 5 FAIL; 3, 6, 7 pass.
- [ ] **Step 3:** Implement in `terminal_events`: compute the result of `finish_or_error`; if it is `Ok(Finish { reason: ToolCalls })`, collect the indices of `ToolUse` blocks with `!name_emitted && !synthesized`, sort ascending, emit the zero-delta event for each (set `name_emitted`), log `tracing::warn!(index, %call_id, "terminal tool_use with an unclosed block that sent no name; emitting its name")`. Then push the terminal result. No position record for these blocks.
- [ ] **Step 4:** Run `cargo test -p paigasus-helikon-providers-anthropic` and the conformance package — expected: all pass.
- [ ] **Step 5:** Commit `fix(providers): SMA-863 name unclosed anthropic tool blocks before finish`.

### Task 4: bedrock — `finish()` returns a `Vec` and a terminal guard

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/stream.rs` (struct fields, `ContentBlockStart` refused-call branch ~L150-162, `finish()` ~L290-306, `finish_events` ~L315, tests at ~L799, 886, 907, 998-1042)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/model.rs:157-159`

**Interfaces:**
- Produces: `pub(crate) fn finish(&mut self) -> Vec<Result<ModelEvent, ModelError>>` — `match self.pending_stop_reason.take() { Some(r) => self.finish_events(r), None => Vec::new() }`.
- Produces: `fn finish_events(&mut self, reason: StopReason) -> Vec<…>` (now `&mut self`); sets `self.terminal_emitted = true` when it returns a non-empty `Vec`.
- Produces: field `terminal_emitted: bool` on `StreamTranslator`; also set to `true` where the refused tool call pushes its `Err`.

- [ ] **Step 1: Write the failing tests:**
  1. `terminal_emitted_is_set_by_finish`: a text turn through `MessageStop` + `Metadata`; assert `t.terminal_emitted` is `true` (unit test inside the module, so the private field is visible).
  2. `terminal_emitted_is_set_by_refused_tool_call`: `StreamTranslator::new(false).forbid_tool_calls(true)`, `tool_use_start(0,"tu","t")`; assert `t.terminal_emitted`.
  3. `terminal_emitted_is_set_by_both_tools_error`: synthesizing mode, synthesized start at 0, real start at 1, `message_stop(ToolUse)`, `metadata(..)`; the last event is `Err`, and `t.terminal_emitted` is `true`.
- [ ] **Step 2:** Run `cargo test -p paigasus-helikon-providers-bedrock` — expected: compile FAIL (no field).
- [ ] **Step 3:** Implement the field, the `finish()`/`finish_events` signature changes, and `model.rs` `for ev in translator.finish() { yield ev; }`. Update the existing `finish()` tests: `.is_none()` → `.is_empty()`, `.expect(..)` → take the single element after asserting `len() == 1`. Rewrite `finish()`'s doc ("Returns an empty `Vec` …").
- [ ] **Step 4:** Run `cargo test -p paigasus-helikon-providers-bedrock` — expected: all pass.
- [ ] **Step 5:** Commit `fix(providers): SMA-863 return a vec from the bedrock stream finish and track the terminal event`.

### Task 5: bedrock — emit the name at `ContentBlockStop`

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/stream.rs` (`BlockState::ToolUse`, `ContentBlockStart` ~L140-178 incl. the stale comment at ~L163-168, `ContentBlockStop` arm L132, test `content_block_stop_produces_no_events` ~L973-978, new tests)

**Interfaces:**
- Consumes: Task 4 `terminal_emitted`.
- Produces: `BlockState::ToolUse { call_id: String, name: String, name_emitted: bool, synthesized: bool }`.
- Produces: `fn emit_unnamed(&mut self, idx: i32) -> Option<ModelEvent>` (or equivalent) — returns the zero-delta `ToolCallDelta { call_id, name: Some(name), args_delta: String::new() }` and sets `name_emitted` when the block at `idx` is `ToolUse` with `!name_emitted && !synthesized`. Task 6 reuses it.

- [ ] **Step 1: Write the failing tests:**
  1. `tool_use_without_delta_emits_name_at_block_stop`: `message_start`, `tool_use_start(0,"tu_1","get_current_time")`, `content_block_stop(0)`, `message_stop(StopReason::ToolUse)`, `metadata(10, 5, None)`. Named deltas `["get_current_time"]` with `args_delta == ""`; the order is `ToolCallDelta`, `Usage`, `Finish(ToolCalls)`.
  2. `captured_empty_delta_still_gives_one_name`: as 1 with `tool_delta(0, "")` before the stop. `tool_call_names(&out) == ["get_current_time"]`.
  3. `block_stop_with_max_tokens_gives_named_call_then_length`: as 1 with `StopReason::MaxTokens`. One name; last event `Finish(Length)`.
  4. `synthesized_tool_without_delta_emits_no_tool_call_delta`: synthesizing mode, `tool_use_start(0,"tu_s",SYNTHESIZED_TOOL_NAME)`, stop, `message_stop(ToolUse)`, `metadata`. No `ToolCallDelta`; `Finish(Stop)`.
  5. `repeated_block_stop_gives_one_name`: two `content_block_stop(0)` → one named delta.
  6. `block_stop_after_terminal_emits_nothing`: test 1's stream without the stop, then `content_block_stop(0)` after `metadata` → that call returns `[]`. Second case: the both-tools `Err` stream from Task 4 test 3, then `content_block_stop(1)` → `[]`.
  7. `parallel_calls_one_without_delta`: block 0 with `tool_delta(0,"{}")`, block 1 with none, stops for both → `tool_call_names == ["a","b"]`, each once.
  8. `forbid_tool_calls_block_stop_after_refusal_emits_nothing`: `forbid_tool_calls(true)`, `tool_use_start(0,..)`, `content_block_stop(0)` → only the `Err` from the start.
  9. `delta_after_block_stop_carries_no_name`: start, stop, then `tool_delta(0, "{}")` → the late delta has `name: None`; one named delta in total.
  10. Rename `content_block_stop_produces_no_events` → `content_block_stop_of_unknown_block_produces_no_events` and keep its body.
- [ ] **Step 2:** Run the bedrock tests — expected: 1, 3, 6(first case), 7 FAIL at least; the others may pass.
- [ ] **Step 3:** Implement: set `synthesized` at `ContentBlockStart`; in the `ContentBlockStop` arm, return early when `terminal_emitted`, else push `emit_unnamed(idx)` with `tracing::debug!(idx, %call_id, "toolUse block closed with no input delta; emitting its name")`. Do not remove the block. Rewrite the stale comment at ~L163-168 to say the name goes out with the first input delta, or at the block stop when no delta came (SMA-863).
- [ ] **Step 4:** Run `cargo test -p paigasus-helikon-providers-bedrock` and the conformance package — expected: all pass.
- [ ] **Step 5:** Commit `fix(providers): SMA-863 emit the bedrock tool name at block stop when no input delta came`.

### Task 6: bedrock — safety net before `Finish(ToolCalls)`

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/stream.rs` (`finish_events`, module doc L3-24, tests)

**Interfaces:**
- Consumes: Task 4 `finish_events(&mut self, ..)`, Task 5 `emit_unnamed`.

- [ ] **Step 1: Write the failing tests:**
  1. `net_names_unclosed_block_metadata_after_message_stop`: start, `message_stop(ToolUse)`, `metadata` (no stop). Order: `Usage`, `ToolCallDelta(name)`, `Finish(ToolCalls)`.
  2. `net_names_unclosed_block_message_stop_after_metadata`: start, `metadata`, `message_stop(ToolUse)`. Order: `Usage`, `ToolCallDelta`, `Finish(ToolCalls)`.
  3. `net_names_unclosed_block_on_finish`: start, `message_stop(ToolUse)`, then `t.finish()` returns exactly `[ToolCallDelta, Finish(ToolCalls)]`.
  4. `net_does_not_fire_for_other_stop_reasons`: start, `message_stop(EndTurn)`, `metadata` → no `ToolCallDelta`.
  5. `net_names_two_unclosed_blocks_in_index_order`: starts at 2 then 1 → names in index order 1, 2.
  6. `net_skips_a_block_that_already_has_its_name`: block 0 with a delta, block 1 without, no stops → named deltas total 2, each name once.
  7. `finish_twice_after_net_flush_is_empty`: test 3 then a second `finish()` is empty.
  8. `late_block_stop_after_net_flush_emits_nothing`: test 1 then `content_block_stop(0)` → `[]`.
- [ ] **Step 2:** Run the bedrock tests — expected: 1, 2, 3, 5, 6 FAIL.
- [ ] **Step 3:** Implement in `finish_events`: when the computed terminal is `Ok(Finish { reason: ToolCalls })`, first push `emit_unnamed(idx)` for each `ToolUse` block with no name sent, indices sorted ascending, each with `tracing::warn!(idx, %call_id, "terminal tool_use with an unclosed block that sent no name; emitting its name")`. Update the module doc: on the `Metadata` path the order is `Usage`, any safety-net `ToolCallDelta`s, then `Finish`; the core contract allows `Usage` anywhere and only requires `Finish` to be terminal.
- [ ] **Step 4:** Run `cargo test -p paigasus-helikon-providers-bedrock` and the conformance package — expected: all pass.
- [ ] **Step 5:** Commit `fix(providers): SMA-863 name unclosed bedrock tool blocks before finish`.

### Task 7: full gate run

**Files:** none (fix only what a gate reports, in the owning crate).

- [ ] **Step 1:** `cargo fmt --all -- --check` — expected: no output.
- [ ] **Step 2:** `cargo clippy --workspace --all-features --all-targets -- -D warnings` — expected: exit 0.
- [ ] **Step 3:** `cargo test --workspace --all-features` — expected: 0 failed.
- [ ] **Step 4:** `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps` — expected: exit 0 (watch for a `pub` doc that links to a private item).
- [ ] **Step 5:** `cargo build -p paigasus-helikon-providers-anthropic --no-default-features` and the same for bedrock — expected: exit 0.
- [ ] **Step 6:** `npx markdownlint-cli2` — expected: 0 errors (covers the spec and this plan).
- [ ] **Step 7:** If a fix was needed, commit it with `fix(providers): SMA-863 <what>`; otherwise no commit.
