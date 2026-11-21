# SMA-830 Provider Tool Type Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `Tool` can declare a provider tool type, and the Anthropic provider
sends `{"type": "memory_20250818", "name": "memory"}` for it; all other
providers send the generic schema.

**Architecture:** Core gets `ProviderToolType`, a defaulted
`Tool::provider_types()`, and a `provider_types` field on `ToolDef`, which
becomes `#[non_exhaustive]` with constructors. The Anthropic tools translator
picks the first `anthropic` entry, unless the builder opt-out is set. Other
providers do not read the field.

**Tech Stack:** Rust 1.94, serde / serde_json, wiremock (Anthropic request
tests), release-plz v0.4.3 (dry run only).

**Spec:** `docs/superpowers/specs/2026-11-20-sma-830-provider-tool-type-design.md`

## Global Constraints

- Worktree root: `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-830`. All paths below are relative to it. Write/Edit must use the absolute worktree path.
- Do not run git commands that change HEAD or the branch (`checkout`, `switch`, `reset`, `rebase`, `stash`). Only `git add <explicit paths>` and `git commit`. Never `git add -A` (`.env` is not ignored).
- Work in the foreground. Do not put cargo runs in the background. Do not stop before the command finishes.
- No version changes in any `Cargo.toml`. No hand bump.
- Every new public item has a `///` comment (`missing_docs` is `-D warnings` in CI). A `///` on a pub item must not intra-doc-link a private item.
- Commit messages: `<type>(<scope>): SMA-830 <lowercase subject>`. Allowed scopes include `core`, `providers`, `runtime`, `book`, `spec`, `plan`. Each commit ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Before each commit: `cargo fmt --all`, then `cargo clippy -p <crates touched> --all-features --all-targets -- -D warnings`.
- `ToolDef` with empty `provider_types` must serialize to the exact JSON of today (no `provider_types` key).
- `ProviderToolType` serializes its type string under the key `"type"`.
- Anthropic provider id: `"anthropic"`. `params` key `cache_control` is removed; keys `type` and `name` are set last.

## Review Focus

1. A `ToolDef` that declares only a non-`anthropic` type (for example `"openai"`) must give the Anthropic generic entry, not a typed one. → Task 3 test `non_anthropic_entry_gives_generic_entry`.
2. A provider id with wrong case (`"Anthropic"`) silently gives the generic entry. Expected: generic entry (documented), no panic. → Task 3 test `provider_id_match_is_case_sensitive`.
3. Two `anthropic` entries: the first wins and the second is ignored. → Task 3 test `first_anthropic_entry_wins`.
4. A Temporal activity input written before this change (no `provider_types` key) must decode. → Task 2 frozen-literal test.
5. Handoff synthetic tools next to a typed tool: the handoff tools keep the generic entry and the typed entry stays typed. → Task 3 test `typed_entry_and_generic_entries_mix`.

---

### Task 1: Core types and `ToolDef` constructors

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs` (`ToolDef` at line ~121; add `ProviderToolType` next to it)
- Modify: `crates/paigasus-helikon-core/src/tool.rs` (`Tool` trait, line ~65-94)
- Modify: `crates/paigasus-helikon-core/src/agent.rs:907-914` (use `ToolDef::from_tool`)
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs:747-757` (use `ToolDef::new`)
- Test: `crates/paigasus-helikon-core/tests/wire_serde.rs`

**Interfaces:**
- Produces:
  - `pub struct ProviderToolType { pub provider: String, #[serde(rename = "type")] pub type_name: String, #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")] pub params: serde_json::Map<String, serde_json::Value> }` — derives `Debug, Clone, PartialEq, Serialize, Deserialize`, `#[non_exhaustive]`.
  - `ProviderToolType::new(provider: impl Into<String>, type_name: impl Into<String>) -> Self` (empty params).
  - `ProviderToolType::with_params(self, params: serde_json::Map<String, serde_json::Value>) -> Self`.
  - `Tool::provider_types(&self) -> &[ProviderToolType]`, default `&[]`. The `///` says: declare at most one entry per provider; a wrapper tool must forward this method.
  - `ToolDef` gains `pub provider_types: Vec<ProviderToolType>` with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`; derives add `PartialEq`; add `#[non_exhaustive]`.
  - `ToolDef::new(name: impl Into<String>, description: impl Into<String>, schema: serde_json::Value) -> Self`.
  - `ToolDef::with_provider_types(self, provider_types: Vec<ProviderToolType>) -> Self`.
  - `ToolDef::from_tool<Ctx, T>(tool: &T) -> Self where Ctx: Send + Sync + 'static, T: Tool<Ctx> + ?Sized` — copies name, description, schema, provider_types.

