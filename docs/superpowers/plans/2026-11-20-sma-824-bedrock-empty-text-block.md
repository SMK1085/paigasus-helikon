# SMA-824 Bedrock empty text block Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Bedrock Converse translator stops sending blank text blocks,
keeps tool rounds in item order, and fails early when a dropped final user
message leaves the request ending with an assistant message.

**Architecture:** All logic changes are in `items_to_messages` in
`crates/paigasus-helikon-providers-bedrock/src/translate/request.rs`. It
filters blank text where it makes the blocks, ports `round_is_answered` from
the Anthropic translator, and returns `Err` directly for the two early-error
cases. `build_request` (`translate/mod.rs`) already propagates the error with
`?`, and `invoke` (`model.rs:49`) calls `build_request` before any network
call.

**Tech Stack:** Rust, `aws-sdk-bedrockruntime` types (`Message`,
`ContentBlock`, `ConversationRole`, `SystemContentBlock`), `insta` snapshots,
`tokio` tests.

**Spec:** `docs/superpowers/specs/2026-11-20-sma-824-bedrock-empty-text-block-design.md`

## Global Constraints

- "Blank" means `text.trim().is_empty()` (Rust `str::trim`). A text that is not
  blank is sent byte for byte, with its whitespace.
- No placeholder text is ever sent in place of a dropped block.
- No change outside `crates/paigasus-helikon-providers-bedrock/` except the
  spec and this plan. No change to `paigasus-helikon-core`. No hand version
  bump.
- Every `tracing` macro uses `target: "paigasus::bedrock::translate"`. A debug
  line about drops contains counts only, never text.
- Commit prefix: `fix(providers): SMA-824 <lowercase subject>`; test-only
  commits use `test(providers): SMA-824 ...`. End each commit message with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Before each commit: `cargo fmt --all` and
  `cargo clippy -p paigasus-helikon-providers-bedrock --all-features --all-targets -- -D warnings`.
- Unit tests compare the full `messages` wire array with `assert_eq!` (use the
  existing `wire()` helper and `messages_to_wire_json`), not only lengths.
- Error text case 1 (no message remains):
  `"no message with content to send (empty or whitespace-only text and unsupported parts are not sent)"`.
- Error text case 2 (drop-induced final assistant):
  `"the last user message had only empty or whitespace-only text, which is not sent; the request would end with an assistant message"`.

## Review Focus

1. A tool round whose results arrive in a different order from the calls
   (`t1, t2, r2, r1`) must stay one round: one assistant turn, one user turn.
   Test added to Task 2.
2. A blank `Item::System` between a `ToolResult` and a real `UserMessage` must
   not change the user turn that today's code makes for that pair. Test added
   to Task 1.
3. A user message that has a nested `ContentPart::ToolResult` and blank text
   keeps the tool result and does not set the case-2 flag. Test added to Task 3.
4. Unicode whitespace (`"\u{00A0}\u{2003}"`) counts as blank. Test added to
   Task 1.
5. A caller prefill with trailing whitespace (`[user q, assistant "Sure.\n"]`)
   is sent with its text unchanged (no trim, no error). Test added to Task 3.

---

### Task 1: Drop blank text and blank system, remove the placeholder

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/request.rs`
  (module doc 1-13, field doc 31, fn doc 35-38, `System` arm 90-97,
  `UserMessage` arm 105-107, `AssistantMessage` arm 147-149, empty guard
  234-239, placeholder 241-249, tests)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs`
  (field doc line 33 "Strictly-alternating", `# Errors` list 85-89)

**Interfaces:**
- Produces: `fn is_blank(text: &str) -> bool` (private, in `request.rs`), used
  by Tasks 2 and 3. `items_to_messages` keeps its signature
  `pub(crate) fn items_to_messages(items: &[Item]) -> Result<TranslatedMessages, ModelError>`.

