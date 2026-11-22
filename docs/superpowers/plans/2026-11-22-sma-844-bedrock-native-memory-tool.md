# SMA-844 Bedrock native memory tool: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Claude model on Bedrock gets the native `memory_*` tool entry when a `ToolDef` declares the `anthropic` provider tool type. All other models and types keep the generic `toolSpec`.

**Architecture:** The tool list is split once per request into generic `toolSpec`s and typed JSON entries. The typed entries, and the tool choice when a typed entry exists, go in `additionalModelRequestFields`. A placeholder `toolSpec` fills `toolConfig` when the history holds a tool round and no other spec exists.

**Tech Stack:** Rust 1.94, `aws-sdk-bedrockruntime` 1.157.0, `insta` JSON snapshots, `tokio` live tests.

**Spec:** `docs/superpowers/specs/2026-11-22-sma-844-bedrock-native-memory-tool-design.md` (read it first; section numbers below refer to it).

## Global Constraints

- Work only in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.worktrees/sma-844`, branch `feature/sma-844-send-the-native-anthropic-memory-tool-type`. Use `cd "$WT" || exit 1` before every command.
- Never run `git checkout`, `switch`, `reset`, `stash`, `rebase` or `merge`. Never `git add -A`; add explicit paths.
- Commit messages: `<type>(<scope>): SMA-844 <lowercase subject>`. Scope is `providers` for crate code, `book` or `docs` for documentation. End each message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Commits are signed through 1Password. If a commit fails with "failed to fill whole buffer", stop and report. Do not bypass signing.
- Before each commit: `cargo fmt --all` and `cargo clippy -p paigasus-helikon-providers-bedrock --all-features --all-targets -- -D warnings`.
- Work in the foreground. Do not background a cargo command. Do not end a task until the command has a final result.
- The gate is `native_tool_types && family == ModelFamily::Anthropic`, in one method only (spec 5.1).
- A typed tool: the first `ProviderToolType` with `provider == "anthropic"`, and its `type_name` starts with `memory_` (spec 5.2).
- The typed entry: `params` copy, remove `cache_control`, then set `type` and `name` (spec 5.3).
- The placeholder: name `SYNTHESIZED_TOOL_NAME` (`__paigasus_structured_output__`), description `"Placeholder. Never call this tool."`, schema `{"type": "object"}`, `synthesizing = false` (spec 5.7).
- Log target for all new `debug!` lines: `paigasus::bedrock::translate`.
- Existing snapshot files must not change. New snapshots are written with `INSTA_UPDATE=always`, then read and checked by hand before the commit.
- Write all doc comments and docs in ASD-STE100 Simplified Technical English (short sentences, active voice, approved words).

## Review Focus

1. A `ToolChoice::Tool` that names `__paigasus_structured_output__` when only typed tools exist and the placeholder is present. It must give the "not defined in the tool list" error. The placeholder is never a valid name. Test: Task 4 `typed_tool_choice_cannot_select_the_placeholder`.
2. A tool with two `anthropic` declarations, `computer_20251124` first and `memory_20250818` second. The first `anthropic` entry wins, so the tool gets the generic spec. Test: Task 2 `first_anthropic_entry_wins_even_when_unsupported`.
3. An application inference profile ARN with `.family(ModelFamily::Anthropic)`. It gets the typed path. Test: Task 1 `family_override_opens_the_gate`.
4. Native path, `JsonObject` (not only `JsonSchema`) after a memory round with only typed tools. It must get the placeholder with `synthesizing == false`, not the anchor. Test: Task 5 `native_json_object_after_memory_round_uses_placeholder`.
5. Native structured output after a tool round with a typed **and** a generic tool. No placeholder and no anchor, `synthesizing == false`, as today with two generic tools. Test: Task 5 `native_structured_with_generic_and_typed_adds_no_placeholder`.

---

## File Structure

| File | Change |
|------|--------|
| `crates/paigasus-helikon-providers-bedrock/src/builder.rs` | `Config.native_tool_types`, `Config::sends_typed_tools`, builder method |
| `crates/paigasus-helikon-providers-bedrock/src/translate/additional.rs` | **new**: `AdditionalFields`, `AdditionalToolChoice` |
| `crates/paigasus-helikon-providers-bedrock/src/translate/tools.rs` | `ToolSet`, `partition_tools`, `placeholder_spec`, `ANTHROPIC_PROVIDER_ID` |
| `crates/paigasus-helikon-providers-bedrock/src/translate/tool_choice_none.rs` | takes a `ToolSet` |
| `crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs` | `PreparedConverse.additional`, forced path, `to_wire_json` |
| `crates/paigasus-helikon-providers-bedrock/src/translate/native.rs` | native path |
| `crates/paigasus-helikon-providers-bedrock/src/translate/typed_tests.rs` | **new**: snapshot and path tests for typed tools |
| `crates/paigasus-helikon-providers-bedrock/src/model.rs` | sends `AdditionalFields` |
| `crates/paigasus-helikon-providers-bedrock/src/stream.rs` | tests only |
| `crates/paigasus-helikon-providers-bedrock/src/fallback.rs` | tests only |
| `crates/paigasus-helikon-providers-bedrock/tests/live.rs` | five live tests |
| `crates/paigasus-helikon/tests/provider_tool_type_ids.rs` | **new**: provider id drift test |
| `crates/paigasus-helikon-providers-bedrock/README.md`, `docs/book/src/concepts/model-providers.md`, `docs/book/src/concepts/tools.md` | docs |

---

### Task 1: The gate, the builder method and the additional-fields type

A refactor with one new option. The wire output does not change.

**Files:**
- Modify: `src/builder.rs` (`Config` at line 54, `offline_config` at line 104, `BedrockModelBuilder` at line 127, `build`)
- Create: `src/translate/additional.rs`
- Modify: `src/translate/mod.rs` (`PreparedConverse`, `has_forced_tool_choice`, `to_wire_json` lines 497-501, `Config { .. }` literals at lines 1008 and 1134, every `tool_choice_none` use)
- Modify: `src/translate/native.rs` (sets `additional` instead of `tool_choice_none`)
- Modify: `src/model.rs` (`additional_fields` at line 44, the test `additional_fields_carry_the_none_flag`)

**Interfaces:**
- Produces:
  - `Config.native_tool_types: bool`; `impl Config { pub(crate) fn sends_typed_tools(&self) -> bool }`
  - `pub fn native_tool_types(mut self, enabled: bool) -> Self` on `BedrockModelBuilder` (field `native_tool_types: bool`, default `true`)
  - `crate::translate::additional::{AdditionalFields, AdditionalToolChoice}`:
    - `#[derive(Debug, Clone, Default, PartialEq)] pub(crate) struct AdditionalFields { pub(crate) tools: Vec<serde_json::Value>, pub(crate) tool_choice: Option<AdditionalToolChoice> }`
    - `#[derive(Debug, Clone, PartialEq, Eq)] pub(crate) enum AdditionalToolChoice { None, Any, Tool(String) }`
    - `impl AdditionalFields { pub(crate) fn to_value(&self) -> Option<serde_json::Value>; pub(crate) fn is_forced(&self) -> bool }`
  - `PreparedConverse.additional: AdditionalFields` replaces `PreparedConverse.tool_choice_none: bool`.