- [ ] **Step 1: Write the failing tests** in `tests/wire_serde.rs`. Change the existing `ToolDef { .. }` literal in `model_request_round_trips` to `ToolDef::new("echo", "d", json!({"type":"object"}))`. Add:

```rust
#[test]
fn tool_def_without_provider_types_serializes_as_before() {
    let def = ToolDef::new("echo", "d", json!({"type": "object"}));
    assert_eq!(
        serde_json::to_value(&def).unwrap(),
        json!({"name": "echo", "description": "d", "schema": {"type": "object"}})
    );
}

#[test]
fn old_tool_def_json_deserializes_with_empty_provider_types() {
    let def: ToolDef = serde_json::from_value(
        json!({"name": "echo", "description": "d", "schema": {"type": "object"}})
    ).unwrap();
    assert!(def.provider_types.is_empty());
}

#[test]
fn provider_tool_type_round_trips_with_type_key() {
    let mut params = serde_json::Map::new();
    params.insert("max_characters".into(), json!(10000));
    let def = ToolDef::new("memory", "d", json!({"type": "object"}))
        .with_provider_types(vec![
            ProviderToolType::new("anthropic", "memory_20250818"),
            ProviderToolType::new("anthropic", "text_editor_20250728").with_params(params),
        ]);
    let v = serde_json::to_value(&def).unwrap();
    assert_eq!(v["provider_types"][0], json!({"provider": "anthropic", "type": "memory_20250818"}));
    assert_eq!(v["provider_types"][1]["max_characters"], serde_json::Value::Null); // params nest under "params"
    assert_eq!(v["provider_types"][1]["params"]["max_characters"], json!(10000));
    let back: ToolDef = serde_json::from_value(v).unwrap();
    assert_eq!(back, def);
}
```

Add a unit test in `tool.rs`'s `#[cfg(test)]` module (it has `OneTool` at line ~408): `from_tool_copies_all_fields` — a test tool that overrides `provider_types` to return one `ProviderToolType::new("anthropic", "memory_20250818")`; assert `ToolDef::from_tool(&tool) == ToolDef::new(<name>, <description>, <schema>).with_provider_types(vec![that entry])`. Also call it through `&dyn Tool<()>` / `Arc<dyn Tool<Ctx>>` deref to prove `?Sized` works. A second test: a tool with no override gives empty `provider_types`.

- [ ] **Step 2: Run to verify failure:** `cargo test -p paigasus-helikon-core --test wire_serde` → compile error (`ToolDef::new`, `ProviderToolType` not found).

- [ ] **Step 3: Implement** the Interfaces above. Replace the mapping at `agent.rs:907-914` with `.map(|t| crate::ToolDef::from_tool(t.as_ref()))` (or equivalent), and `handoff_tool_def` with `ToolDef::new(...)`. `ProviderToolType` is exported through the existing `pub use model::*` in `src/lib.rs`; check it is reachable as `paigasus_helikon_core::ProviderToolType`.

- [ ] **Step 4: Run:** `cargo test -p paigasus-helikon-core` → all pass. Other workspace crates do not compile yet (Task 2 fixes them); that is expected.

- [ ] **Step 5: Commit:** `feat(core): SMA-830 let a tool declare provider tool types`.

### Task 2: Move every other `ToolDef` literal to the constructor; Temporal tests

