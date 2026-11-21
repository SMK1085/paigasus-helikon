# SMA-833 Bedrock `ToolChoice::None` with a tool history: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Bedrock request with `ToolChoice::None` and a tool round in the history gets a 200 response, and the model cannot make a real tool call.

**Architecture:** One new translate module applies a per-family strategy after `items_to_messages`. Claude gets derived tool specs plus a typed flag that `model.rs` turns into `additionalModelRequestFields = {"tool_choice":{"type":"none"}}`. All other families get their tool blocks written as JSON-escaped text. A stream-translator guard turns any real tool call on a `None` request into an error.

**Tech Stack:** Rust 1.94, `aws-sdk-bedrockruntime`, `aws-smithy-types::Document`, `serde_json` (no `preserve_order`), `insta` snapshots.

**Spec:** `docs/superpowers/specs/2026-11-22-sma-833-bedrock-tool-choice-none-design.md`

## Global Constraints

- Crate: `crates/paigasus-helikon-providers-bedrock`. No other crate changes. No public API changes; every new item is `pub(crate)`.
- Commit format: `<type>(providers): SMA-833 <lowercase subject>`, with the trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Use `fix(providers)` for code and `test(providers)` / `docs(providers)` where it fits.
- Derived spec description, exact: `A tool that this conversation used earlier.` Derived schema, exact: `{"type":"object"}`.
- Text formats, exact: `[tool call <toolUseId>: <name> <input JSON>]` and `[tool result <toolUseId>: <JSON string of the text>]`.
- Extra request field, exact: `{"tool_choice": {"type": "none"}}`.
- Stream guard error text, exact: `the model called a tool, but tool_choice is None`.
- Log target: `paigasus::bedrock::translate`.
- Run `cargo fmt --all` and `cargo clippy -p paigasus-helikon-providers-bedrock --all-features --all-targets -- -D warnings` before each commit.
- Work in the foreground. Do not end a task while a cargo command still runs.
- Do not run `git checkout`, `git switch`, `git reset`, `git stash` or `git merge`. Stay on `feature/sma-833-bedrock-toolchoicenone-sends-no-toolconfig` in `/Users/smaschek/dev/paigasus/paigasus-helikon/.worktrees/sma-833`. Stage files with explicit paths, never `git add -A`.

## Review Focus

1. A history that calls two different tools and one tool twice: Claude gets each name once, in first-occurrence order. Pinned in Task 3 (`claude_derives_each_name_once_in_order`).
2. A tool input with nested objects and several keys: the text history must be byte-stable across runs (sorted keys). Pinned in Task 1 and Task 3 (`text_history_sorts_input_keys`).
3. A tool result that holds `]`, `"` and a newline: it must not close the bracket. Pinned in Task 3 (`text_history_escapes_result_text`).
4. A `None` request on the forced path that later falls back to the native path (rule B): both builds must give the same `None` shape. Pinned in Task 4 (`none_shape_is_the_same_on_both_paths`).
5. A request with `None` and no tool round: its wire shape must not change. Pinned in Task 4 by the existing snapshot `snap_tool_choice_none_with_user_tools_omits_tool_config` (it must stay unchanged) and `none_without_tool_round_keeps_shape_on_nova`.

---

### Task 1: `document_to_value`

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/document.rs`

**Interfaces:**
- Produces: `pub(crate) fn document_to_value(d: &Document) -> serde_json::Value` (in `crate::document`).

- [ ] **Step 1: Write the failing tests** in the existing `tests` module:

```rust
#[test]
fn document_to_value_round_trips_nested_values() {
    let v = json!({"b": [1, -2, 1.5, null, true], "a": {"z": "x", "y": {}}});
    assert_eq!(document_to_value(&value_to_document(&v)), v);
}