- [ ] **Step 1: Write the failing tests**

In `additional.rs` (`#[cfg(test)] mod tests`):

```rust
#[test]
fn empty_fields_give_no_value() {
    assert_eq!(AdditionalFields::default().to_value(), None);
}
#[test]
fn choices_have_the_anthropic_shape() {
    let f = |c| AdditionalFields { tools: vec![], tool_choice: Some(c) }.to_value().unwrap();
    assert_eq!(f(AdditionalToolChoice::None), json!({"tool_choice": {"type": "none"}}));
    assert_eq!(f(AdditionalToolChoice::Any), json!({"tool_choice": {"type": "any"}}));
    assert_eq!(f(AdditionalToolChoice::Tool("memory".into())),
               json!({"tool_choice": {"type": "tool", "name": "memory"}}));
}
#[test]
fn tools_are_sent_in_order() {
    let v = AdditionalFields { tools: vec![json!({"a":1}), json!({"b":2})], tool_choice: None }.to_value();
    assert_eq!(v, Some(json!({"tools": [{"a":1}, {"b":2}]})));
}
#[test]
fn only_any_and_tool_are_forced() {
    let f = |c| AdditionalFields { tools: vec![], tool_choice: c }.is_forced();
    assert!(!f(None));
    assert!(!f(Some(AdditionalToolChoice::None)));
    assert!(f(Some(AdditionalToolChoice::Any)));
    assert!(f(Some(AdditionalToolChoice::Tool("x".into()))));
}
```

In `builder.rs` tests:

```rust
#[test]
fn native_tool_types_defaults_to_true() { /* build with offline client; assert cfg.native_tool_types */ }
#[test]
fn native_tool_types_false_closes_the_gate() { /* claude id + .native_tool_types(false) => !sends_typed_tools() */ }
#[test]
fn gate_is_open_only_for_anthropic() { /* claude id => true; "us.amazon.nova-pro-v1:0" => false */ }
#[test]
fn family_override_opens_the_gate() {
    // "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/abc123"
    // with .family(ModelFamily::Anthropic) => sends_typed_tools() == true
}
```

