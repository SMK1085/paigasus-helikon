# SMA-820 Gemini same-role merge and FIFO tool-result naming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Gemini request translator names each `functionResponse` from the call it closes (FIFO), and never sends two adjacent contents with the same role.

**Architecture:** `items_to_contents` keeps one pass in item order. An `OpenCalls` value replaces the global `call_id -> name` map: calls open entries, results close the earliest open entry with their id. The pass produces `(role, parts)` entries; a final fold drops empty entries and merges adjacent entries with the same role.

**Tech Stack:** Rust, `serde_json`, `insta` snapshots, `tracing`, `tokio` (live tests).

**Spec:** `docs/superpowers/specs/2026-11-20-sma-820-gemini-merge-same-role-contents-design.md`

## Global Constraints

- Work only in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-820`, branch `feature/sma-820-gemini-translator-sends-two-adjacent-user`. Do not run `git checkout`, `git switch`, `git reset`, `git rebase` or `git stash`.
- Work synchronously in the foreground. Do not end a turn while a `cargo` command runs.
- No hand version bump in any `Cargo.toml`. No change to `stream.rs` or to other crates.
- Warning target for dropped parts: `paigasus::gemini::translate`.
- Error messages, exact: `tool result references unknown call_id {call_id}` (no earlier call and no later call), `tool result for call_id {call_id} comes before its call` (a later item has a call with that id).
- Commit prefix (`providers-gemini` is not in the `.versionrc` scope allowlist, so use `providers`): `fix(providers): SMA-820 <lowercase subject>`. End each commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Before each commit: `cargo fmt --all` and `cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings`.
- Stage files by explicit path. Never `git add -A` or `git add .`.

## Review Focus

1. A `UserMessage` whose only part is a dropped duplicate result: the entry becomes empty, is dropped, and its neighbours merge. Test in Task 1.
2. A conversation that is only `user("")`: no content remains, so the empty-conversation error returns (not `"parts": []`). Test in Task 2.
3. Same-id calls as `ContentPart::ToolUse` parts in one `AssistantMessage`, closed by nested `ContentPart::ToolResult` parts in one `UserMessage`: FIFO by part order. Test in Task 1.
4. An `AssistantMessage` with text and a `ToolUse` part, then an `Item::ToolCall`: one `model` content with three parts in order. Test in Task 2.
5. An `Item::System` between two user messages: the system text goes to `systemInstruction` and the two user messages merge. Test in Task 2.

---

### Task 1: FIFO name resolution with `OpenCalls`

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/src/translate/request.rs` (replace the `call_names` pre-scan at lines 15-31; change `user_parts` at lines 108-149 and the `Item::ToolResult` branch at lines 64-81)
- Test: the `#[cfg(test)] mod tests` in the same file

**Interfaces:**
- Produces (private to `request.rs`):
  - `struct OpenCalls<'a>` with `fn new(items: &'a [Item]) -> Self`, `fn open(&mut self, call_id: &'a str, name: &'a str)`, `fn close(&mut self, call_id: &str) -> Result<Option<&'a str>, ModelError>`.
  - `close` returns `Ok(Some(name))` for the earliest open call with that id (and removes it), `Ok(None)` when no call is open but an earlier call with that id was opened (a duplicate result; the caller drops the part and logs `tracing::warn!`), and `Err` with one of the two exact messages otherwise. `new` collects the set of all call ids in `items` (from `Item::ToolCall` and `ContentPart::ToolUse`) only to choose between the two error messages. It must not store names by id.
  - `fn user_parts(content: &'a [ContentPart], calls: &mut OpenCalls<'a>) -> Result<Vec<Value>, ModelError>`.
- The main loop calls `calls.open` for each `Item::ToolCall` and for each `ContentPart::ToolUse` in an `AssistantMessage`, in part order, when it reaches that item.
- In this task, keep one pushed content per item, as today. Task 2 adds the merge.

- [ ] **Step 1: Write the failing tests**