- [ ] **Step 1: Write the failing tests** in `request.rs` `mod tests`:
  - `empty_text_part_is_dropped`: `[user [text "q"], assistant [text "a"], user [text ""], user [text "r"]]`
    → messages `[user[q], assistant[a], user[r]]`.
  - `whitespace_only_text_part_is_dropped`: same with `"  \n\t"`.
  - `unicode_whitespace_text_part_is_dropped`: same with `"\u{00A0}\u{2003}"`.
  - `blank_text_is_dropped_but_real_text_kept_verbatim`: `[user [text "", text "  hi  "]]`
    → `[user[text "  hi  "]]`.
  - `blank_user_text_keeps_pending_tool_results`:
    `[user q, assistant [ToolUse tu_1], ToolResult tu_1 "pong", user [text ""]]`
    → `[user[q], assistant[tool_use tu_1], user[tool_result tu_1 "pong"]]`.
  - `blank_assistant_text_is_dropped`: `[user q, assistant [text "  ", text "a"]]`
    → `[user[q], assistant[a]]`.
  - `assistant_with_no_blocks_is_not_sent`: `[user q, assistant [text ""], user r]`
    → `[user[q, r]]` (one joined user turn).
  - `replayed_blank_user_between_real_turns_is_dropped`:
    `[user q, assistant a, user "", user r]` → `[user[q], assistant[a], user[r]]`.
  - `blank_first_user_gives_assistant_first`: `[user "", assistant a, user b]`
    → `[assistant[a], user[b]]`.
  - Rename `assistant_first_gets_synthetic_user_prepended` to
    `assistant_first_is_sent_without_placeholder`: `[assistant [text "done"]]`
    → messages `[assistant[done]]`.
  - Change `tool_call_item_queues_onto_assistant_turn`: messages length 1, the
    only message is the assistant `tool_use tu_1`.
  - `whitespace_only_system_is_absent`: `[system "  \n", user hi]` → `tm.system`
    is empty; `[system ""]` alone and `[system "  "] , user ""` both return
    `ModelError::Other` whose `to_string()` contains `"no message with content to send"`.
  - `blank_system_inside_open_round_still_flushes`:
    `[user q, assistant [ToolUse tu_1], ToolResult tu_1 "r", system " ", user "next"]`
    → messages equal to the result for the same items with `system "s"` in place
    of `system " "`, and `tm.system` is empty for the blank one.
  - `only_blank_user_returns_error`: `[user ""]` → `Err(ModelError::Other)`
    containing the case-1 text.

- [ ] **Step 2: Run the tests and see them fail**

  Run: `cargo test -p paigasus-helikon-providers-bedrock --lib translate::request`
  Expected: the new tests FAIL (blank blocks present, placeholder present).
  If many unrelated tests fail with a native-roots / TLS root error, that is
  the known macOS checkout-path problem; report it, do not change code for it.

- [ ] **Step 3: Implement**
  - Add `is_blank`. In the `UserMessage` and `AssistantMessage` arms, skip a
    `ContentPart::Text` when `is_blank(text)`.
  - In the `System` arm: keep both flush calls; push the
    `SystemContentBlock::Text` only when `!is_blank(&text)`.
  - Delete the placeholder block (lines 241-249).
  - Replace the empty-guard message with the case-1 text.
  - Update the doc sites: module doc (remove "first message must be user" and
    "strictly alternating"; state that blank text is not sent and that adjacent
    same-role turns are joined), field doc line 31, the `items_to_messages`
    `# Errors` text, `mod.rs:33`, and the `build_request` `# Errors` list
    ("No message with content after empty/whitespace-only text is dropped").

- [ ] **Step 4: Run the tests and see them pass**

  Run: `cargo test -p paigasus-helikon-providers-bedrock`
  Expected: PASS, including all `insta` snapshots unchanged. If a snapshot
  changes, stop and report which one and why; do not accept it blindly.

- [ ] **Step 5: Commit**

  `git add crates/paigasus-helikon-providers-bedrock/src/translate/request.rs crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs`
  `git commit -m "fix(providers): SMA-824 drop blank text and system blocks in the bedrock translator"`

