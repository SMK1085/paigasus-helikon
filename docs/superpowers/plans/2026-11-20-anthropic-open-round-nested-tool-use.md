# SMA-834 Anthropic open round with nested tool use — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A tool-use-only `Item::AssistantMessage` inside an open tool round joins that round in the Anthropic translator, as an `Item::ToolCall` does.

**Architecture:** One new branch in the `AssistantMessage` arm of `translate_messages`, before the current flush. A shared inner helper does "append to the last assistant message, or else queue", and the `ToolCall` arm and the new branch both call it.

**Tech Stack:** Rust, `serde_json::Value` wire, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-11-20-anthropic-open-round-nested-tool-use-design.md`

## Global Constraints

- Only `crates/paigasus-helikon-providers-anthropic/src/translate/request.rs` and `crates/paigasus-helikon-providers-anthropic/tests/live.rs` change.
- No existing test changes. Every existing test stays green without edits.
- No change to `round_is_answered`, `user_blocks`, or `assistant_blocks`.
- Commit prefix: `<type>(providers): SMA-834 <lowercase message>`. Every commit message ends with a blank line and `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Before each commit: `cargo fmt --all` and `cargo clippy -p paigasus-helikon-providers-anthropic --all-features --all-targets -- -D warnings` are clean.
- Work in the foreground. Do not background a cargo run. Do not stop before the command ends.
- Do not run any git command that moves HEAD or switches the branch.

## Review Focus

1. A mixed message (text plus tool use) inside an open round must keep the flush. The parity test in Task 1 pins this.
2. A message whose only non-tool parts are reasoning or blank text must join. The reasoning test in Task 1 pins this.
3. A join into a run of adjacent assistant messages must go to the last message of the run. The adjacent-run test in Task 1 pins this.
4. Two adjacent nested messages with no pending results must stay two messages. The existing test `round_spanning_adjacent_assistant_messages_is_not_split` pins this.
5. An answered round must start a new round. `nested_tool_use_after_answered_round_starts_new_round` in Task 1 pins this.

---

### Task 1: Join an open round from a tool-use-only assistant message

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/request.rs` (`translate_messages`, lines 26-142; tests module after line 780)

**Interfaces:**
- Produces (inner fn of `translate_messages`, next to `flush_pending_assistant`):
  `fn push_tool_uses(messages: &mut Vec<Value>, pending_tool_use: &mut Vec<Value>, blocks: Vec<Value>)`.
  It extends the content array of `messages.last_mut()` when that message has `role == "assistant"`. Otherwise it extends `pending_tool_use`.
- Produces (test helper, next to `tu`/`tr`):
  `fn nested(parts: Vec<ContentPart>) -> Item` returns `Item::AssistantMessage { content: parts, agent: None }`, and
  `fn use_part(id: &str) -> ContentPart` returns `ContentPart::ToolUse { call_id: id, name: "ping", args: json!({}) }`.

- [ ] **Step 1: Write the five tests**

```rust
/// SMA-834: the nested form joins an open round as `Item::ToolCall` does.
#[test]
fn nested_tool_use_inside_open_round_joins_it() {
    let items = vec![
        user("q"),
        nested(vec![use_part("t1"), use_part("t2")]),
        result("t1"),
        nested(vec![use_part("t3")]),
        result("t2"),
        result("t3"),
    ];
    assert_eq!(
        translate_messages(&items).messages,
        json!([
            utext("q"),
            {"role": "assistant", "content": [tu("t1"), tu("t2"), tu("t3")]},
            {"role": "user", "content": [tr("t1"), tr("t2"), tr("t3")]},
        ])
    );
}

#[test]
fn nested_tool_use_after_answered_round_starts_new_round() {
    let items = vec![
        user("q"),
        nested(vec![use_part("t1")]),
        result("t1"),
        nested(vec![use_part("t2")]),
        result("t2"),
    ];
    assert_eq!(
        translate_messages(&items).messages,
        json!([
            utext("q"),
            {"role": "assistant", "content": [tu("t1")]},
            {"role": "user", "content": [tr("t1")]},
            {"role": "assistant", "content": [tu("t2")]},
            {"role": "user", "content": [tr("t2")]},
        ])
    );
}

/// The tool-use-only check reads the blocks after `assistant_blocks`
/// drops reasoning, so this message joins too.
#[test]
fn nested_tool_use_with_reasoning_inside_open_round_joins_it() {
    // Same items as `nested_tool_use_inside_open_round_joins_it`, but the
    // t3 message is `nested(vec![ContentPart::Reasoning { text: "scratch".to_owned() }, use_part("t3")])`.
    // Same expected array.
}

#[test]
fn nested_tool_use_joins_last_of_adjacent_assistant_run() {
    let items = vec![
        user("q"),
        nested(vec![use_part("t1")]),
        nested(vec![use_part("t2")]),
        result("t2"),
        nested(vec![use_part("t3")]),
        result("t1"),
        result("t3"),
    ];
    // Same expected array as `round_spanning_adjacent_assistant_messages_is_not_split`:
    // [utext("q"), asst[tu(t1)], asst[tu(t2), tu(t3)], user[tr(t2), tr(t1), tr(t3)]]
}