Add a helper `fn call(id: &str, name: &str) -> Item` (an `Item::ToolCall` with `args: json!({})`) and `fn result(id: &str, json: &str) -> Item` (an `Item::ToolResult` with one `Text` part). Add these tests. Each asserts the `name` (and `id`) of every `functionResponse` in `t.contents`, found in order with a helper `fn responses(t: &TranslatedContents) -> Vec<(String, String)>` that returns `(id, name)` for each `functionResponse` part across all contents:

- `cross_turn_repeated_id_names_each_result_from_its_own_call`: `user("a"), call("fc_0","search"), result("fc_0","{}"), Item::AssistantMessage{text "ok"}, user("b"), call("fc_0","pay"), result("fc_0","{}")` → `[("fc_0","search"), ("fc_0","pay")]`.
- `same_round_repeated_id_tool_call_items_pair_fifo`: `user("a"), call("fc_0","search"), call("fc_0","pay"), result("fc_0","{}"), result("fc_0","{}")` → `[("fc_0","search"), ("fc_0","pay")]`.
- `same_round_repeated_id_tool_use_parts_pair_fifo`: `user("a")`, one `AssistantMessage` with parts `ToolUse{fc_0, search}`, `ToolUse{fc_0, pay}`, then one `UserMessage` with parts `ToolResult{fc_0}`, `ToolResult{fc_0}` → `[("fc_0","search"), ("fc_0","pay")]`.
- `results_in_different_order_than_calls_keep_their_names`: `user("a"), call("fc_0","search"), call("fc_1","pay"), result("fc_1","{}"), result("fc_0","{}")` → `[("fc_1","pay"), ("fc_0","search")]`.
- `duplicate_result_for_one_call_is_dropped`: `user("a"), call("fc_0","search"), result("fc_0","{\"n\":1}"), result("fc_0","{\"n\":2}")` → `responses == [("fc_0","search")]`, and the kept response has `response.n == 1`.
- `duplicate_result_as_only_part_leaves_no_empty_content`: `user("a"), call("fc_0","search"), result("fc_0","{}")`, then a `UserMessage` with only `ToolResult{fc_0}`, then `user("b")` → no content in `t.contents` has an empty `parts` array.
- `result_before_its_call_errors`: `user("a"), result("fc_0","{}"), call("fc_0","search")` → `Err`, and `err.to_string()` contains `comes before its call`.
- Keep `tool_result_without_matching_call_errors`; add an assertion that its message contains `unknown call_id ghost`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-providers-gemini --lib translate::request`
Expected: FAIL in `cross_turn_repeated_id_names_each_result_from_its_own_call` (gets `pay` for the first result), the two `same_round_*` tests, `duplicate_result_for_one_call_is_dropped` and `result_before_its_call_errors`.

- [ ] **Step 3: Implement `OpenCalls` and wire it into `items_to_contents` and `user_parts`**

`OpenCalls` holds `open: Vec<(&'a str, &'a str)>` (call id, name) in open order, `seen: HashSet<&'a str>` (ids opened so far), and `all: HashSet<&'a str>` (ids of every call in `items`). `close` removes the first `open` entry with a matching id. For `Ok(None)`, log `tracing::warn!(target: "paigasus::gemini::translate", call_id, "tool result closes no open call; dropping")`. Update the doc comment of `user_parts` (it now says the name comes from the earliest open call).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-providers-gemini`
Expected: PASS, all tests, snapshots unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-gemini/src/translate/request.rs
git commit -m "fix(providers): SMA-820 name each function response from the call it closes"
```

The commit body says: a result closes the earliest open call with its id (the core rule); a second result for one call is dropped with a warning; a result before its call now fails with `tool result for call_id {id} comes before its call`.

---

### Task 2: Merge adjacent same-role contents and drop empty parts

> Note: after the live run, Sven added the tool-result boundary (see the spec, "Merge"). The expected arrays in Step 1 for `tool_result_then_user_message_request_shape` and for the "more than two" test changed to match. The latter is now `adjacent_user_texts_merge_but_not_into_a_tool_result`.

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/src/translate/request.rs` (`items_to_contents`, `user_parts`, `assistant_parts`; the test `tool_result_then_user_message_request_shape` and its doc comment)
- Modify: `crates/paigasus-helikon-providers-gemini/src/translate/mod.rs:223-236` (inline asserts in `snap_parallel_same_name_tool_calls`)
- Modify: `crates/paigasus-helikon-providers-gemini/src/translate/snapshots/*.snap` (only through `cargo insta`)