Use the same construction that the existing builder tests use (`anthropic_override_on_sonnet55_keeps_native_path` at line 707) to reach the built `Config`.

- [ ] **Step 2: Run the tests and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib additional`, then the same command with `builder`
Expected: compile errors (the types and method do not exist).

- [ ] **Step 3: Implement**

- `AdditionalFields::to_value`: an object with `tools` only when not empty, and `tool_choice` only when set. `None` when both are absent.
- `model::additional_fields(p)` becomes `p.additional.to_value().map(|v| value_to_document(&v))`.
- `to_wire_json`: `if let Some(v) = p.additional.to_value() { wire["additionalModelRequestFields"] = v; }`.
- `has_forced_tool_choice`: the current check `|| self.additional.is_forced()`.
- Every place that set `tool_choice_none: x` now sets `additional: AdditionalFields { tools: vec![], tool_choice: x.then_some(AdditionalToolChoice::None) }`. Every read of `p.tool_choice_none` becomes `p.additional.tool_choice == Some(AdditionalToolChoice::None)`. Update the tests that read the old field. Rename the `model.rs` test to `additional_fields_carry_the_none_choice` and build `p.additional` instead of the flag.
- `NoneOutcome.tool_choice_none` stays a `bool` in this task (Task 3 changes it).
- Builder doc comment: copy the meaning of `AnthropicModelBuilder::native_tool_types` (`crates/paigasus-helikon-providers-anthropic/src/builder.rs:140-148`), and add that only Claude models and only `memory_*` types are affected.

- [ ] **Step 4: Run all crate tests**

Run: `cargo test -p paigasus-helikon-providers-bedrock`
Expected: PASS. `git status --short crates/paigasus-helikon-providers-bedrock/src/translate/snapshots` shows no change.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/src/builder.rs crates/paigasus-helikon-providers-bedrock/src/translate/additional.rs crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs crates/paigasus-helikon-providers-bedrock/src/translate/native.rs crates/paigasus-helikon-providers-bedrock/src/model.rs
git commit -m "feat(providers): SMA-844 add the native_tool_types option to the bedrock builder"
```

---

### Task 2: Partition of the tool list, the typed entry and the placeholder

**Files:**
- Modify: `src/translate/tools.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `pub(crate) const ANTHROPIC_PROVIDER_ID: &str = "anthropic";` (doc comment names `paigasus_helikon_providers_anthropic::PROVIDER_ID`)
  - `pub(crate) const PLACEHOLDER_DESCRIPTION: &str = "Placeholder. Never call this tool.";`
  - `#[derive(Debug, Clone, PartialEq)] pub(crate) struct TypedTool { pub(crate) name: String, pub(crate) entry: serde_json::Value }`
  - `#[derive(Debug, Clone)] pub(crate) struct ToolSet { pub(crate) generic: Vec<ToolSpecification>, pub(crate) typed: Vec<TypedTool> }`
  - `impl ToolSet { pub(crate) fn has_typed(&self) -> bool; pub(crate) fn has_any(&self) -> bool; pub(crate) fn is_typed_name(&self, name: &str) -> bool; pub(crate) fn typed_entries(&self) -> Vec<serde_json::Value> }`
  - `pub(crate) fn partition_tools(defs: &[ToolDef], ruleset: Ruleset, typed_enabled: bool) -> Result<ToolSet, ModelError>`
  - `pub(crate) fn placeholder_spec() -> Result<ToolSpecification, ModelError>`
  - `tool_specs` stays and returns `partition_tools(defs, ruleset, false)?.generic`.

- [ ] **Step 1: Write the failing tests**

Replace `provider_type_is_ignored` with these tests (helper `memory_def()` = `ToolDef::new("memory", "custom description", schema).with_provider_types(vec![ProviderToolType::new("anthropic", "memory_20250818")])`):

- `gate_closed_gives_the_generic_spec`: `partition_tools(&[memory_def()], Strict, false)` has `typed.is_empty()` and `format!("{:?}", generic)` equals the result for the same def without a declaration.
- `memory_type_gives_exact_typed_entry`: with `true`, `typed == [TypedTool { name: "memory", entry: json!({"type": "memory_20250818", "name": "memory"}) }]` and `generic.is_empty()`.
- `params_are_merged`: params `{"max_characters": 10000}` give `{"type":"memory_20250818","name":"memory","max_characters":10000}`.
- `type_and_name_in_params_do_not_override`: params `{"type":"x","name":"y"}` give `type == "memory_20250818"`, `name == "memory"`.
- `cache_control_in_params_is_removed`.
- `non_anthropic_declaration_gives_the_generic_spec`: provider `"openai"`.
- `unsupported_anthropic_type_gives_the_generic_spec`: type `computer_20251124`.
- `first_anthropic_entry_wins_even_when_unsupported`: declarations `[anthropic/computer_20251124, anthropic/memory_20250818]` give the generic spec.
- `typed_tools_keep_request_order`: defs `[memory_a(typed), plain, memory_b(typed)]` give `typed` names `["memory_a", "memory_b"]` and one generic spec.
- `reserved_name_is_rejected_for_a_typed_tool`: a typed def named `SYNTHESIZED_TOOL_NAME` gives `ModelError::Other` containing "reserved".
- `placeholder_has_the_reserved_name_and_an_object_schema`: `placeholder_spec()` has `name() == SYNTHESIZED_TOOL_NAME`, `description() == Some(PLACEHOLDER_DESCRIPTION)`, and a JSON input schema.
- `provider_id_matches_the_anthropic_provider`: `assert_eq!(ANTHROPIC_PROVIDER_ID, "anthropic")`.