/// A message with text inside an open round still flushes, in both forms.
/// Both forms give a request that the API rejects (known gap, SMA-836).
/// The test asserts parity between the two forms, not that the output is valid.
#[test]
fn mixed_assistant_inside_open_round_matches_top_level_form() {
    let head = || vec![user("q"), nested(vec![use_part("t1"), use_part("t2")]), result("t1")];
    let tail = [result("t2"), result("t3")];
    // nested form: head + [nested(vec![text("b"), use_part("t3")])] + tail
    // top-level form: head + [asst("b"), call("t3")] + tail
    // assert_eq!(translate_messages(&nested_form).messages, translate_messages(&top_level).messages);
}
```

Write the bodies of the three commented tests as the comments say. `Item` is not `Clone`-required here: build each vector with `head()` and push the tail items by constructing them again if `Item` is not `Clone`.

- [ ] **Step 2: Run the tests and see the expected failures**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib translate::request::tests::nested_ translate::request::tests::mixed_`
Expected: `nested_tool_use_inside_open_round_joins_it`, `nested_tool_use_with_reasoning_inside_open_round_joins_it`, and `nested_tool_use_joins_last_of_adjacent_assistant_run` FAIL. `nested_tool_use_after_answered_round_starts_new_round` and `mixed_assistant_inside_open_round_matches_top_level_form` PASS (they guard current behavior).

(If `cargo test` takes only one filter, run it once with the filter `tests::nested_` and once with `tests::mixed_`.)

- [ ] **Step 3: Implement**

1. Add the inner fn `push_tool_uses` (signature above). Change the `ToolCall` arm to call it with `vec![block]`. The `ToolCall` behavior does not change.
2. In the `AssistantMessage` arm, before the current `if !blocks.is_empty()` branch, add a branch for: blocks non-empty, every block has `b["type"] == "tool_use"`, `!pending_tool_results.is_empty()`, and `!round_is_answered(&messages, &pending_tool_results)`. On that branch: set `last_item_dropped = false`, then call `push_tool_uses`. Keep the current branch as the `else if`.
3. Update the arm's comment. It says, in short sentences:
   - a message of only tool uses is the nested form of `Item::ToolCall`; inside an open round it joins that round, as a call does; a flush would put a user turn that lacks some results right after the open assistant turn;
   - on this path the last message is an assistant message (`round_is_answered` is `true` for an empty trailing assistant run), and `pending_tool_use` is empty (each `ToolResult` flushes it), so the append keeps the order;
   - setting `last_item_dropped` here is for symmetry; the flag is already `false`, because a `ToolResult` came after any dropped user message.

- [ ] **Step 4: Run the full crate tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic --all-features`
Expected: all tests PASS, including the five new ones and every existing test.

- [ ] **Step 5: Mutation check of condition 1**

Remove the "every block is `tool_use`" condition temporarily. Run `cargo test -p paigasus-helikon-providers-anthropic --lib tests::mixed_`.
Expected: FAIL. Restore the condition and run the test again. Expected: PASS. Do not commit the mutation.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-anthropic --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-anthropic/src/translate/request.rs
git commit -m "fix(providers): SMA-834 let tool-use-only assistant messages join an open anthropic round"
```

---

### Task 2: Live test for the nested form inside an open round

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/tests/live.rs` (after `tool_only_multi_round_loop_is_accepted`, line 297)

**Interfaces:**
- Consumes: `run_accepted(messages, with_tool, last_turn_cache)`, `skip_if_no_key()`, `user`, `result`, `assistant` from `live.rs`.
- Produces: `fn nested(ids: &[&str]) -> Item` in `live.rs`, an `AssistantMessage` with one `ContentPart::ToolUse { name: "ping", args: json!({}) }` per id.

- [ ] **Step 1: Write the test**

```rust
#[tokio::test]
#[ignore]
async fn nested_tool_use_inside_open_round_is_accepted() {
    if skip_if_no_key() {
        return;
    }
    let (a, b, c) = (
        "toolu_01A09q90qw90lq917835lq9",
        "toolu_01B09q90qw90lq917835lq8",
        "toolu_01C09q90qw90lq917835lq7",
    );
    let msgs = vec![
        user("ping three times"),
        nested(&[a, b]),
        result(a),
        nested(&[c]),
        result(b),
        result(c),
        assistant("done"),
        user("reply ok"),
    ];
    run_accepted(msgs, true, false).await;
}
```

- [ ] **Step 2: Compile it**

Run: `cargo test -p paigasus-helikon-providers-anthropic --test live --no-run`
Expected: it compiles. `cargo test -p paigasus-helikon-providers-anthropic --test live` reports the test as ignored.

- [ ] **Step 3: Run it live, only if `ANTHROPIC_API_KEY` is set**

Run: `cargo test -p paigasus-helikon-providers-anthropic --test live nested_tool_use_inside_open_round_is_accepted -- --ignored --nocapture`
Expected: PASS. Record the result for the PR body. If the key is not set, write "not run live: no key" in the report. Do not print the key.

- [ ] **Step 4: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-anthropic --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-anthropic/tests/live.rs
git commit -m "test(providers): SMA-834 add live anthropic test for nested tool use in an open round"
```