### Task 2: Keep tool rounds in item order

The "empty assistant is absent" change and the `round_is_answered` port go in
this one commit (spec: the reasoning-only loop is correct only with both).

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/request.rs`
  (`AssistantMessage` arm 141-195, `ToolCall` arm 197-211, tests)

**Interfaces:**
- Consumes: `is_blank` from Task 1.
- Produces: `fn round_is_answered(messages: &[Message], results: &[ContentBlock]) -> bool`
  (private). `true` when every `ToolUse` id in the trailing run of assistant
  messages has a `ToolResult` with the same `tool_use_id` in `results`; also
  `true` when the last message is not an assistant message. Port of the
  Anthropic function in
  `crates/paigasus-helikon-providers-anthropic/src/translate/request.rs`.

- [ ] **Step 1: Write the failing tests** (Bedrock expected arrays; names follow
  the SMA-819 tests in the Anthropic `request.rs`):
  - `tool_call_after_pending_result_does_not_fold_into_earlier_assistant`:
    `[user q, assistant a, ToolCall t1, ToolResult r1, ToolCall t2, ToolResult r2]`
    → `[user[q], assistant[a, t1], user[r1], assistant[t2], user[r2]]`.
  - `tool_call_after_pending_result_at_end_of_input`:
    `[user q, assistant a, ToolCall t1, ToolResult r1, ToolCall t2]`
    → `[user[q], assistant[a, t1], user[r1], assistant[t2]]`.
  - `tool_call_after_pending_result_at_system`: the same items plus
    `system "s"` → same messages; `tm.system == [s]`.
  - `tool_call_after_pending_result_at_user_message`: the same items plus
    `user "x"` → `[user[q], assistant[a, t1], user[r1], assistant[t2], user[x]]`.
  - `multi_round_tool_loop_without_assistant_text_alternates`:
    `[user q, t1, r1, t2, r2, assistant done]`
    → `[user[q], assistant[t1], user[r1], assistant[t2], user[r2], assistant[done]]`.
  - `reasoning_only_multi_round_loop_alternates`:
    `[user q, assistant[Reasoning], t1, r1, assistant[Reasoning], t2, r2, assistant done]`
    → the same array as the previous test.
  - `whitespace_assistant_before_tool_call_keeps_order`:
    `[user q, assistant "  ", t1, r1, assistant done]`
    → `[user[q], assistant[t1], user[r1], assistant[done]]`.
  - `interleaved_parallel_round_is_not_split`:
    `[user q, assistant[ToolUse t1, ToolUse t2], r1, ToolCall t3, r2, r3]`
    → `[user[q], assistant[t1, t2, t3], user[r1, r2, r3]]`.
  - `out_of_order_results_keep_one_round` (Review Focus 1):
    `[user q, assistant[ToolUse t1, ToolUse t2], r2, r1, assistant done]`
    → `[user[q], assistant[t1, t2], user[r2, r1], assistant[done]]`.
  - `dropped_assistant_inside_open_round_does_not_split_it`:
    `[user q, assistant[ToolUse t1, ToolUse t2], r1, assistant "", r2, assistant done]`
    → `[user[q], assistant[t1, t2], user[r1, r2], assistant[done]]`.
  - `dropped_assistant_between_result_and_user_keeps_one_user_turn`:
    `[user q, assistant[ToolUse t1], r1, assistant[Reasoning], user next]`
    → `[user[q], assistant[t1], user[r1, next]]`.
  - `assistant_message_flushes_calls_before_results`:
    `[user q, assistant a, t1, t2, r1, t3, assistant b, r2, r3]`
    → `[user[q], assistant[a, t1, t2, t3], user[r1], assistant[b], user[r2, r3]]`.
    (The input is malformed: `b` comes inside an open round. Converse rejects
    both possible orders. The test pins that late calls join the open round,
    as at the other flush sites.)

  Here `t1` is `Item::ToolCall` unless the array shows `ToolUse` inside an
  assistant message, and `r1` is `Item::ToolResult { call_id: "t1", content: [text "r1"] }`.

- [ ] **Step 2: Run the tests and see them fail**

  Run: `cargo test -p paigasus-helikon-providers-bedrock --lib translate::request`
  Expected: the fold, the open-round and the calls-first tests FAIL.

- [ ] **Step 3: Implement**
  - `AssistantMessage` arm: build the blocks first. When the blocks are empty,
    do nothing (no flush). When not empty: `flush_assistant`, then
    `flush_user_results`, then push or coalesce as today.
  - `ToolCall` arm: before it queues the block, call
    `flush_user_results` when `!pending_tool_results.is_empty()` and
    `round_is_answered(&messages, &pending_tool_results)`.
  - Make `flush_user_results` coalesce into a trailing user message, the same
    way `flush_assistant` coalesces into a trailing assistant message.

- [ ] **Step 4: Run the tests and see them pass**

  Run: `cargo test -p paigasus-helikon-providers-bedrock`
  Expected: PASS; snapshots unchanged.

- [ ] **Step 5: Commit**

  `git commit -m "fix(providers): SMA-824 keep bedrock tool rounds in item order"`

### Task 3: Fail early on a drop-induced final assistant message

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/request.rs`
  (loop state, all four conversation arms, end-of-function checks, tests)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs` (tests)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/model.rs` (tests)