**Files (49 literal sites in total; Task 1 did core's 3):**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/{mod.rs,response_format.rs,tools.rs}`, `tests/{live.rs,prompt_caching.rs,structured_output.rs}`
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/{mod.rs,tools.rs}`, `tests/live.rs`
- Modify: `crates/paigasus-helikon-providers-gemini/src/translate/{mod.rs,response_format.rs,tools.rs}`, `tests/live.rs`
- Modify: `crates/paigasus-helikon-providers-litellm/src/translate/tools.rs`
- Modify: `crates/paigasus-helikon-providers-openai/tests/live.rs`
- Modify: `tests/provider-stream-conformance/tests/conformance.rs`
- Modify: `crates/paigasus-helikon-runtime-temporal/src/worker.rs:396-404` (use `ToolDef::from_tool`), test at `worker.rs:767`
- Test: `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs` (next to `call_model_decodes_frozen_envelope_literal`, line ~777)

**Interfaces:**
- Consumes: `ToolDef::new`, `ToolDef::with_provider_types`, `ToolDef::from_tool`, `ProviderToolType::new` (Task 1).

- [ ] **Step 1: Find every site:** `grep -rn "ToolDef {" crates tests --include='*.rs'` (use `grep -rn "ToolDef {" crates tests | grep '\.rs:'` on zsh). Expected after this task: zero hits outside `model.rs`'s own definition.
- [ ] **Step 2: Replace each literal** with `ToolDef::new(name, description, schema)`. Behaviour must not change.
- [ ] **Step 3: Write the Temporal tests.**
  - In `activity_input.rs`: `call_model_decodes_tool_defs_old_and_new_shape` — a frozen JSON string literal of a `CallModelArgs` with `"tools":[{"name":"a","description":"d","schema":{"type":"object"}},{"name":"memory","description":"d","schema":{"type":"object"},"provider_types":[{"provider":"anthropic","type":"memory_20250818"}]}]`. Decode it the same way the existing frozen test does. Assert `tools[0].provider_types.is_empty()` and `tools[1].provider_types == vec![ProviderToolType::new("anthropic", "memory_20250818")]`.
  - In `worker.rs:767` `register_accepts_plain_agent_and_snapshots_tool_defs`: give the test tool a `provider_types` override (or add a second tool that has one), and assert the snapshotted `ToolDef` carries it.
- [ ] **Step 4: Run:** `cargo build --workspace --all-features --all-targets` → OK. `cargo test -p paigasus-helikon-runtime-temporal` → pass. `cargo test --workspace --all-features` → pass (record any pre-existing failure that also fails on `main`, for example trybuild on a newer local rustc, and do not fix it).
- [ ] **Step 5: Commit:** `refactor(core): SMA-830 build tool defs with the constructor across the workspace`. (Use type `refactor` or `test` as convco allows; the PR title carries the `!`.)

### Task 3: Anthropic typed entry, opt-out and provider id

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/tools.rs`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/mod.rs:46` (pass the opt-out)
- Modify: `crates/paigasus-helikon-providers-anthropic/src/builder.rs` (`Config` and `AnthropicModelBuilder`)
- Modify: `crates/paigasus-helikon-providers-anthropic/src/model.rs:168-170`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/lib.rs` (export `PROVIDER_ID`)
- Test: unit tests in `translate/tools.rs` and `translate/mod.rs`; `tests/messages_wire.rs`; `tests/live.rs`
- Modify: `crates/paigasus-helikon-providers-anthropic/README.md`

**Interfaces:**
- Consumes: `ToolDef`, `ProviderToolType` (Task 1).
- Produces:
  - `pub const PROVIDER_ID: &str = "anthropic";` at crate root, with `///` that names its use in `ProviderToolType::new(PROVIDER_ID, "memory_20250818")`.
  - `AnthropicModelBuilder::native_tool_types(self, enabled: bool) -> Self`, default `true`; `Config` gets `pub(crate) native_tool_types: bool`.
  - `pub(crate) fn translate_tools(defs: &[ToolDef], native_tool_types: bool) -> Value`.
  - `AnthropicModel::provider()` returns `PROVIDER_ID`.

- [ ] **Step 1: Write the failing unit tests** in `translate/tools.rs` (helper `memory_def()` = `ToolDef::new("memory", "custom description", json!({"type":"object","properties":{"command":{"type":"string"}}})).with_provider_types(vec![ProviderToolType::new("anthropic","memory_20250818")])`):
  - `memory_type_gives_exact_typed_entry`: `translate_tools(&[memory_def()], true)[0] == json!({"type": "memory_20250818", "name": "memory"})`.
  - `params_are_merged`: params `{"max_characters": 10000}` → entry has `"max_characters": 10000`, `type`, `name`.
  - `type_and_name_in_params_do_not_override`: params `{"type":"x","name":"y"}` → entry `type == "memory_20250818"`, `name == "memory"`.
  - `cache_control_in_params_is_removed`: params `{"cache_control":{"type":"ephemeral"}}` → entry has no `cache_control` key.
  - `non_anthropic_entry_gives_generic_entry`: only `ProviderToolType::new("openai", "x")` → entry equals the generic `{"name","description","input_schema"}` entry.
  - `provider_id_match_is_case_sensitive`: `"Anthropic"` → generic entry.
  - `first_anthropic_entry_wins`: entries `memory_20250818` then `bash_20250124` → `type == "memory_20250818"`.
  - `opt_out_gives_generic_entry`: `translate_tools(&[memory_def()], false)[0]` has `input_schema` and `description`, no `type`.
  - `typed_entry_and_generic_entries_mix`: `[generic_def, memory_def()]` → `[0]` has `input_schema`, `[1]` is the typed entry.
  - Update the existing `passes_through_name_description_and_schema` to the new signature.
- [ ] **Step 2: Write the failing build-body tests** in `translate/mod.rs`'s test module (call `build_body` like the existing tests around line 370):
  - `cache_marker_lands_on_typed_entry_when_last`: `CacheStrategy::Tools`, tools `[memory_def()]`, no `response_format` → `tools[0] == json!({"type":"memory_20250818","name":"memory","cache_control":{"type":"ephemeral"}})`.
  - `structured_output_tool_takes_marker_and_typed_entry_unchanged`: `CacheStrategy::Tools` plus `ResponseFormat::JsonSchema` → the last tool is `__paigasus_structured_output__` with `cache_control`, and the typed entry has no `cache_control`.
  - `builder_opt_out_reaches_body`: a `Config` with `native_tool_types: false` → the memory tool has `input_schema`.
- [ ] **Step 3: Write the failing model and wire tests.**
  - In `src/model.rs` tests (`provider_and_model_getters`, line ~201): assert `m.provider() == PROVIDER_ID`.
  - In `tests/messages_wire.rs`: `typed_tool_entry_is_sent_in_request_body` — wiremock server, a request with `memory_def()`, assert the captured body's `tools[0] == json!({"type":"memory_20250818","name":"memory"})` and no `anthropic-beta` header is present.
  - In `tests/live.rs`: `#[ignore]` `memory_typed_tool_smoke` in the same style as `cache_strategy_round_trip` (`skip_if_no_key()`), with `CacheStrategy::Tools` and `memory_def()`; assert the stream ends without a `ModelError`.
- [ ] **Step 4: Run to verify failure:** `cargo test -p paigasus-helikon-providers-anthropic` → compile errors / failures.
- [ ] **Step 5: Implement** the Interfaces. In `translate_tools`, for each def: if `native_tool_types` and the first entry with `provider == PROVIDER_ID` exists, start from `params.clone()`, remove `cache_control`, insert `type` and `name` last, and `tracing::debug!(target: "paigasus::anthropic::translate", tool = %def.name, r#type = %entry.type_name, "typed tool entry")`; else the generic entry.
- [ ] **Step 6: Run:** `cargo test -p paigasus-helikon-providers-anthropic --all-features` → pass. If `ANTHROPIC_API_KEY` is set in the environment, also run `cargo test -p paigasus-helikon-providers-anthropic --test live memory_typed_tool_smoke -- --ignored` and record the result; if not set, record "not run, no key".
- [ ] **Step 7: README:** add a "Native tool types" section to `crates/paigasus-helikon-providers-anthropic/README.md`: what the typed entry is, a `Tool::provider_types` example with `ProviderToolType::new(PROVIDER_ID, "memory_20250818")`, that no description or schema is sent, that `native_tool_types(false)` is the opt-out, that a wrapper tool must forward `provider_types`. Use a ` ```rust,ignore ` fence or plain prose; this README is not a doctest, but keep the snippet correct.
- [ ] **Step 8: Commit:** `feat(providers): SMA-830 send the anthropic typed tool entry for a declared provider type`.

### Task 4: Fallback tests in every other provider

**Files:**
- Test: `crates/paigasus-helikon-providers-bedrock/src/translate/tools.rs` (`tool_specs`)
- Test: `crates/paigasus-helikon-providers-gemini/src/translate/tools.rs` (`function_declarations`)
- Test: `crates/paigasus-helikon-providers-litellm/src/translate/tools.rs` (`to_tools`)
- Test: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` and `src/backend/responses.rs` (`build_request`, test modules at line ~879 and ~798)

**Interfaces:**
- Consumes: `ToolDef::new`, `with_provider_types`, `ProviderToolType::new` (Task 1).

- [ ] **Step 1: Write one test per translator**, each named `provider_type_is_ignored`: build `plain = ToolDef::new("memory", "d", schema)` and `typed = plain.clone().with_provider_types(vec![ProviderToolType::new("anthropic", "memory_20250818")])`; assert the translator output for `[typed]` equals the output for `[plain]`. For Bedrock, compare with `format!("{:?}", ..)` if the SDK type has no `PartialEq`. For OpenAI Chat and Responses, build a `ModelRequest` with each and compare the serialized request (`serde_json::to_value` of the built request, or the tools part of it) as the existing tests in that module do.
- [ ] **Step 2: Run:** `cargo test -p paigasus-helikon-providers-bedrock -p paigasus-helikon-providers-gemini -p paigasus-helikon-providers-litellm -p paigasus-helikon-providers-openai --all-features` → pass. (These tests pass at once: the providers already ignore the field. That is the behaviour to pin.) Mutation check: temporarily make one translator read `provider_types` (for example put `type_name` into the description) and confirm its test fails; revert.
- [ ] **Step 3: Commit:** `test(providers): SMA-830 pin the generic schema fallback in every other provider`.

### Task 5: Book and CHANGELOG upgrade notes

**Files:**
- Modify: `docs/book/src/concepts/tools.md` (trait listing at line ~20-30; new section "Provider tool types")
- Modify: `docs/book/src/concepts/model-providers.md` (one line per provider on the declaration)
- Modify: `crates/paigasus-helikon-core/README.md` only if it lists the `Tool` trait methods
- Modify (`## [Unreleased]` section of each CHANGELOG.md; create the heading under the title if absent, in the release-plz format): core, facade (`crates/paigasus-helikon/`), and the 17 library dependents: evals, runtime-tokio, mcp, providers-anthropic, providers-bedrock, providers-gemini, providers-litellm, providers-openai, runtime-actix, runtime-agentcore, runtime-axum, runtime-temporal, sessions-postgres, sessions-testkit, sessions-redis, sessions-sqlite, tools. Not the CLI.

- [ ] **Step 1: Book.** `tools.md`: add `fn provider_types(&self) -> &[ProviderToolType] { &[] }` to the trait listing; a section with the memory example, the rule "at most one entry per provider", the wrapper-forwarding rule, and which providers use it. `model-providers.md`: Anthropic uses it (with `native_tool_types` opt-out); OpenAI, Gemini, Bedrock, LiteLLM send the generic schema.
- [ ] **Step 2: CHANGELOG notes.** Core and facade: the four bullets from the spec section "CHANGELOG upgrade notes" (constructor; upgrade together and check `cargo tree -d`; wrapper forwards `provider_types`; JSON of a `ToolDef` without provider types does not change). Each dependent: "Requires `paigasus-helikon-core` 0.8 (`ToolDef` is now `#[non_exhaustive]`; see the core CHANGELOG)." Anthropic adds the typed-entry and `native_tool_types` note. Runtime-temporal adds: "A `call_model` activity input now can carry `provider_types`. A worker older than this version decodes it but drops the field, so that turn sends the generic tool entry."
- [ ] **Step 3: Verify:** `mdbook build docs/book` → clean. `npx markdownlint-cli2` → clean. `git diff --stat` shows a CHANGELOG change in all 19 crate directories listed above.
- [ ] **Step 4: Commit:** `docs(book): SMA-830 document provider tool types and the core 0.8 upgrade`.

### Task 6 (coordinator, Opus, before the PR): full gates and release dry run

- [ ] **Step 1:** Confirm SMA-838 PR 1 has published (crates.io shows the republished versions, for example anthropic 0.2.0). If not, stop and tell Sven. Then rebase the branch on `origin/main` (coordinator only).
- [ ] **Step 2:** Run every CI gate in `CLAUDE.md` (fmt, clippy, test, doc with `-D warnings`, doc-coverage, markdownlint, the three guard scripts, and SMA-838's `scripts/check-breaking-cascade.sh` if present on `main`).
- [ ] **Step 3:** In a scratch clone, squash the branch into one commit titled `feat(core)!: SMA-830 let a tool declare a provider tool type`, and run release-plz **v0.4.3** `release-plz update`. Record the version table. Expected: core and facade a minor bump, every listed library dependent a minor bump (0.x), CLI a patch. Any patch for a library dependent is a failure: find the crate whose directory the commit did not touch.
