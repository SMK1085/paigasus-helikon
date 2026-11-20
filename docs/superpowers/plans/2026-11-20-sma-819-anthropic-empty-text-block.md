# SMA-819 Anthropic empty text block Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Anthropic translator never sends a blank text block, never sends an empty message, keeps `tool_use`/`tool_result` in item order, and fails early when the drops leave no valid request.

**Architecture:** All translation changes are in `translate/request.rs` (`translate_messages`, `user_blocks`, `assistant_blocks`). `translate_messages` reports one new flag on `TranslatedMessages`. `build_body` in `translate/mod.rs` turns that flag and an empty `messages` array into `ModelError::Other`.

**Tech Stack:** Rust, `serde_json::Value`, `wiremock` (wire tests), `tokio` (live tests).

**Spec:** `docs/superpowers/specs/2026-11-20-sma-819-anthropic-empty-text-block-design.md`

## Global Constraints

- Crate: `crates/paigasus-helikon-providers-anthropic`. Do not change `paigasus-helikon-core` or any other provider.
- "Blank" means `text.trim().is_empty()` (Rust `str::trim`).
- A kept text block keeps its exact text. Never trim the text that is sent.
- Never put placeholder text in place of a dropped block.
- A `tracing` line about a drop may contain counts only, never text.
- No hand version bump. Commit type `fix(providers-anthropic)` for code, `test(providers-anthropic)` for tests only, `docs(plan)` / `docs(spec)` for docs. Prefix the subject with `SMA-819`.
- Before each commit: `cargo fmt --all` and `cargo clippy -p paigasus-helikon-providers-anthropic --all-targets -- -D warnings`.
- Work synchronously in the foreground. Do not end your turn while a cargo command runs. Do not run `git checkout`, `git switch`, `git reset`, `git stash` or any command that moves HEAD or changes branches.
- Work only in `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-819`. Use absolute paths under that root for every file edit.

## Review Focus

1. A request that ends in a blank user message after a tool result (the SMA-804 `Inject([Text ""])` path) must send the `tool_result` blocks and must not fail. Covered in Task 2 and Task 3.
2. A caller-made prefill (the last item is an `AssistantMessage`) must not get the decision-(c) error, even if an earlier user message was blank. Covered in Task 3.
3. Many tool loops in one history (call, result, call, result, text) must keep strict alternation. Covered in Task 1 (`multi_round_tool_loop_without_assistant_text_alternates`).
4. A reasoning-only assistant turn before tool calls (a session recorded with thinking on) must translate exactly as a tool-only turn. Covered in Task 2.
5. `CacheStrategy::LastTurn` must not put `cache_control` on a dropped block. Covered in Task 3.

---

### Task 1: Keep `tool_use` and `tool_result` in item order

This fixes a bug that is on `main` today. Task 2 depends on it.

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/request.rs` (the `AssistantMessage`, `ToolCall` and `ToolResult` arms of `translate_messages`, lines 62-93; tests module)

**Interfaces:**
- Consumes: nothing new.
- Produces: no signature change. Behavior: wire order follows item order.

- [ ] **Step 1: Write the failing tests** in the `tests` module. Use full `assert_eq!` on `out.messages`.

```rust
#[test]
fn tool_only_turn_then_assistant_text_keeps_order() {
    let items = vec![
        Item::UserMessage { content: vec![text("q")] },
        Item::ToolCall { call_id: "tu_1".into(), name: "ping".into(), args: json!({}) },
        Item::ToolResult { call_id: "tu_1".into(), content: vec![text("pong")] },
        Item::AssistantMessage { content: vec![text("done")], agent: None },
    ];
    assert_eq!(translate_messages(&items).messages, json!([
        {"role": "user", "content": [{"type": "text", "text": "q"}]},
        {"role": "assistant", "content": [{"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}}]},
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "pong"}]},
        {"role": "assistant", "content": [{"type": "text", "text": "done"}]},
    ]));
}

#[test]
fn tool_call_after_pending_result_does_not_fold_into_earlier_assistant() {
    // user q, assistant "a" + tu_1, result r1, tu_2 (next turn, no text), result r2
    // expected: user[q], assistant[a, tool_use tu_1], user[tool_result tu_1],
    //           assistant[tool_use tu_2], user[tool_result tu_2]
}

#[test]
fn multi_round_tool_loop_without_assistant_text_alternates() {
    // user q, tu_1, r1, tu_2, r2, assistant "done"
    // expected: user, assistant[tu_1], user[r1], assistant[tu_2], user[r2], assistant[done]
}
```

Write the full `assert_eq!` for the second and third tests in the same style as the first.

- [ ] **Step 2: Run the tests and see them fail**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib translate::request`
Expected: the three new tests FAIL (wrong order). All earlier tests PASS.