**Interfaces:**
- Consumes: `is_blank` (Task 1), the "empty assistant is absent" arm (Task 2).
- Produces: `items_to_messages` returns `Err(ModelError::Other)` with the case-2
  text.

- [ ] **Step 1: Write the failing tests**
  - In `request.rs`, errors (`unwrap_err()`, `to_string()` contains
    `"would end with an assistant message"`):
    `trailing_blank_user_after_assistant_is_error` (`[user q, assistant a, user ""]`),
    `trailing_blank_user_then_blank_assistant_is_error` (`[user q, assistant a, user "", assistant ""]`),
    `trailing_blank_user_then_reasoning_assistant_is_error` (`[user q, assistant a, user "", assistant[Reasoning]]`).
  - In `request.rs`, not errors (`is_ok()`, plus the stated messages):
    `blank_user_then_kept_assistant_is_sent` (`[user q, assistant a, user "", assistant b]` → `[user[q], assistant[a, b]]`),
    `trailing_blank_user_after_tool_result_is_sent` (`[user q, assistant a, t1, r1, user ""]` → `[user[q], assistant[a, t1], user[r1]]`),
    `caller_prefill_is_sent` (`[user q, assistant a]`),
    `caller_prefill_keeps_trailing_whitespace` (`[user q, assistant "Sure.\n"]` → assistant text is exactly `"Sure.\n"`),
    `trailing_blank_assistant_without_dropped_user_is_sent` (`[user q, assistant a, assistant ""]`),
    `trailing_reasoning_assistant_without_dropped_user_is_sent` (`[user q, assistant a, assistant[Reasoning]]`),
    `trailing_empty_or_image_user_keeps_main_behavior` (`[user q, assistant a, user []]` and `[user q, assistant a, user [Image url]]` → both `Ok`, messages `[user[q], assistant[a]]`),
    `blank_user_with_nested_tool_result_is_sent` (Review Focus 3: `[user q, assistant[ToolUse t1], user [ToolResult t1 "r", text ""]]` → `Ok`, last message `user[tool_result t1]`).
  - In `mod.rs` tests: `build_request_rejects_trailing_blank_user` runs the
    case-2 items through `build_request(&claude_cfg(), &req)` once with
    default settings and once with `ToolChoice::None`; both `Err(ModelError::Other)`.
  - In `model.rs` tests (`#[tokio::test]`): `invoke_rejects_blank_only_request_before_network`
    uses `offline_model(...)` and `[user ""]`; `invoke(...).await` is `Err`.