- [ ] **Step 2: Run the tests and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib translate::tools`
Expected: compile errors.

- [ ] **Step 3: Implement**

- The reserved-name check runs for every def before the split.
- The match: `def.provider_types.iter().find(|p| p.provider == ANTHROPIC_PROVIDER_ID)`, then typed only if `typed_enabled && pt.type_name.starts_with("memory_")`.
- The entry follows `crates/paigasus-helikon-providers-anthropic/src/translate/tools.rs:23-31`.
- `debug!` lines: one per typed entry (`tool`, `type`), and one when an `anthropic` declaration gets the generic spec, with `reason = "gate closed"` or `reason = "type not supported on bedrock"`.
- The placeholder uses `value_to_document(&json!({"type": "object"}))`. `placeholder_spec` logs `debug!("added the placeholder tool spec")`, so each call site gets the log line without its own copy.

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test -p paigasus-helikon-providers-bedrock`
Expected: PASS, no snapshot change.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/src/translate/tools.rs
git commit -m "feat(providers): SMA-844 split bedrock tools into generic specs and typed memory entries"
```

---

### Task 3: `ToolChoice::None` takes the tool set

**Files:**
- Modify: `src/translate/tool_choice_none.rs` (`NoneOutcome`, `apply_tool_choice_none`, tests from line 180)
- Modify: `src/translate/mod.rs` (None branch of `build_request`, lines 222-238)
- Modify: `src/translate/native.rs` (None branch, lines 78-90)

**Interfaces:**
- Consumes: `ToolSet`, `placeholder_spec` (Task 2); `AdditionalFields`, `AdditionalToolChoice` (Task 1); `Config::sends_typed_tools` (Task 1).
- Produces:
  - `pub(crate) fn apply_tool_choice_none(messages: Vec<Message>, tools: &ToolSet, family: ModelFamily) -> Result<NoneOutcome, ModelError>`
  - `NoneOutcome { messages: Vec<Message>, tools: Vec<Tool>, additional: AdditionalFields }` (replaces the `tool_choice_none: bool` field).
  - `build_request` and `build_native` each make the `ToolSet` **once, at the top**, with `let set = partition_tools(&req.tools, ruleset, cfg.sends_typed_tools())?;`. They pass `&set` to `apply_tool_choice_none`. Tasks 4 and 5 use the same `set`; they do not call `tool_specs` again.

- [ ] **Step 1: Write the failing tests** (in `tool_choice_none.rs`)

Update the existing tests to the new signature (`partition_tools(tools, Strict, false)`; `out.tool_choice_none` becomes `out.additional.tool_choice == Some(AdditionalToolChoice::None)`). Add:

- `typed_only_memory_history_gives_placeholder_and_none`: tools = typed `memory` only, history = memory `toolUse` + `toolResult`, family Anthropic. Expect `tools` names `[SYNTHESIZED_TOOL_NAME]`, `additional.tools == [json!({"type":"memory_20250818","name":"memory"})]`, `additional.tool_choice == Some(None)`.
- `typed_name_gets_no_derived_spec`: tools = typed `memory` + generic `get_balance`, history with a `memory` call. Expect `tools` names `["get_balance"]` (no `memory`, no placeholder), typed entry present, none choice.
- `typed_tools_without_tool_round_send_nothing`: history without tool blocks. Expect `tools` empty and `additional == AdditionalFields::default()`.
- `text_history_family_ignores_typed_set`: family `AmazonNova` with a set made with `typed_enabled = false`. Expect the same result as today.

In `mod.rs` tests add `none_shape_is_the_same_on_both_paths_with_typed_tool`: the copy of `none_shape_is_the_same_on_both_paths` (line 1638) with a typed `memory` tool and a memory history. Compare `toolConfig` and `additionalModelRequestFields` of both paths.

- [ ] **Step 2: Run the tests and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib tool_choice_none`
Expected: compile errors.