- [ ] **Step 3: Fix the order in `translate_messages`**
  - `AssistantMessage` arm: call `flush_pending_assistant` before `flush_pending_user`.
  - `ToolCall` arm: call `flush_pending_user(&mut messages, &mut pending_tool_results)` before the fold check.
  - `ToolResult` arm: call `flush_pending_assistant(&mut messages, &mut pending_tool_use)` before the push.

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib translate::request`
Expected: all PASS, including `tool_call_folds_into_preceding_assistant`, `tool_result_then_user_message_request_shape` and `adjacent_tool_results_coalesce_into_one_user_turn`.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/translate/request.rs
git commit -m "fix(providers-anthropic): SMA-819 keep tool_use and tool_result in item order"
```

---

### Task 2: Drop blank text, empty messages and blank system text

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/request.rs` (`TranslatedMessages`, `translate_messages`, `user_blocks`, `assistant_blocks`, tests)

**Interfaces:**
- Consumes: Task 1 ordering.
- Produces: a new field on `pub(crate) struct TranslatedMessages`:
  `pub(crate) trailing_user_dropped: bool`. It is `true` only when the last conversation item (`UserMessage`, `AssistantMessage`, `ToolCall` or `ToolResult`) is a `UserMessage` that produced no message, **and** the last element of `messages` has `role == "assistant"`. `Item::System` and unknown variants do not count as conversation items.

- [ ] **Step 1: Write the failing tests.** Use full `assert_eq!` on `out.messages`.
  - Rename `empty_text_part_is_emitted_as_empty_block` to `empty_text_part_is_dropped`. Assert `out.messages == json!([])`.
  - `whitespace_only_text_part_is_dropped`: user `"  \n\t"` gives `json!([])`.
  - `blank_text_is_dropped_but_real_text_kept_verbatim`: user `["", "  hi  ", " "]` gives one user message with `[{"type":"text","text":"  hi  "}]`.
  - `blank_user_text_keeps_pending_tool_results`: `user q, ToolCall tu_1, ToolResult tu_1, user ""` gives `user[q], assistant[tool_use tu_1], user[tool_result tu_1]`, and `trailing_user_dropped == false`.
  - `blank_assistant_text_is_dropped`: assistant `["  ", "ok"]` gives `assistant[text ok]`.
  - `assistant_with_no_blocks_is_not_sent`: `user q, assistant "", user r` gives `user[q], user[r]`.
  - `reasoning_only_assistant_is_not_sent`: `user q, assistant [Reasoning], user r` gives `user[q], user[r]`.
  - `reasoning_only_assistant_before_tool_call_keeps_order`: `user q, assistant [Reasoning], ToolCall tu_1, ToolResult tu_1, assistant "done"` gives the same array as `tool_only_turn_then_assistant_text_keeps_order`.
  - `whitespace_assistant_before_tool_call_keeps_order`: the same with assistant `"\n\n"`.
  - `blank_assistant_after_kept_assistant_folds_tool_call_into_kept`: `user q, assistant "a", assistant " ", ToolCall tu_1` gives `user[q], assistant[text a, tool_use tu_1]`.
  - `replayed_blank_user_between_real_turns_is_dropped`: `user q, assistant a, user "", user r` gives `user[q], assistant[a], user[r]`.
  - `trailing_blank_user_after_assistant_sets_flag`: `user q, assistant a, user ""` gives `user[q], assistant[a]` and `trailing_user_dropped == true`.
  - `caller_prefill_does_not_set_flag`: `user q, assistant a, user "", assistant b` gives `trailing_user_dropped == false`.
  - `whitespace_only_system_is_absent`: `System "  \n"`, user q gives `out.system == None`.

- [ ] **Step 2: Run the tests and see them fail**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib translate::request`
Expected: the new tests FAIL. The struct field does not compile until Step 3, which counts as the failure.

- [ ] **Step 3: Implement**
  - Add a private helper `fn is_blank(text: &str) -> bool` (`text.trim().is_empty()`).
  - `user_blocks` and `assistant_blocks` skip a `ContentPart::Text` where `is_blank` is true.
  - `UserMessage` arm: push the message only when the combined `blocks` (pending tool results plus user blocks) is not empty.
  - `AssistantMessage` arm: push the message only when `assistant_blocks` is not empty.
  - Track the last conversation item in a local `last_item_was_dropped_user: bool`. Set it in the four conversation arms. After the final flushes, compute `trailing_user_dropped` from it and the role of the last message.
  - `system`: `None` when `system_text.trim().is_empty()`.
  - Update the module doc of `translate_messages` to state the drop rule in one sentence.

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/translate/request.rs
git commit -m "fix(providers-anthropic): SMA-819 drop blank text blocks and empty messages"
```

---

### Task 3: Fail early in `build_body`

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/mod.rs` (`build_body`, after `translate_messages`; tests module from line 140)
- Modify: `crates/paigasus-helikon-providers-anthropic/tests/messages_wire.rs` (one new test)