- [ ] **Step 2: Run the tests and see them fail**

  Run: `cargo test -p paigasus-helikon-providers-bedrock`
  Expected: the three error tests in `request.rs` and the `mod.rs` test FAIL
  (they return `Ok`). The `model.rs` test may already pass after Task 1; that
  is fine.

- [ ] **Step 3: Implement**
  - Add `let mut last_user_dropped = false;`.
  - `UserMessage` arm: set it to `true` when the message had at least one blank
    `Text` part and produced no block (no text, no tool result, no pending
    results); set it to `false` in every other case.
  - Kept `AssistantMessage` (non-empty blocks), `ToolCall` and `ToolResult`
    arms: set it to `false`. An `AssistantMessage` with no blocks and the
    `System` arm leave it unchanged.
  - After the final flushes and the case-1 check: when `last_user_dropped` and
    the last message has `ConversationRole::Assistant`, return the case-2
    error.

- [ ] **Step 4: Run the tests and see them pass**

  Run: `cargo test -p paigasus-helikon-providers-bedrock`
  Expected: PASS.

- [ ] **Step 5: Commit**

  `git commit -m "fix(providers): SMA-824 fail early when a dropped user message leaves a final assistant turn"`

### Task 4: Live tests

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/tests/live.rs`

**Interfaces:**
- Consumes: the public `BedrockModel::from_env(&model_id)` and `Model::invoke`.

- [ ] **Step 1: Write the `#[tokio::test] #[ignore]` tests**, each using
  `skip_if_missing_env()`, `max_output_tokens = Some(64)`, and the same
  "stream completes with no `Err` event" assertion as
  `tool_with_tagged_enum_and_nested_generic`:
  - `blank_user_after_tool_result_is_sent` (spec case a):
    `[user "Ping the host.", assistant [ToolUse tu_1 ping {}], ToolResult tu_1 "pong", user ""]`.
  - `replayed_blank_user_is_dropped` (case b):
    `[user "Name a color.", assistant "Blue.", user "", user "Name another."]`.
  - `whitespace_assistant_before_tool_round_keeps_order` (case c):
    `[user "Ping the host.", assistant "  ", ToolCall tu_1 ping {}, ToolResult tu_1 "pong", assistant "The host answered.", user "Thanks. Say bye."]`.
  - `assistant_first_is_sent_without_placeholder` (case d):
    `[assistant "Hello.", user "Say hi."]`; return early with a loud
    `eprintln!` unless `model_id.contains("anthropic.claude")`.
  - `whitespace_only_system_is_absent` (case e):
    `[Item::System [text "  \n"], user "Say hi."]`.

  Cases a and c set `req.tools = vec![ToolDef { name: "ping", description: "Ping a host", schema: {"type":"object","properties":{}} }]`.
  Update the module doc example to mention
  `BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5`.

- [ ] **Step 2: Compile**

  Run: `cargo test -p paigasus-helikon-providers-bedrock --test live --no-run`
  Expected: builds.

- [ ] **Step 3: Run live** (the coordinator runs this, with the user's AWS
  profile in the environment; never print credentials):

  Run: `BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5 AWS_REGION=eu-central-1 cargo test -p paigasus-helikon-providers-bedrock --test live -- --ignored --nocapture`
  Expected: all live tests PASS, none print a skip line. Save the output for
  the PR body. A run against `main` is not necessary, because the probes in
  the spec already show the failure. Note: the two existing family-gated live
  tests skip with a `global.` ID (a known out-of-scope bug); record that in
  the PR body.

- [ ] **Step 4: Commit**

  `git commit -m "test(providers): SMA-824 add live bedrock tests for blank text and tool order"`

### Task 5: Full gates

- [ ] Run each CI gate from `CLAUDE.md` that this change can affect:
  `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-features --all-targets -- -D warnings`,
  `cargo test --workspace --all-features`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`,
  `npx markdownlint-cli2`.
  Expected: all green. Report any failure with its output.