- [ ] **Step 3: Implement**

- `AnthropicNoneField`: `specs = tools.generic.clone()`; `seen` holds generic and typed names; derived specs as today; then, if `tools.has_typed()`: add `placeholder_spec()?` when `specs` is empty, set `additional.tools = tools.typed_entries()`, and always set the none choice. Without typed tools, keep today's early return when `specs` is empty.
- Both callers copy `outcome.additional` into `PreparedConverse.additional`.
- The native structured `None` branch (`if structured { Vec::new() }`) does not change: it sends no typed tools.

- [ ] **Step 4: Run all crate tests**

Run: `cargo test -p paigasus-helikon-providers-bedrock`
Expected: PASS, no existing snapshot change.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/src/translate/tool_choice_none.rs crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs crates/paigasus-helikon-providers-bedrock/src/translate/native.rs
git commit -m "feat(providers): SMA-844 send typed tools with the bedrock tool_choice none strategy"
```

---

### Task 4: Forced-tool path

**Files:**
- Modify: `src/translate/mod.rs` (`build_request` after the None branch, lines 240-296; `check_tool_name` at line 81; add `#[cfg(test)] mod typed_tests;`)
- Create: `src/translate/typed_tests.rs`

**Interfaces:**
- Consumes: Tasks 1-3.
- Produces: `fn check_tool_name(tools: &[Tool], typed: &ToolSet, name: &str) -> Result<(), ModelError>` (the placeholder is not in `tools` when the check runs, so it is never valid). This task also updates the call in `native.rs` (line 102) to pass `&set`, so the crate compiles. Task 5 changes the rest of the native path.
- `typed_tests.rs` helpers, reused by Task 5: `fn memory_tool() -> ToolDef`, `fn balance_tool() -> ToolDef`, `fn memory_round() -> Vec<Item>` (user text, assistant `ToolCall` for `memory` with `{"command":"view","path":"/memories"}`, its `ToolResult`), `fn cfg(id: &str) -> Config` (= `crate::builder::offline_config`), `fn forced(cfg, req)` and `fn native(cfg, req)` (call `build_request` with the explicit `OutputPath`), `fn tool_names(w: &Value) -> Vec<String>`. Model ids: forced `us.anthropic.claude-sonnet-4-6`, native `us.anthropic.claude-sonnet-5-5`, Nova `us.amazon.nova-pro-v1:0`. Copy the item shapes from the existing `none_after_tool_round` helper in `mod.rs`.

- [ ] **Step 1: Write the failing tests** (in `typed_tests.rs`, forced path)

Each test asserts the shape and ends with `assert_json_snapshot!(w)` where it says "snapshot".

- `snap_typed_only_first_turn`: `toolConfig` is null; `additionalModelRequestFields == {"tools":[{"type":"memory_20250818","name":"memory"}]}`. Snapshot.
- `snap_typed_and_generic`: `tool_names == ["get_balance"]`, `toolConfig.toolChoice` null, typed entry present. Snapshot.
- `typed_auto_sends_no_choice`: `ToolChoice::Auto`: no `toolChoice`, no `tool_choice` key.
- `snap_typed_required`: `Required` with typed + generic: `toolConfig.toolChoice` null, `tool_choice == {"type":"any"}`, `has_forced_tool_choice()`. Snapshot.
- `typed_required_with_only_typed_tools_is_valid`: no error, `tool_choice == {"type":"any"}`.
- `snap_typed_tool_choice_typed_name`: `Tool { name: "memory" }`: `tool_choice == {"type":"tool","name":"memory"}`. Snapshot.
- `typed_tool_choice_generic_name`: `Tool { name: "get_balance" }`: the same, in the additional fields only.
- `typed_tool_choice_unknown_name_is_error`: `Tool { name: "nope" }`: error containing "not defined in the tool list".
- `typed_tool_choice_cannot_select_the_placeholder`: only typed tools, `memory_round()`, `Tool { name: SYNTHESIZED_TOOL_NAME }`: the same error.
- `snap_typed_structured_output`: `JsonSchema` with a typed tool: `tool_names == [SYNTHESIZED_TOOL_NAME]`, `toolConfig.toolChoice` null, `tool_choice == {"type":"tool","name":SYNTHESIZED_TOOL_NAME}`, `synthesizing == true`. Snapshot.
- `snap_typed_only_tool_round_adds_placeholder`: only typed, `memory_round()`, `Auto`: `tool_names == [SYNTHESIZED_TOOL_NAME]`, description `PLACEHOLDER_DESCRIPTION`, `synthesizing == false`. Snapshot.
- `typed_and_generic_tool_round_adds_no_placeholder`: `tool_names == ["get_balance"]`.
- `snap_typed_none_after_tool_round`: only typed, `memory_round()`, `ToolChoice::None`: placeholder, typed entry, `tool_choice == {"type":"none"}`. Snapshot.
- `gate_closed_requests_are_unchanged`: for (Nova id, typed tool) and (forced Claude id with `native_tool_types = false` set on the `Config`), `to_wire_json` equals the result for the same request with a plain `memory` def without a declaration.