**Interfaces:**
- Consumes: `TranslatedMessages::trailing_user_dropped` and `messages` from Task 2.
- Produces: `build_body` returns `Err(ModelError::Other(_))` for the two cases below. The message texts are:
  - empty: `"no message with content to send (empty and whitespace-only text is not sent)"`
  - flag: `"the last user message has no content after empty and whitespace-only text was dropped; the request would end with an assistant message"`

- [ ] **Step 1: Write the failing tests** in `translate/mod.rs` tests. Use the existing `cfg()` and `user_text()` helpers. Check the error with `matches!(err, ModelError::Other(_))` and `err.to_string().contains(...)` on a distinct word of each text.
  - `blank_only_user_is_rejected`: `[user ""]`.
  - `system_only_is_rejected`: `[System "be nice"]`.
  - `system_and_blank_user_is_rejected`: `[System "be nice", user " "]`.
  - `trailing_blank_user_after_assistant_is_rejected`: `[user q, assistant a, user ""]`.
  - `caller_prefill_after_blank_user_is_sent`: `[user q, assistant a, user "", assistant b]` returns `Ok`.
  - `blank_user_after_tool_result_is_sent`: `[user q, assistant a + ToolCall tu_1, ToolResult tu_1, user ""]` returns `Ok`, and the last message is `user` with one `tool_result` block.
  - `last_turn_cache_skips_dropped_blank_user`: a config built with `.cache_strategy(CacheStrategy::LastTurn)`, items `[user q, assistant a, ToolCall tu_1, ToolResult tu_1, user ""]`. Assert that the last message's last block (the `tool_result`) has `cache_control`, and that no block in `body["messages"]` has `"type": "text"` with blank `text`.

  In `tests/messages_wire.rs`:
  - `blank_only_request_fails_without_http_call`: mount a `Mock` on `POST /v1/messages` with `.expect(0)`. `model.invoke(req_with(vec![user("  ")]), ...)` returns `Err(ModelError::Other(_))`. The server verifies `.expect(0)` on drop.

- [ ] **Step 2: Run the tests and see them fail**

Run: `cargo test -p paigasus-helikon-providers-anthropic`
Expected: the rejection tests and the wire test FAIL. The `Ok` tests may already pass.

- [ ] **Step 3: Implement** the two checks in `build_body`, directly after `request::translate_messages`, with `ModelError::Other(anyhow::anyhow!(...))`. Use the texts above exactly.

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test -p paigasus-helikon-providers-anthropic`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/translate/mod.rs crates/paigasus-helikon-providers-anthropic/tests/messages_wire.rs
git commit -m "fix(providers-anthropic): SMA-819 fail early when no valid request remains"
```

---

### Task 4: Live tests and documentation check

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/tests/live.rs`
- Check (edit only if a page describes the translation rule): `crates/paigasus-helikon-providers-anthropic/README.md`, `docs/book/src/*.md`

**Interfaces:**
- Consumes: the public `AnthropicModel` API only.

- [ ] **Step 1: Add four `#[tokio::test] #[ignore]` tests** that use `claude-sonnet-4-6` (it rejects a prefill, so it is the strict model), `max_output_tokens = Some(64)`, `skip_if_no_key()`, and a `ToolDef` named `ping` with schema `{"type":"object"}` when the items contain a tool call. Each test drains the stream and panics on any `Err` event.
  - `blank_user_after_tool_result_is_accepted`: user, assistant `"calling"` + `ToolCall tu_1`, `ToolResult tu_1 "pong"`, user `""`.
  - `blank_user_after_tool_result_with_last_turn_cache_is_accepted`: the same with `.cache_strategy(CacheStrategy::LastTurn)`.
  - `reasoning_only_assistant_tool_loop_is_accepted`: user, assistant `[Reasoning "scratch"]`, `ToolCall tu_1`, `ToolResult tu_1 "pong"`, assistant `"done"`, user `"thanks, reply ok"`.
  - `replayed_blank_user_is_accepted`: user `"hi"`, assistant `"Hello"`, user `""`, user `"Reply with ok"`.

  Use the real tool-use ID format `toolu_01...` for `call_id` values.

- [ ] **Step 2: Run the live tests with the real key**

Run (the key goes into the environment only; never print it):
```bash
set -a; . /Users/smaschek/dev/paigasus/paigasus-helikon/.env; set +a
cargo test -p paigasus-helikon-providers-anthropic --test live -- --ignored --nocapture 2>&1 | tail -30
```
Expected: all live tests PASS, including the four new ones. Record the `test result:` line for the PR. A run that skips because no key is set does not count.

- [ ] **Step 3: Documentation check.** `grep -rn -i "empty\|whitespace\|prefill" crates/paigasus-helikon-providers-anthropic/README.md docs/book/src`. If a page describes how the translator sends text or prefill, add one sentence about the drop rule and the early error. If no page does, make no edit and state that in the task report.

- [ ] **Step 4: Run the full CI gates for this crate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-providers-anthropic --all-features
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-providers-anthropic --no-deps
```
Expected: all clean.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/tests/live.rs
git commit -m "test(providers-anthropic): SMA-819 add live tests for dropped blank text"
```