**Interfaces:**
- Consumes: `OpenCalls` and `user_parts` from Task 1.
- Produces: `fn merge_same_role(entries: Vec<(&'static str, Vec<Value>)>) -> Vec<Value>` (private). It drops entries with no parts, appends the parts of an entry to the last content when the roles are equal, and else starts `{"role": role, "parts": parts}`.
- The main loop pushes `("user", ..)` / `("model", ..)` entries instead of JSON contents. The empty-conversation error check runs on the output of `merge_same_role`.
- `user_parts` and `assistant_parts` skip a `ContentPart::Text` whose `text` is empty. `text_parts` (system) does not change.

- [ ] **Step 1: Write the failing tests**

Change `tool_result_then_user_message_request_shape` to expect exactly:

```json
[
  {"role": "user", "parts": [{"text": "start"}]},
  {"role": "model", "parts": [{"text": "calling"}, {"functionCall": {"id": "fc_0", "name": "search", "args": {"q": "cats"}}}]},
  {"role": "user", "parts": [{"functionResponse": {"id": "fc_0", "name": "search", "response": {"hits": 3}}}, {"text": "news"}]}
]
```

Its doc comment says that the checkpoint `Inject` shape now becomes one merged `user` content (SMA-820). Add tests that assert the full `Value::Array(t.contents)`:

- `more_than_two_adjacent_same_role_contents_merge_in_order`: `user("start"), call("fc_0","search"), result("fc_0","{}"), user("one"), user("two")` → last content is `{"role":"user","parts":[{functionResponse fc_0 search {}}, {"text":"one"}, {"text":"two"}]}`, and `t.contents.len() == 3`.
- `parallel_calls_merge_into_one_model_and_one_user_content`: `user("x"), call("fc_0","search"), call("fc_1","search"), result("fc_0","{\"n\":1}"), result("fc_1","{\"n\":2}")` → three contents: user text, `model` with two `functionCall` parts (fc_0, fc_1), `user` with two `functionResponse` parts (fc_0, fc_1).
- `nested_result_and_text_merge_with_preceding_tool_result`: `user("x"), call("fc_0","search"), call("fc_1","pay"), result("fc_0","{}")`, then a `UserMessage` with parts `ToolResult{fc_1}`, `Text "next"` → last content is `user` with `[functionResponse fc_0 search, functionResponse fc_1 pay, {"text":"next"}]`.
- `system_between_user_messages_does_not_split_them`: `user("a"), Item::System{"be terse"}, user("b")` → `contents == [{"role":"user","parts":[{"text":"a"},{"text":"b"}]}]`, `system.parts[0].text == "be terse"`.
- `reasoning_only_assistant_message_is_dropped_and_neighbours_merge`: `user("a")`, an `AssistantMessage` with only a `ContentPart::Reasoning` part, `user("b")` → one `user` content with two text parts.
- `assistant_text_tool_use_and_tool_call_form_one_model_content`: `user("a")`, an `AssistantMessage` with `Text "t"` and `ToolUse{fc_0, search}`, then `call("fc_1","pay")` → second content is `model` with parts `[{"text":"t"}, functionCall fc_0, functionCall fc_1]`.
- `empty_text_part_is_dropped`: `user("a"), call("fc_0","search"), result("fc_0","{}")`, then a `UserMessage` with `Text ""` → last content is `user` with only the `functionResponse` part.
- `only_empty_user_text_is_an_empty_conversation_error`: `[user("")]` → `Err`.