- [ ] **Step 2: Run the tests and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib typed_tests`
Expected: FAIL (wrong shapes, or compile errors).

- [ ] **Step 3: Implement** in `build_request`

- `let set = partition_tools(&req.tools, ruleset, cfg.sends_typed_tools())?;` once, before the None branch, and pass it to Task 3's call.
- `all_tools` = generic + synthesized tool, as today.
- `Required` without tools: the error only when `!set.has_any()` and not synthesizing.
- `check_tool_name(&all_tools, &set, name)`.
- When `set.has_typed()`: `toolConfig` choice is `None`. The additional choice is `Tool(SYNTHESIZED_TOOL_NAME)` when synthesizing, otherwise from the caller (`Required` → `Any`, `Tool{name}` → `Tool(name)`, `Auto` → nothing), only when `family.supports_forced_tool_choice()` (it is, for Anthropic). Then `additional.tools = set.typed_entries()`.
- Placeholder: when `set.has_typed() && all_tools.is_empty() && has_tool_round(&translated.messages)`, push `Tool::ToolSpec(placeholder_spec()?)`. Push it after `check_tool_name`.
- When `!set.has_typed()`, the code path is exactly today's.

- [ ] **Step 4: Write the new snapshots, read them, run all tests**

Run: `INSTA_UPDATE=always cargo test -p paigasus-helikon-providers-bedrock --lib typed_tests`, then read each new `src/translate/snapshots/*typed_tests*.snap` and confirm it matches the assertions above. Then `cargo test -p paigasus-helikon-providers-bedrock`.
Expected: PASS. `git status --short` shows only new snapshot files, no modified ones.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs crates/paigasus-helikon-providers-bedrock/src/translate/typed_tests.rs crates/paigasus-helikon-providers-bedrock/src/translate/snapshots/
git commit -m "feat(providers): SMA-844 send typed memory tools on the bedrock forced-tool path"
```

---

### Task 5: Native path

**Files:**
- Modify: `src/translate/native.rs` (lines 74-125)
- Modify: `src/translate/typed_tests.rs` (native tests)

**Interfaces:**
- Consumes: `partition_tools`, `placeholder_spec`, `check_tool_name(&all_tools, &set, name)` (Task 4), Task 3's None call.

- [ ] **Step 1: Write the failing tests** (native id `us.anthropic.claude-sonnet-5-5`)

- `snap_native_typed_json_schema_after_memory_round`: only typed, `memory_round()`, `JsonSchema`: `tool_names == [SYNTHESIZED_TOOL_NAME]` with description `PLACEHOLDER_DESCRIPTION`, `synthesizing == false`, `outputConfig` present, typed entry present, `!has_forced_tool_choice()`. Snapshot.
- `native_json_object_after_memory_round_uses_placeholder`: the same with `JsonObject`: `synthesizing == false`, the `JsonObjectAsText` notice.
- `native_structured_with_generic_and_typed_adds_no_placeholder`: typed + generic, `memory_round()`, `JsonSchema`: `tool_names == ["get_balance"]`, `synthesizing == false`.
- `native_typed_first_turn_json_schema`: only typed, no round, `JsonSchema`: `toolConfig` null, typed entry present, `outputConfig` present.
- `native_required_with_only_typed_tools_gives_notice`: no error, notices contain `NativeNotice::RequiredOmitted`, no `tool_choice` key.
- `native_tool_choice_memory_with_only_typed_tools_gives_notice`: `Tool { name: "memory" }`: no error, `NativeNotice::ToolOmitted`, no `tool_choice` key.
- `native_typed_tool_choice_unknown_name_is_error`.
- `native_none_with_json_schema_after_round_sends_no_typed_tools`: typed tool, `memory_round()`, `ToolChoice::None`, `JsonSchema`: no `additionalModelRequestFields`, `tool_names == [SYNTHESIZED_TOOL_NAME]`, `synthesizing == true` (today's anchor).
- `native_none_without_structured_after_round`: typed only, `None`: the same shape as `snap_typed_none_after_tool_round` (compare `toolConfig` and additional fields).

- [ ] **Step 2: Run the tests and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib typed_tests::native`
Expected: FAIL (the anchor is used, or the checks reject typed names).

- [ ] **Step 3: Implement** in `build_native`

- Make the `ToolSet` once. `all_tools` = `set.generic`, except in the two `None` branches (unchanged, and Task 3's call).
- `check_tool_name(&all_tools, &set, name)`.
- Anchor: `structured && all_tools.is_empty() && !sent_typed && has_tool_round(..)` (`sent_typed` is `set.has_typed()` when the request is not `ToolChoice::None`).
- Placeholder: `sent_typed && all_tools.is_empty() && has_tool_round(..)`, `synthesizing` stays `false`.
- `Required` error: only when `all_tools.is_empty() && !set.has_any() && !structured`.
- `Auto` with `sent_typed`: no `toolConfig` choice.
- `additional.tools = set.typed_entries()` when `sent_typed`.

- [ ] **Step 4: Snapshots and all tests**

Run: `INSTA_UPDATE=always cargo test -p paigasus-helikon-providers-bedrock --lib typed_tests`, read the new snapshot, then `cargo test -p paigasus-helikon-providers-bedrock`.
Expected: PASS, no modified snapshot.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/src/translate/native.rs crates/paigasus-helikon-providers-bedrock/src/translate/typed_tests.rs crates/paigasus-helikon-providers-bedrock/src/translate/snapshots/
git commit -m "feat(providers): SMA-844 send typed memory tools on the bedrock native path"
```

---

### Task 6: Fallback and stream behaviour

Tests only. They fix the behaviour that the spec states as fact.

**Files:**
- Modify: `src/fallback.rs` (tests, near line 241)
- Modify: `src/stream.rs` (tests, helpers `tool_use_start`, `tool_delta`, `message_stop`, `run` at lines 389-486)

- [ ] **Step 1: Write the tests**

- `fallback.rs`: `rejected_additional_forced_choice_retries_on_native`. Build a first `PreparedConverse` on the forced path for `us.anthropic.claude-sonnet-4-6` with a typed tool and `Tool { name: "memory" }`. Use the existing fallback test pattern, where the first send fails with `forced_choice_rejected: true`. Assert that the second send is on the native path and that the policy learns `OutputPath::Native`.
- `stream.rs`: `typed_memory_call_with_structured_native_request_finishes_with_tool_calls`. `StreamTranslator::new(false).native(true)`, events: start `tu_1`/`memory`, delta `{"command":"view","path":"/memories"}`, stop, `message_stop(ToolUse)`. Assert a `ToolCallDelta` for `memory` and a last `Finish { reason: ToolCalls }`, with no `Err`.
- `stream.rs`: `placeholder_call_without_synthesis_is_a_normal_tool_call`. `StreamTranslator::new(false)`, a call named `SYNTHESIZED_TOOL_NAME` with input `{}`. Assert a `ToolCallDelta` with that name and `Finish { reason: ToolCalls }`.

- [ ] **Step 2: Run the tests**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib fallback`, then the same command with `stream`
Expected: PASS. If the fallback test fails, `has_forced_tool_choice` in Task 1 is wrong: fix it there.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/src/fallback.rs crates/paigasus-helikon-providers-bedrock/src/stream.rs
git commit -m "test(providers): SMA-844 pin the bedrock fallback and stream for typed tools"
```

---

### Task 7: Live tests

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/tests/live.rs` (module doc at lines 1-32, new tests at the end)

**Interfaces:**
- Consumes: `BedrockModel::from_env`, `ProviderToolType` from `paigasus_helikon_core`.

- [ ] **Step 1: Write the five `#[tokio::test] #[ignore]` tests** (spec section 7)

Each test returns early on `skip_if_missing_env()`, and returns early with an `eprintln!` when `ModelFamily::from_model_id` is not `Anthropic`. A helper `memory_tool()` declares `ProviderToolType::new("anthropic", "memory_20250818")`, with the description "Memory directory tool." and a schema `{"type":"object","properties":{"command":{"type":"string"},"path":{"type":"string"}},"required":["command"]}`. A helper `memory_round()` gives the turn-2 history of spec section 3: the user message, a `ToolCall` for `memory` with `{"command":"view","path":"/memories"}`, and its result "Directory: /memories\n(empty)". A helper `collect(stream)` returns the text, the tool-call names and the finish reason, and panics on an `Err`.

1. `live_typed_memory_two_turns`: turn 1 `Auto` with the spec message: the tool-call names contain `memory`. Turn 2 with `memory_round()`: no error, text is not empty.
2. `live_typed_memory_forced_choice`: `ToolChoice::Tool { name: "memory" }`: the tool-call names contain `memory`.
3. `live_typed_memory_forced_structured_output`: a `JsonSchema` `{capital: string}` with the memory tool, on the first turn: no error.
4. `live_typed_memory_native_structured_after_round`: `memory_round()` + `JsonSchema` `{capital: string}`: no error, the text parses as JSON with a `capital` key.
5. `live_typed_memory_none_after_round`: `memory_round()` + `ToolChoice::None`: no error, text is not empty, no tool call.

Add a module-doc section "Native memory tool (SMA-844)" with the two run commands below.

- [ ] **Step 2: Compile the tests**

Run: `cargo test -p paigasus-helikon-providers-bedrock --test live --no-run`
Expected: build OK.

- [ ] **Step 3: Run the tests live**

The AWS SSO session may be expired. If a test fails with an SSO or credential error, stop and ask the user to run `aws sso login --profile maip-dev-admin`.

```bash
AWS_PROFILE=maip-dev-admin AWS_DEFAULT_REGION=us-east-1 BEDROCK_MODEL_ID=us.anthropic.claude-sonnet-4-6 \
  cargo test -p paigasus-helikon-providers-bedrock --test live live_typed_memory -- --ignored --nocapture
AWS_PROFILE=maip-dev-admin AWS_DEFAULT_REGION=us-east-1 BEDROCK_MODEL_ID=us.anthropic.claude-sonnet-5-5 \
  cargo test -p paigasus-helikon-providers-bedrock --test live live_typed_memory -- --ignored --nocapture
```

Expected: on Sonnet 4.6, tests 1, 2, 3 and 5 pass. On Sonnet 5.5, tests 1, 4 and 5 pass, and test 2 passes through the SMA-839 fallback or fails only because the model does not call `memory`. Test 4 on Sonnet 4.6 and test 3 on Sonnet 5.5 are extra runs. Record each test's result per model in `docs/superpowers/plans/2026-11-22-sma-844-live-results.md` (date, model, test, PASS/FAIL, one-line note). The PR body uses this file.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/tests/live.rs docs/superpowers/plans/2026-11-22-sma-844-live-results.md
git commit -m "test(providers): SMA-844 add live bedrock tests for the native memory tool"
```

---

### Task 8: Documentation and the provider id drift test

**Files:**
- Create: `crates/paigasus-helikon/tests/provider_tool_type_ids.rs`
- Modify: `crates/paigasus-helikon-providers-bedrock/README.md` (new section after "`ToolChoice::None` after a tool round", line 128)
- Modify: `docs/book/src/concepts/model-providers.md:324`
- Modify: `docs/book/src/concepts/tools.md:139`

- [ ] **Step 1: Write the drift test**

```rust
//! The Bedrock provider matches the provider id "anthropic" (SMA-844).
#![cfg(feature = "anthropic")]

#[test]
fn anthropic_provider_id_is_the_value_that_bedrock_matches() {
    assert_eq!(paigasus_helikon::anthropic::PROVIDER_ID, "anthropic");
}
```

Together with Task 2's `provider_id_matches_the_anthropic_provider`, this pins both constants to the same value.

Run: `cargo test -p paigasus-helikon --all-features --test provider_tool_type_ids`
Expected: PASS.

- [ ] **Step 2: Write the docs** (STE, from spec section 8)

- README "Provider tool types": the typed memory path for Claude models, the `memory_*` rule, `native_tool_types(false)`, the placeholder (and that `Required` after a tool round with only typed tools also allows a call to the placeholder, which the agent answers as an unknown tool), and that other types get the generic schema. Include one short code example of `.native_tool_types(false)` fenced as `rust,ignore`.
- `model-providers.md:324`: replace "Bedrock does not use provider tool types. It sends the generic schema for every tool." with two or three sentences on the typed memory path and the opt-out. Keep the link to `./tools.md#provider-tool-types`.
- `tools.md:139`: replace "Anthropic is the only provider that uses this declaration now." and the list, so that it says that Anthropic, and Bedrock for Claude models and `memory_*` types, use the declaration. OpenAI, Gemini and LiteLLM send the generic schema.

- [ ] **Step 3: Check the docs**

Run: `npx markdownlint-cli2` and `mdbook build docs/book`.
Expected: 0 issues, and the build is clean.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon/tests/provider_tool_type_ids.rs crates/paigasus-helikon-providers-bedrock/README.md docs/book/src/concepts/model-providers.md docs/book/src/concepts/tools.md
git commit -m "docs(providers): SMA-844 document the native memory tool on bedrock"
# `book` is not an allowed scope in .versionrc; `providers` and `docs` are.
```

---

## Final check (after Task 8)

Run every gate from `CLAUDE.md` → "Common commands": `fmt`, `clippy --workspace --all-features --all-targets -- -D warnings`, `test --workspace --all-features`, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`, the doc-coverage script, `npx markdownlint-cli2`, and `bash scripts/check-breaking-cascade.sh`. Expected: all green, and no existing snapshot file is modified (`git diff --stat main -- '*.snap'` lists only new files).