#[test]
fn document_to_value_sorts_object_keys() {
    let v = json!({"zeta": 1, "alpha": 2, "mid": 3});
    let s = serde_json::to_string(&document_to_value(&value_to_document(&v))).unwrap();
    assert_eq!(s, r#"{"alpha":2,"mid":3,"zeta":1}"#);
}
```

- [ ] **Step 2: Run and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib document::`
Expected: compile error, `document_to_value` not found.

- [ ] **Step 3: Implement `document_to_value`.** Map each `Document` variant to the matching `Value`. `Number::PosInt` → `u64`, `NegInt` → `i64`, `Float` → `serde_json::Number::from_f64`, and a non-finite float → `Value::Null`. `Document::Object` collects into a `serde_json::Map`; it is sorted because `preserve_order` is off. Update the module doc comment ("provides converters in both directions").

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib document::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/src/document.rs
git commit -m "fix(providers): SMA-833 add document_to_value for bedrock documents"
```

### Task 2: `ModelFamily::tool_choice_none_strategy`

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/family.rs`

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub(crate) enum ToolChoiceNoneStrategy { AnthropicNoneField, TextHistory }` in `crate::family`.
  - `pub(crate) fn tool_choice_none_strategy(self) -> ToolChoiceNoneStrategy` on `ModelFamily`, next to `supports_forced_tool_choice`.

- [ ] **Step 1: Write the failing test** in the `family.rs` tests:

```rust
#[test]
fn tool_choice_none_strategy_per_family() {
    use ToolChoiceNoneStrategy::*;
    assert_eq!(Anthropic.tool_choice_none_strategy(), AnthropicNoneField);
    for f in [AmazonNova, AmazonTitan, Llama, Mistral, Cohere, Unknown] {
        assert_eq!(f.tool_choice_none_strategy(), TextHistory, "{f:?}");
    }
}
```

- [ ] **Step 2: Run and see it fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib family::`
Expected: compile error.

- [ ] **Step 3: Implement.** `Anthropic` → `AnthropicNoneField`; every other variant → `TextHistory`. The doc comment cites probes P4 (Nova rejects the field) and P9 (spec section 3).

- [ ] **Step 4: Run and see it pass** (same command). Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/src/family.rs
git commit -m "fix(providers): SMA-833 add a per-family strategy for tool_choice none"
```

### Task 3: the `tool_choice_none` translate module

**Files:**
- Create: `crates/paigasus-helikon-providers-bedrock/src/translate/tool_choice_none.rs`
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs` (add `pub(crate) mod tool_choice_none;`)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/native.rs` (remove `has_tool_round`, import it from the new module)

**Interfaces:**
- Consumes: `document_to_value` (Task 1); `ModelFamily::tool_choice_none_strategy`, `ToolChoiceNoneStrategy` (Task 2); the existing `tool_specs(&[ToolDef], Ruleset) -> Result<Vec<ToolSpecification>, ModelError>`.
- Produces:
  - `pub(crate) fn has_tool_round(messages: &[Message]) -> bool` (moved unchanged from `native.rs`).
  - `pub(crate) struct NoneOutcome { pub(crate) messages: Vec<Message>, pub(crate) tools: Vec<Tool>, pub(crate) tool_choice_none: bool }`.
  - `pub(crate) fn apply_tool_choice_none(messages: Vec<Message>, user_tools: &[ToolDef], family: ModelFamily, ruleset: Ruleset) -> Result<NoneOutcome, ModelError>`.

Behavior of `apply_tool_choice_none` (spec 5.1, 5.3 and 5.4):
- No tool round → return the messages unchanged, `tools` empty, flag `false`. Do not call `tool_specs`.
- `AnthropicNoneField` → `tools` = user specs from `tool_specs`, then a derived spec for each `toolUse` name not yet in the list (first occurrence, no duplicates). If the `ToolSpecification` builder refuses a name, skip it with a `debug!`. If `tools` is still empty, return the messages unchanged, `tools` empty, flag `false`. Otherwise the flag is `true`. Messages unchanged.
- `TextHistory` → replace each `ToolUse` and `ToolResult` block in place with the exact text formats from Global Constraints. The input is `serde_json::to_string(&document_to_value(input))`. The result text is all `ToolResultContentBlock::Text` contents joined with `\n`, then `serde_json::to_string` of that string. `tools` empty, flag `false`. Log one `debug!` when at least one block changed.

- [ ] **Step 1: Write the failing tests** in a `tests` module in the new file. Make the messages with `items_to_messages` from core `Item`s, as `translate/mod.rs` tests do (a user text, an assistant `ContentPart::ToolUse`, an `Item::ToolResult`). Add these tests:

```rust
// Claude, history calls get_balance, user tools = [get_balance]
#[test] fn claude_keeps_user_tools_and_sets_flag()
//   assert!(out.tool_choice_none); assert_eq!(names(&out.tools), ["get_balance"]);
//   assert_eq!(out.messages, input_messages);

// Claude, no user tools
#[test] fn claude_derives_spec_from_history()
//   names == ["get_balance"]; description == "A tool that this conversation used earlier.";
//   the ToolInputSchema::Json document, through document_to_value, == json!({"type":"object"})

// Claude, user tools = [search], history calls get_balance, then search, then get_balance again
#[test] fn claude_derives_each_name_once_in_order()
//   names == ["search", "get_balance"]

// Claude, history calls "__paigasus_structured_output__", no user tools
#[test] fn claude_accepts_reserved_name_from_history()
//   names == ["__paigasus_structured_output__"]; flag true

// Claude, user tool named "__paigasus_structured_output__", tool round present
#[test] fn claude_rejects_reserved_user_tool_name()
//   assert!(matches!(err, ModelError::Other(_)))

// Claude, history = only an Item::ToolResult (orphan) + user text, no user tools
#[test] fn claude_orphan_results_keep_shape()
//   flag false; tools empty; messages unchanged

// Nova and Llama
#[test] fn text_history_replaces_tool_blocks()
//   no ToolUse/ToolResult blocks remain; flag false; tools empty;
//   assistant block text == r#"[tool call tu_1: get_balance {"account_id":"acc_123"}]"#
//   user block text     == r#"[tool result tu_1: "$1,234.56"]"#

#[test] fn text_history_sorts_input_keys()
//   args json!({"z": 1, "a": {"y": 2, "b": 3}}) →
//   r#"[tool call tu_1: get_balance {"a":{"b":3,"y":2},"z":1}]"#

#[test] fn text_history_escapes_result_text()
//   result text "x]\n\"y\"" → r#"[tool result tu_1: "x]\n\"y\""]"#

// Claude and Nova, history with no tool blocks
#[test] fn no_tool_round_is_unchanged()
//   flag false; tools empty; messages unchanged; tool_specs not called (a reserved user tool name gives Ok)
```

- [ ] **Step 2: Run and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib translate::tool_choice_none::`
Expected: compile error.

- [ ] **Step 3: Implement the module** to the behavior above. Build derived specs with `ToolSpecification::builder().name(..).description(..).input_schema(ToolInputSchema::Json(value_to_document(&json!({"type":"object"})))).build()`. Move `has_tool_round` here and change `native.rs` to import it.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib translate::`
Expected: PASS, and all existing `translate::` tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/src/translate/tool_choice_none.rs crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs crates/paigasus-helikon-providers-bedrock/src/translate/native.rs
git commit -m "fix(providers): SMA-833 add the tool_choice none history strategies"
```

### Task 4: wire the strategy into both builders and the send call

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs` (`PreparedConverse`, `build_request` `none_choice` branch, `to_wire_json`, tests)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/native.rs` (`build_native`)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/model.rs` (`send_converse_stream`)
- Test snapshots: `crates/paigasus-helikon-providers-bedrock/src/translate/snapshots/` (two new `.snap` files)

**Interfaces:**
- Consumes: `apply_tool_choice_none`, `NoneOutcome`, `has_tool_round` (Task 3); the existing `assemble_tool_config(Vec<Tool>, Option<BedrockToolChoice>)`.
- Produces: `PreparedConverse.tool_choice_none: bool` (doc: "Send the Anthropic `tool_choice: none` field in `additionalModelRequestFields`."). Every other constructor of `PreparedConverse` sets it to `false`.

Wiring:
- `build_request`, `none_choice` branch (after the existing conflict guard): call `apply_tool_choice_none(translated.messages, &req.tools, family, ruleset)`, then `assemble_tool_config(outcome.tools, None)`. `synthesizing: false`. Update the branch comment and the `translate_tool_choice` doc comment ("`ToolChoice::None` is handled before this function").
- `build_native`: when `tool_choice` is `Some(ToolChoice::None)` and `structured` is `false`, call the helper on the translated messages and use `outcome.tools` and `outcome.messages`. The flag comes from `outcome`. When `structured` is `true`, keep today's code (no user tools, the anchor). The anchor check then sees the same messages as today.
- `send_converse_stream`: when `p.tool_choice_none` is `true`, call `.set_additional_model_request_fields(Some(Document::Object(..)))` with `{"tool_choice": {"type": "none"}}`. Build the document in one private function `additional_fields(p: &PreparedConverse) -> Option<Document>` so a later key merges there.
- `to_wire_json`: add `"additionalModelRequestFields": {"tool_choice": {"type": "none"}}` only when the flag is set, so existing snapshots do not change.

- [ ] **Step 1: Write the failing tests** in `translate/mod.rs` tests. Add a `nova_cfg()` helper (`make_cfg("global.amazon.nova-2-lite-v1:0")`) and a `haiku55_cfg()` helper (`make_cfg("global.anthropic.claude-haiku-5-5")`).

```rust
#[test] fn snap_forced_path_tool_choice_none_after_tool_round_on_claude()
//   haiku55_cfg, tool_round_history(), tools = [get_balance_tool()], None
//   p.tool_choice_none; !p.synthesizing; tool_names == ["get_balance"];
//   w["toolConfig"]["toolChoice"].is_null();
//   w["additionalModelRequestFields"] == json!({"tool_choice":{"type":"none"}}); snapshot

#[test] fn snap_tool_choice_none_after_tool_round_on_nova()
//   nova_cfg, same request; w["toolConfig"].is_null(); !p.tool_choice_none;
//   w.get("additionalModelRequestFields").is_none(); message text blocks as in Task 3; snapshot

#[test] fn native_tool_choice_none_after_tool_round_on_claude()
//   sonnet55_cfg, same request, no response format: flag true, tool_names == ["get_balance"], no outputConfig

#[test] fn native_json_object_tool_choice_none_keeps_anchor()
//   sonnet55_cfg, tool round, None, ResponseFormat::JsonObject: p.synthesizing; tool names == [SYNTHESIZED_TOOL_NAME]; flag false

#[test] fn native_text_history_through_build_native_on_nova()
//   native::build_native(&nova_cfg(), &req) directly: toolConfig null, flag false, no ToolUse blocks

#[test] fn none_shape_is_the_same_on_both_paths()
//   for haiku55_cfg: to_wire_json(build_request(.., ForcedTool)) and (.., Native) agree on
//   "toolConfig" and "additionalModelRequestFields"

#[test] fn none_without_tool_round_keeps_shape_on_nova()
//   nova_cfg, vec![user_text("hi")], tools = [get_balance_tool()], None: toolConfig null, flag false

#[test] fn other_tool_choices_never_set_the_flag()
//   for tc in [None, Some(Auto), Some(Required), Some(Tool{"get_balance"})] on claude_cfg and sonnet55_cfg
//   with tool_round_history(): !p.tool_choice_none
```

Also, in the existing `snap_native_tool_choice_none_json_schema_finalizing_anchor`, add `assert!(!p.tool_choice_none);`. Its snapshot must not change.

- [ ] **Step 2: Run and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib translate::`
Expected: compile error (no `tool_choice_none` field).

- [ ] **Step 3: Implement the wiring** described above.

- [ ] **Step 4: Run, accept only the two new snapshots, and check that no old one changed**

Run: `cargo insta test -p paigasus-helikon-providers-bedrock --lib --accept-unseen` (or `INSTA_UPDATE=new cargo test ...` and review), then `git status --short crates/paigasus-helikon-providers-bedrock/src/translate/snapshots`.
Expected: all tests pass. `git status` shows exactly two new `.snap` files (`??`) and no modified `.snap` file (` M`).

- [ ] **Step 5: Run the crate tests**

Run: `cargo test -p paigasus-helikon-providers-bedrock --all-features`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs crates/paigasus-helikon-providers-bedrock/src/translate/native.rs crates/paigasus-helikon-providers-bedrock/src/model.rs crates/paigasus-helikon-providers-bedrock/src/translate/snapshots/<the two new files>
git commit -m "fix(providers): SMA-833 send a valid tool history when tool_choice is none"
```

### Task 5: the stream-translator guard

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/stream.rs`
- Modify: `crates/paigasus-helikon-providers-bedrock/src/model.rs` (`invoke`)

**Interfaces:**
- Produces: `pub(crate) fn forbid_tool_calls(mut self, forbid: bool) -> Self` on `StreamTranslator`, next to `native`; a new field `forbid_tool_calls: bool`.

Behavior: on `ContentBlockStart::ToolUse` when `forbid_tool_calls` is `true`, and the block is not the synthesized anchor in synthesizing mode, push `Err(ModelError::Other(anyhow!("the model called a tool, but tool_choice is None")))` once, do not record the block, and emit no `ToolCallDelta` for its deltas. In `invoke`, compute `let forbid = matches!(request.model_settings.tool_choice, Some(ToolChoice::None));` before `request` moves into the rebuild closure, and build the translator with `.forbid_tool_calls(forbid)`.

- [ ] **Step 1: Write the failing tests** in `stream.rs` tests, using the existing `tu_start`/delta/`message_stop` helpers:

```rust
#[test] fn forbid_tool_calls_turns_a_tool_use_into_an_error()
//   StreamTranslator::new(false).forbid_tool_calls(true); feed start("get_balance") + input delta + stop(ToolUse)
//   exactly one Err whose to_string() contains "tool_choice is None"; zero ToolCallDelta

#[test] fn tool_use_without_forbid_is_unchanged()
//   same events, forbid false: ToolCallDelta events present, no Err

#[test] fn forbid_tool_calls_allows_the_synthesized_anchor()
//   StreamTranslator::new(true).forbid_tool_calls(true); anchor start + delta + stop(ToolUse)
//   no Err; TokenDelta present (the same events as the existing synthesizing test)
```

- [ ] **Step 2: Run and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib stream::`
Expected: compile error.

- [ ] **Step 3: Implement** the field, the builder method, the check in the `ToolUse` start arm, and the `invoke` wiring.

- [ ] **Step 4: Run and see them pass**

Run: `cargo test -p paigasus-helikon-providers-bedrock --all-features`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/src/stream.rs crates/paigasus-helikon-providers-bedrock/src/model.rs
git commit -m "fix(providers): SMA-833 reject a tool call on a tool_choice none request"
```

### Task 6: live tests, README, and the live run

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/tests/live.rs`
- Modify: `crates/paigasus-helikon-providers-bedrock/README.md` (the tool-choice text near lines 83 and 99)

**Interfaces:**
- Consumes: the public `BedrockModel` API only.

- [ ] **Step 1: Add two live tests** (`#[tokio::test] #[ignore]`, `skip_if_missing_env()` first). Both use the history: `user("What is the weather in Paris? Use the tool.")`, an `Item::AssistantMessage` with `ContentPart::ToolUse { call_id: "t1", name: "get_weather", args: json!({"city": "Paris"}) }`, `tool_result("t1", "18 C, sunny")`, then the final user turn. `req.tools` = a `get_weather` `ToolDef` with schema `{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}`. `tool_choice = Some(ToolChoice::None)`, `max_output_tokens = Some(1024)`.
  - `tool_choice_none_after_tool_round_answers_in_text`: final user turn `"Summarize the weather result in one sentence."`. Assert: no `Err`, no `ToolCallDelta`, `Finish` reason is not `ToolCalls`, text not empty.
  - `tool_choice_none_after_tool_round_makes_no_tool_call`: final user turn `"Now also check the weather in Berlin, then answer."`. Assert: no `Err`, no `ToolCallDelta`, `Finish` reason is not `ToolCalls`. Empty text is allowed.

- [ ] **Step 2: Compile them**

Run: `cargo test -p paigasus-helikon-providers-bedrock --test live --no-run`
Expected: builds.

- [ ] **Step 3: Run the live tests on the three models** (needs a valid `maip-dev-admin` SSO session; if `aws sts get-caller-identity --profile maip-dev-admin` fails, stop and ask the user to run `aws sso login --sso-session work`)

```bash
for m in global.anthropic.claude-sonnet-5-5 global.anthropic.claude-haiku-5-5 global.amazon.nova-2-lite-v1:0; do
  AWS_PROFILE=maip-dev-admin AWS_DEFAULT_REGION=us-east-1 BEDROCK_MODEL_ID=$m \
    cargo test -p paigasus-helikon-providers-bedrock --test live tool_choice_none -- --ignored --nocapture
done
```

Expected: 2 passed for each of the 3 models. Record the output for the PR body.

- [ ] **Step 4: Update the README.** One short paragraph: with `ToolChoice::None` after a tool round, Claude models get the history tools plus the Anthropic `tool_choice: none` field; other families get the tool history as text and no tools; the provider fails the stream if the model still calls a tool.

- [ ] **Step 5: Run the full local gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
npx markdownlint-cli2
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/tests/live.rs crates/paigasus-helikon-providers-bedrock/README.md
git commit -m "test(providers): SMA-833 live-test tool_choice none after a tool round"
```