The reasoning part is `ContentPart::Reasoning { text: "r".into() }`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-providers-gemini --lib translate`
Expected: FAIL in the changed and the new tests; `snap_parallel_same_name_tool_calls` still passes at this step.

- [ ] **Step 3: Implement `merge_same_role` and the empty-text skip**

Use the interfaces above. Do not change the order of parts.

- [ ] **Step 4: Update the parallel snapshot test and review the snapshot diffs**

In `mod.rs`, change the inline asserts of `snap_parallel_same_name_tool_calls` to `b["contents"][2]["parts"][0]["functionResponse"]` (`name == "search"`, `id == "fc_0"`) and `b["contents"][2]["parts"][1]["functionResponse"]` (`id == "fc_1"`), and update its comment. Then run:

Run: `cargo insta test -p paigasus-helikon-providers-gemini --review` (or `cargo insta test` then `cargo insta review`)
Expected: only `snap_parallel_same_name_tool_calls` changes, to `contents` of length 3 (user, model with 2 `functionCall` parts, user with 2 `functionResponse` parts). Accept only that diff. If another snapshot changes, stop and report it.

- [ ] **Step 5: Run all crate tests**

Run: `cargo test -p paigasus-helikon-providers-gemini --all-features`
Expected: PASS, no pending `.snap.new` files (`git status --short` shows none).

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-providers-gemini/src/translate/request.rs \
        crates/paigasus-helikon-providers-gemini/src/translate/mod.rs \
        crates/paigasus-helikon-providers-gemini/src/translate/snapshots/paigasus_helikon_providers_gemini__translate__snap__snap_parallel_same_name_tool_calls.snap
git commit -m "fix(providers): SMA-820 merge adjacent same-role contents"
```

---

### Task 3: Live tests through the translator

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/tests/live.rs`

**Interfaces:**
- Consumes: the translator behavior of Tasks 1-2 through `GeminiModel::invoke`.
- `dev_model()` default changes from `"gemini-2.5-flash"` to `"gemini-3.8-flash"` (override with `GEMINI_MODEL_ID`). Update the module doc comment to say so. Do not change other occurrences of `gemini-2.5-flash` (SMA-565).

- [ ] **Step 1: Write the three ignored live tests**

Add a helper `async fn assert_finishes(model: &GeminiModel, messages: Vec<Item>)` that builds a `ModelRequest` with `messages` and `tools` = `ToolDef`s for `search` (`{"q": string}`) and `pay` (`{"amount": number}`), invokes it, and asserts that no stream item is `Err` and that one item is `ModelEvent::Finish { .. }`. Each test returns early when `dev_model()` is `None`. Every history ends with a user text after the last tool result, because the translator sends no thought signatures (SMA-827) and Gemini checks them only in the current turn:

- `live_developer_tool_result_then_user_text`: `user "search cats"`, `ToolCall fc_0 search {"q":"cats"}`, `ToolResult fc_0 {"hits":3}`, `user "Answer in French."`.
- `live_developer_parallel_round`: `user "search a and b"`, `ToolCall fc_0 search {"q":"a"}`, `ToolCall fc_1 search {"q":"b"}`, `ToolResult fc_0 {"n":1}`, `ToolResult fc_1 {"n":2}`, `user "Summarize."`.
- `live_developer_cross_turn_repeated_call_id`: `user "search cats"`, `ToolCall fc_0 search`, `ToolResult fc_0 {"hits":3}`, assistant text `"3 hits."`, `user "now pay 5"`, `ToolCall fc_0 pay {"amount":5}`, `ToolResult fc_0 {"ok":true}`, `user "Confirm."`.

- [ ] **Step 2: Compile the live tests**

Run: `cargo test -p paigasus-helikon-providers-gemini --test live --no-run`
Expected: builds with no warnings.

- [ ] **Step 3: Run the live tests (coordinator only)**

The implementer subagent does not run this step. The coordinator runs it with `GEMINI_API_KEY` from `.env` (Sven authorized this on 2026-11-20):

Run: `cargo test -p paigasus-helikon-providers-gemini --test live -- --ignored live_developer_`
Expected: the three new tests pass. The "before" evidence is the probe (shape 9 gave HTTP 400 for today's wrong name); the new tests do not exist on the old code, so no control run. Record the result in the spec section "After the fix".

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-providers-gemini/tests/live.rs \
        docs/superpowers/specs/2026-11-20-sma-820-gemini-merge-same-role-contents-design.md
git commit -m "test(providers): SMA-820 add live tests for merged contents and repeated call ids"
```

The commit body carries the probe source (`probe.sh`) and its before results, as the spec requires.
