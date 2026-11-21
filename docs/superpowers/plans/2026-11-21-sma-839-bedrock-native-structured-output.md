# SMA-839 Bedrock Native Structured Output Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Claude Sonnet 5.5 and Opus 5.5 on Bedrock never get a forced `toolChoice`, and `ResponseFormat::JsonSchema` works on them through `outputConfig.textFormat`.

**Architecture:** A per-handle `ForcedChoicePolicy` selects an `OutputPath` (`ForcedTool` or `Native`) from a static model rule (rule A) and a runtime flag (rule B). `build_request` takes the path; the `ForcedTool` branch stays byte-identical and a new `build_native` builds the native request. `invoke` sends through `send_with_fallback`, which on the exact forced-choice rejection learns the flag and sends one native request before the stream yields any event.

**Tech Stack:** Rust 1.94, `aws-sdk-bedrockruntime` (`OutputConfig`, `OutputFormat`, `OutputFormatType`, `OutputFormatStructure`, `JsonSchemaDefinition`), `serde_json`, `tokio`, `insta` snapshots, `tracing`.

**Spec:** `docs/superpowers/specs/2026-11-21-sma-839-bedrock-native-structured-output-design.md`

**Execution:** subagent-driven.

## Global Constraints

- Work only in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-839`. Start every shell command with `cd /Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-839 || exit 1`. Use absolute paths with Write/Edit.
- Do not run git commands that move HEAD or change branches (`checkout`, `switch`, `reset`, `rebase`, `stash`, `merge`, `pull`). Work synchronously in the foreground; do not end a task until its commands have a terminal status.
- One crate changes: `crates/paigasus-helikon-providers-bedrock`. No change to `paigasus-helikon-core`. No new dependency and no new dev-dependency.
- Do not bump any version by hand and do not edit `CHANGELOG.md`. release-plz bumps the crate and the facade.
- The `ForcedTool` request must not change for models that work today: all nine existing files in `src/translate/snapshots/` stay byte-identical.
- A forced `toolChoice` (`any` or `tool`) never goes to a model where `rejects_forced_tool_choice` is `true`.
- The fallback starts only on the two exact texts `type "tool" and "any" are not supported` and `doesn't support the toolConfig.toolChoice.` with code `ValidationException`. It runs at most one time for each request.
- New items are `pub(crate)`. A `///` doc on a `pub` item must not intra-doc-link to a `pub(crate)` item (the `-D warnings` docs gate fails). Use prose.
- Each task runs `cargo fmt --all` and `cargo clippy -p paigasus-helikon-providers-bedrock --all-features --all-targets -- -D warnings` before its commit.
- Commits: `git add <explicit paths>` only (never `-a`/`-A`; `.env` and `.claude/` are not ignored). Allowed scopes come from `.versionrc`: use `fix(providers)`, `test(providers)`, `docs(providers)` and `docs(docs)`. `docs(book)` is **not** allowed. Every message ends with the trailer line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Docs prose (README, mdBook, doc comments) uses ASD-STE100 Simplified Technical English: short sentences, active voice, no idiom.

## Review Focus

1. A property named `minimum`, `not` or `if`, or a `default`/`enum`/`const`/`examples` value that contains such keys, must survive `native_output_schema` unchanged. Task 3 test `native_schema_keeps_property_names_and_values_that_match_keywords`.
2. A node with both `oneOf` and `anyOf` must keep both constraints after the rename. Task 3 test `native_schema_one_of_next_to_any_of_keeps_both`.
3. After a fallback, the stream translator must use the `synthesizing` value of the request that was sent (forced: `true`, native: often `false`), not of the first request. Task 5 test `fallback_returns_synthesizing_of_the_sent_request`.
4. Mixed-case IDs, ARNs and dated IDs of Sonnet 5.5 / Opus 5.5 select the native path; a non-Anthropic ID that contains `claude-sonnet-5-5` does not. Task 1 test `rejects_forced_tool_choice_matches_only_sonnet_and_opus_5_5`.
5. A transient error (throttling, 5xx) on the first send must go to the caller unchanged and must not set the learned flag (ADR-10). Task 5 test `other_errors_do_not_fall_back`.

---

### Task 1: Rule A — `rejects_forced_tool_choice`

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/family.rs:50-69` (extract the resource-ID logic), add the function after `supports_forced_tool_choice` (line 96)
- Test: same file, `mod tests`

**Interfaces:**
- Produces: `pub(crate) fn rejects_forced_tool_choice(model_id: &str) -> bool` (free function in `family.rs`), and private `fn resource_id(lowered: &str) -> Option<&str>` used by both `from_model_id` and the new function.

- [ ] **Step 1: Write the failing test** in `family.rs` `mod tests`:

```rust
#[test]
fn rejects_forced_tool_choice_matches_only_sonnet_and_opus_5_5() {
    for id in [
        "anthropic.claude-sonnet-5-5",
        "anthropic.claude-opus-5-5",
        "us.anthropic.claude-sonnet-5-5",
        "global.anthropic.claude-sonnet-5-5",
        "eu.anthropic.claude-opus-5-5",
        "us.anthropic.claude-opus-5-5-20261101-v1:0",
        "GLOBAL.Anthropic.Claude-Sonnet-5-5",
        "arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-sonnet-5-5",
        "arn:aws:bedrock:us-east-1:123456789012:inference-profile/global.anthropic.claude-opus-5-5",
    ] {
        assert!(rejects_forced_tool_choice(id), "id={id}");
    }
    for id in [
        "global.anthropic.claude-sonnet-5",
        "us.anthropic.claude-opus-5",
        "global.anthropic.claude-haiku-5-5",
        "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "amazon.nova-pro-v1:0",
        "mistral.mistral-large-2407-v1:0",
        "meta.claude-sonnet-5-5",
        "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/a1b2c3d4e5f6",
        "",
    ] {
        assert!(!rejects_forced_tool_choice(id), "id={id}");
    }
}
```

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib family::tests`
Expected: compile error, `cannot find function rejects_forced_tool_choice`.

- [ ] **Step 3: Implement**

Extract `fn resource_id(lowered: &str) -> Option<&str>`: for an `arn:` ID return the text after the last `/` (`None` if there is no `/`); else return the ID. `from_model_id` uses it (`None` → `Unknown`); its behavior does not change. `rejects_forced_tool_choice` returns `true` only when `ModelFamily::from_model_id(model_id) == Anthropic` and the lowercased resource ID contains `claude-sonnet-5-5` or `claude-opus-5-5`. Doc comment: the models in this list reject a forced `toolChoice`; a stale list costs one rejected request, because rule B recovers. Add one sentence to the `supports_forced_tool_choice` doc: some Anthropic models reject it; see `rejects_forced_tool_choice`.

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib family::tests`
Expected: PASS, including the unchanged `detects_family_behind_any_leading_segment`.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-bedrock --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-bedrock/src/family.rs
git commit -m "$(cat <<'EOF'
fix(providers): SMA-839 add the static rule for models that reject a forced tool choice

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: `OutputPath`, `ForcedChoicePolicy` and the handle layout

**Files:**
- Create: `crates/paigasus-helikon-providers-bedrock/src/policy.rs`
- Modify: `crates/paigasus-helikon-providers-bedrock/src/lib.rs` (add `mod policy;`)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/model.rs:29` (handle struct), `:52`, `:132`, `:140` (`self.0` → `self.config`)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/builder.rs:208-214` (construct the policy), `:393`, `:402` (`model.0` → `model.config`)

**Interfaces:**
- Consumes: `rejects_forced_tool_choice(&str) -> bool` (Task 1), `ModelFamily`.
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub(crate) enum OutputPath { ForcedTool, Native }`
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub(crate) enum NativeNotice { RequiredOmitted, ToolOmitted, JsonObjectAsText }`
  - `#[derive(Debug)] pub(crate) struct ForcedChoicePolicy { rejects_static: bool, learned: AtomicBool, family: ModelFamily, warned: [AtomicBool; 3] }`
  - `ForcedChoicePolicy::new(model_id: &str, family: ModelFamily) -> Self` (`rejects_static = rejects_forced_tool_choice(model_id)`)
  - `#[cfg(test)] ForcedChoicePolicy::without_static_rule(family: ModelFamily) -> Self` (`rejects_static = false`)
  - `fn family(&self) -> ModelFamily`
  - `fn path(&self) -> OutputPath` — `Native` if `rejects_static || learned`, else `ForcedTool`
  - `fn record_rejection(&self) -> bool` — `!learned.swap(true, SeqCst)`
  - `fn first_notice(&self, n: NativeNotice) -> bool` — `!warned[n as usize].swap(true, SeqCst)`
  - `fn note(&self, n: NativeNotice, model_id: &str)` — one `tracing::warn!` (target `paigasus::bedrock::translate`, field `model_id`) when `first_notice` is `true`. Texts: Required: "ToolChoice::Required is omitted: this model rejects a forced tool choice"; Tool: "ToolChoice::Tool is omitted: this model rejects a forced tool choice"; JsonObject: "ResponseFormat::JsonObject degrades to text: native output cannot express a free-form object".
  - `pub struct BedrockModel { pub(crate) config: Arc<Config>, pub(crate) policy: Arc<ForcedChoicePolicy> }` (`#[derive(Debug, Clone)]` stays). `Config` does not change.

- [ ] **Step 1: Write the failing tests** in `policy.rs` `mod tests`:

```rust
#[test]
fn path_for_each_combination() {
    assert_eq!(ForcedChoicePolicy::new("global.anthropic.claude-sonnet-5-5", Anthropic).path(), OutputPath::Native);
    assert_eq!(ForcedChoicePolicy::new("global.anthropic.claude-sonnet-5", Anthropic).path(), OutputPath::ForcedTool);
    let p = ForcedChoicePolicy::without_static_rule(Anthropic);
    assert_eq!(p.path(), OutputPath::ForcedTool);
    assert!(p.record_rejection());
    assert_eq!(p.path(), OutputPath::Native);
    let both = ForcedChoicePolicy::new("us.anthropic.claude-opus-5-5", Anthropic);
    both.record_rejection();
    assert_eq!(both.path(), OutputPath::Native);
}

#[test]
fn record_rejection_is_true_only_once_across_threads() {
    let p = std::sync::Arc::new(ForcedChoicePolicy::without_static_rule(Anthropic));
    let firsts: usize = (0..8)
        .map(|_| { let p = p.clone(); std::thread::spawn(move || p.record_rejection()) })
        .collect::<Vec<_>>().into_iter()
        .map(|h| h.join().unwrap() as usize).sum();
    assert_eq!(firsts, 1);
}

#[test]
fn first_notice_is_true_once_per_kind() {
    let p = ForcedChoicePolicy::without_static_rule(Anthropic);
    assert!(p.first_notice(NativeNotice::RequiredOmitted));
    assert!(!p.first_notice(NativeNotice::RequiredOmitted));
    assert!(p.first_notice(NativeNotice::ToolOmitted));
    assert!(p.first_notice(NativeNotice::JsonObjectAsText));
}
```

In `model.rs` `mod tests`, add:

```rust
#[test]
fn clones_share_the_policy() {
    let m = offline_model("anthropic.claude-3-5-sonnet-20241022-v2:0");
    let m2 = m.clone();
    assert!(m.policy.record_rejection());
    assert_eq!(m2.policy.path(), crate::policy::OutputPath::Native);
    let fresh = offline_model("anthropic.claude-3-5-sonnet-20241022-v2:0");
    assert_eq!(fresh.policy.path(), crate::policy::OutputPath::ForcedTool);
}
```

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib policy::` and then `cargo test -p paigasus-helikon-providers-bedrock --lib model::tests::clones_share_the_policy` (Cargo accepts one test filter for each command)
Expected: compile errors (module `policy` and field `policy` do not exist).

- [ ] **Step 3: Implement** the items in the Interfaces block. `build()` creates `Arc::new(ForcedChoicePolicy::new(&model_id, family))` and logs one `debug!` (target `paigasus::bedrock::builder`) when the static rule selects native output. The `BedrockModel` doc keeps "`Clone + Send + Sync`" and says that clones share learned state; it does not link to `ForcedChoicePolicy`.

- [ ] **Step 4: Run the crate tests and see them pass**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib`
Expected: PASS (all builder tests pass with `model.config`).

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-bedrock --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-bedrock/src/policy.rs crates/paigasus-helikon-providers-bedrock/src/lib.rs crates/paigasus-helikon-providers-bedrock/src/model.rs crates/paigasus-helikon-providers-bedrock/src/builder.rs
git commit -m "$(cat <<'EOF'
fix(providers): SMA-839 add the per-handle forced-choice policy and output path

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: `native_output_schema` (denylist rewrite)

**Files:**
- Create: `crates/paigasus-helikon-providers-bedrock/src/translate/native_schema.rs`
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs:3-7` (add `pub(crate) mod native_schema;`)

**Interfaces:**
- Produces: `pub(crate) fn native_output_schema(schema: &serde_json::Value) -> serde_json::Value` (total, never fails, does not read `Ruleset`).

**Algorithm** (the tests do not fully determine it, so it is fixed here). `fn rewrite(v: &Value) -> Value`:

```text
if v is not an object: return v.clone()            # `true`, `false`, numbers pass through
m = clone of the object
# 1. keyword removal (only on this schema node, never on a properties/$defs map)
remove keys: minimum, maximum, exclusiveMinimum, exclusiveMaximum, multipleOf,
             maxItems, uniqueItems, not, if, then, else
if m.minItems is a number > 1: remove minItems     # 0 and 1 stay
# 2. oneOf -> anyOf
if m has oneOf:
    one = m.remove(oneOf)
    if m has no anyOf: m.anyOf = one
    else: append {"anyOf": one} to m.allOf (create allOf as [] if absent)
# 3. object nodes
is_object = m.type == "object"
         or (m.type is an array that contains "object")
         or m has key "properties"
is_map = m has key additionalProperties or patternProperties or propertyNames
if is_object and not is_map: m.additionalProperties = false
# (adding `false` next to propertyNames makes the object always empty: a wrong result)
# 4. recurse ONLY into these subschema positions
properties, $defs, definitions : object maps -> rewrite each value, keep the keys
anyOf, allOf                   : arrays      -> rewrite each element
items                          : rewrite only when it is an object (an array form is a tuple: keep)
# every other key (additionalProperties, patternProperties, propertyNames,
# prefixItems, $ref, enum, const, default, examples, ...) is kept as is, unvisited
return m
```

The `{}` schema has no type and no `properties`, so it stays `{}`. A recursive `$ref` (for example `"#"`) stays.

Fixtures for the schemars tests: these are the exact `schemars::schema_for!` outputs of schemars 1.2.3 (the `Cargo.lock` version). Bedrock has no schemars dependency, and the plan adds none, so paste them as `json!` literals. Source types: `/// The kind of a transaction.` `#[serde(rename_all = "snake_case")] enum Kind { /// Money in. Deposit, /// Money out. Withdrawal }`, `struct Money { amount: f64, currency: String }`, `struct Transaction { kind: Option<Kind>, money: Option<Money>, count: u32, tags: HashMap<String, String> }`, `struct Node { name: String, children: Vec<Node> }`.

```text
KIND        = {"$schema":"https://json-schema.org/draft/2020-12/schema","title":"Kind","description":"The kind of a transaction.","oneOf":[{"description":"Money in.","type":"string","const":"deposit"},{"description":"Money out.","type":"string","const":"withdrawal"}]}
OPTION_KIND = {"$schema":"https://json-schema.org/draft/2020-12/schema","title":"Nullable_Kind","anyOf":[{"$ref":"#/$defs/Kind"},{"type":"null"}],"$defs":{"Kind":{"description":"The kind of a transaction.","oneOf":[{"description":"Money in.","type":"string","const":"deposit"},{"description":"Money out.","type":"string","const":"withdrawal"}]}}}
OPTION_MONEY= {"$schema":"https://json-schema.org/draft/2020-12/schema","title":"Nullable_Money","anyOf":[{"$ref":"#/$defs/Money"},{"type":"null"}],"$defs":{"Money":{"type":"object","properties":{"amount":{"type":"number","format":"double"},"currency":{"type":"string"}},"required":["amount","currency"]}}}
TRANSACTION = {"$schema":"https://json-schema.org/draft/2020-12/schema","title":"Transaction","type":"object","properties":{"count":{"type":"integer","format":"uint32","minimum":0},"kind":{"anyOf":[{"$ref":"#/$defs/Kind"},{"type":"null"}]},"money":{"anyOf":[{"$ref":"#/$defs/Money"},{"type":"null"}]},"tags":{"type":"object","additionalProperties":{"type":"string"}}},"required":["count","tags"],"$defs":{"Kind":{"description":"The kind of a transaction.","oneOf":[{"description":"Money in.","type":"string","const":"deposit"},{"description":"Money out.","type":"string","const":"withdrawal"}]},"Money":{"type":"object","properties":{"amount":{"type":"number","format":"double"},"currency":{"type":"string"}},"required":["amount","currency"]}}}
NODE        = {"$schema":"https://json-schema.org/draft/2020-12/schema","title":"Node","type":"object","properties":{"children":{"type":"array","items":{"$ref":"#"}},"name":{"type":"string"}},"required":["name","children"]}
```

- [ ] **Step 1: Write the failing tests** in `native_schema.rs` `mod tests`. Each asserts with `assert_eq!(native_output_schema(&input), expected)`:

| Test | Input | Expected |
| --- | --- | --- |
| `native_schema_adds_additional_properties_false_to_object_nodes` | `{"type":"object"}`; `{"type":["object","null"]}`; `{"properties":{"a":{"type":"string"}}}`; `{"type":"string"}` | the first three get `"additionalProperties": false`; the fourth is unchanged |
| `native_schema_renames_one_of_to_any_of` | `{"oneOf":[{"type":"string"},{"type":"integer"}]}` | `{"anyOf":[{"type":"string"},{"type":"integer"}]}` |
| `native_schema_removes_numeric_limits` | `{"type":"number","minimum":0,"maximum":9,"exclusiveMinimum":0,"exclusiveMaximum":9,"multipleOf":2}` | `{"type":"number"}` |
| `native_schema_removes_max_items_and_unique_items` | `{"type":"array","maxItems":3,"uniqueItems":true}` | `{"type":"array"}` |
| `native_schema_removes_min_items_above_one_keeps_zero_and_one` | `minItems` 0, 1, 2 | 0 and 1 kept, 2 removed |
| `native_schema_removes_not_if_then_else` | `{"type":"string","not":{"const":"x"},"if":{},"then":{},"else":{}}` | `{"type":"string"}` |
| `native_schema_keeps_other_keywords` | a string node with `enum`, `const`, `format`, `pattern`, `minLength`, `maxLength`, `$schema`, `title`, `description`, `default`, `examples`; an object with `required` | unchanged except `additionalProperties: false` on the object |
| `native_schema_recurses_into_subschemas` | an object whose `properties.a` is an object, `properties.b` is an array with object `items`, `$defs.D` and `definitions.E` are objects, `anyOf`/`allOf` hold objects, each with `minimum: 1` on a nested number | every nested object gets `additionalProperties: false`, every `minimum` is gone |
| `native_schema_passes_through_unsafe_shapes` | `{"type":"object","additionalProperties":true}`; `{"type":"object","additionalProperties":{"type":"string"}}`; `{"type":"object","patternProperties":{"^a":{"type":"string"}}}`; `{"type":"object","propertyNames":{"pattern":"^a"}}`; `{"type":"array","prefixItems":[{"type":"string"}]}`; `true`; `{}`; `{"type":"array","items":{"$ref":"#"}}` | each unchanged (no `additionalProperties` is added to the `patternProperties` or `propertyNames` node) |
| `native_schema_keeps_property_names_and_values_that_match_keywords` | `{"type":"object","properties":{"minimum":{"type":"number"},"not":{"type":"string"},"if":{"type":"boolean"}},"default":{"minimum":5,"not":1},"examples":[{"maximum":2}]}` | the three property keys stay, `default` and `examples` are unchanged, root gets `additionalProperties: false` |
| `native_schema_one_of_next_to_any_of_keeps_both` | `{"anyOf":[{"type":"string"}],"oneOf":[{"minLength":1},{"maxLength":9}]}` | `{"anyOf":[{"type":"string"}],"allOf":[{"anyOf":[{"minLength":1},{"maxLength":9}]}]}` |
| `native_schema_schemars_unit_enum_with_docs` | `KIND` | `KIND` with `oneOf` renamed to `anyOf`; descriptions and `const` kept |
| `native_schema_schemars_option_enum` | `OPTION_KIND` | root `anyOf` unchanged; `$defs.Kind.oneOf` → `anyOf` |
| `native_schema_schemars_option_struct` | `OPTION_MONEY` | `$defs.Money.additionalProperties == false`; nothing else changes |
| `native_schema_schemars_u32_fields` | `TRANSACTION` | `properties.count == {"type":"integer","format":"uint32"}`; root and `$defs.Money` get `additionalProperties: false` |
| `native_schema_schemars_hashmap_field_passes_through` | `TRANSACTION` | `properties.tags == {"type":"object","additionalProperties":{"type":"string"}}` |
| `native_schema_schemars_recursive_type_passes_through` | `NODE` | `properties.children.items == {"$ref":"#"}`; root gets `additionalProperties: false` |

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib translate::native_schema`
Expected: compile error, `native_output_schema` not found.

- [ ] **Step 3: Implement `native_output_schema`** per the algorithm above. Module doc: the policy is a denylist; removed limits are not enforced by Bedrock, and core `validate_terminal` checks the result against the caller's original schema.

- [ ] **Step 4: Run them and see them pass**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib translate::native_schema`
Expected: PASS (17 tests).

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-bedrock --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-bedrock/src/translate/native_schema.rs crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs
git commit -m "$(cat <<'EOF'
fix(providers): SMA-839 add the native output schema rewrite for bedrock

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: The native request (`build_native`, `output_config`, `to_wire_json`)

**Files:**
- Create: `crates/paigasus-helikon-providers-bedrock/src/translate/native.rs`
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs:22-42` (`PreparedConverse`), `:91-94` (`build_request` signature + early branch), `:132-140` and `:232-239` (new fields), `:306-432` (`to_wire_json`), tests `:450-489` (helper), `:574-593` (invert test), new tests and snapshots
- Modify: `crates/paigasus-helikon-providers-bedrock/src/model.rs:49` (pass `self.policy.path()`, temporary until Task 5)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/capabilities.rs:1-17` (doc), `:98-122` (test doc and message)
- Possibly modify: root `Cargo.toml:181` (only if Step 6 shows that 1.135.0 does not compile)
- Test: `crates/paigasus-helikon-providers-bedrock/src/translate/snapshots/` (new files only)

**Interfaces:**
- Consumes: `OutputPath`, `NativeNotice`, `ForcedChoicePolicy::new` (Task 2); `native_output_schema` (Task 3); existing `synthesize`, `tool_specs`, `items_to_messages`, `build_inference_config`.
- Produces:
  - `PreparedConverse` gets `pub(crate) output_config: Option<aws_sdk_bedrockruntime::types::OutputConfig>` and `pub(crate) notices: Vec<NativeNotice>`. The `ForcedTool` branch sets `None` / `vec![]`.
  - `impl PreparedConverse { pub(crate) fn has_forced_tool_choice(&self) -> bool }` — `true` when `tool_config.tool_choice()` is `Any` or `Tool`.
  - `pub(crate) fn build_request(cfg: &Config, req: &ModelRequest, path: OutputPath) -> Result<PreparedConverse, ModelError>` — first line: `if path == OutputPath::Native { return native::build_native(cfg, req); }`. The rest of the body is unchanged.
  - `pub(crate) fn build_native(cfg: &Config, req: &ModelRequest) -> Result<PreparedConverse, ModelError>` in `native.rs`.
  - Test helper in `translate/mod.rs` tests: `fn build(cfg: &Config, req: &ModelRequest) -> Result<PreparedConverse, ModelError>` that calls `build_request(cfg, req, ForcedChoicePolicy::new(&cfg.model_id, cfg.family).path())`. Replace every existing test call `build_request(&x, &req)` with `build(&x, &req)`.
  - `fn sonnet55_cfg() -> Config { make_cfg("global.anthropic.claude-sonnet-5-5") }` and `fn assert_no_forced_choice(p: &PreparedConverse)` (asserts `!p.has_forced_tool_choice()`).

**`build_native` rules** (spec section 7.5, plus the decided question 1):

1. `translated = items_to_messages(&req.messages)?`.
2. If the format is `JsonSchema { name, schema, .. }`: `output_config = Some(OutputConfig::builder().text_format(OutputFormat::builder().r#type(OutputFormatType::JsonSchema).structure(OutputFormatStructure::JsonSchema(JsonSchemaDefinition::builder().schema(serde_json::to_string(&native_output_schema(schema))).name(name).build()?)).build()?).build())`. Map builder errors to `ModelError::Other`. `strict` has no effect.
3. If the format is `JsonObject`: no `outputConfig`; push `NativeNotice::JsonObjectAsText`.
4. If `tool_choice` is `Some(ToolChoice::None)`: send no user tools (do not call `tool_specs`, same as the forced path). Else `user_specs = tool_specs(&req.tools, ruleset)?`.
5. Anchor: if `output_config.is_some()`, no user tool is sent, and any translated message holds a `ContentBlock::ToolUse` or `ContentBlock::ToolResult`, append `synthesize(rf, family, ruleset)?.tool` (drop its `tool_choice`) and set `synthesizing = true`. Else `synthesizing = false`.
6. Tool choice:
   - `Auto` → `toolChoice: auto` (with no tools: omit `toolConfig`, keep the existing `debug!`).
   - `Required` → if no tool is in the list and the format is neither `JsonSchema` nor `JsonObject`, return `ModelError::Other("ToolChoice::Required requires at least one tool to be defined")` (same text as today). Else omit the choice and push `NativeNotice::RequiredOmitted`.
   - `Tool { name }` → if `name` is not in the tool list, return the existing error text `ToolChoice::Tool references tool {name:?} which is not defined in the tool list`. Else omit the choice and push `NativeNotice::ToolOmitted`.
   - `None` → handled in step 4; `toolConfig` holds only the anchor, if any.
   - Other (non-exhaustive) → omit with the existing `debug!`.
7. No conflict errors: `JsonSchema` + `Tool` and `JsonSchema` + `None` are accepted.
8. `toolConfig` = all tools (user tools first, anchor last) plus the optional `auto` choice, or `None` when the list is empty. `inference_config = build_inference_config(req, cfg)`.

**`to_wire_json`:** after the existing `json!` object is made, insert the key `"outputConfig"` **only when** `p.output_config` is `Some`, with the shape `{"textFormat": {"type": <OutputFormatType::as_str()>, "structure": {"jsonSchema": {"name": <name()>, "schema": <serde_json::from_str(schema())>}}}}`. Read it through `text_format()` / `structure()` / `as_json_schema()`, never through `Debug`. Do not project `notices`.

- [ ] **Step 1: Invert the existing test and write the failing tests** in `translate/mod.rs` tests.

Rename `json_schema_on_global_claude_profile_synthesizes` (line 574) to `json_schema_on_global_sonnet_5_synthesizes`, and change its model ID to `global.anthropic.claude-sonnet-5`. Its assertions stay. Add:

```rust
#[test]
fn json_schema_on_global_sonnet_5_5_uses_native_output() {
    // same request as json_schema_on_global_sonnet_5_synthesizes, on sonnet55_cfg()
    let p = build(&sonnet55_cfg(), &req).unwrap();
    assert_no_forced_choice(&p);
    assert!(p.tool_config.is_none());
    assert!(!p.synthesizing);
    let js = p.output_config.as_ref().unwrap().text_format().unwrap()
        .structure().unwrap().as_json_schema().unwrap();
    assert_eq!(js.name(), Some("Person"));
    assert_eq!(serde_json::from_str::<serde_json::Value>(js.schema()).unwrap(),
               json!({"type": "object", "additionalProperties": false}));
}
```

Snapshot tests on `sonnet55_cfg()`. Each calls `assert_no_forced_choice(&p)` and then `assert_json_snapshot!(to_wire_json(&p))`. The `Transaction` schema is the one in `snap_structured_output_json_schema_on_claude_synthesizes`. `search` is a user tool `{"type":"object","properties":{"q":{"type":"string"}}}`:

| Test | Request | Extra assertions |
| --- | --- | --- |
| `snap_native_json_schema` | user text + `JsonSchema Transaction` | `toolConfig` null, `outputConfig` present, `synthesizing == false` |
| `snap_native_json_schema_finalizing_turn_anchor` | user text, assistant `ToolUse tu_1 get_balance`, `ToolResult tu_1`, **no tools**, `JsonSchema Transaction` | `toolConfig.tools` is exactly the synthesized tool, `toolChoice` null, `synthesizing == true` |
| `snap_native_json_object_degrades_to_text` | user text + `JsonObject` | no `outputConfig` key, `toolConfig` null, `p.notices == [JsonObjectAsText]` |
| `snap_native_tool_choice_required_omitted` | user tool `search` + `Required` | `toolChoice` null, `p.notices == [RequiredOmitted]` |
| `snap_native_tool_choice_tool_omitted` | user tool `search` + `Tool{search}` | `toolChoice` null, `p.notices == [ToolOmitted]` |
| `snap_native_tool_choice_auto` | user tool `search` + `Auto` | `toolChoice == {"auto": {}}` |
| `snap_native_tool_choice_none_with_json_schema` | user tool `search` + `None` + `JsonSchema` | `toolConfig` null, `outputConfig` present |
| `snap_native_tool_choice_none_json_schema_finalizing_anchor` | tool-round history + `None` + `JsonSchema` | only the synthesized tool, `toolChoice` null, `synthesizing == true` |
| `snap_native_json_schema_plus_tool_choice_tool` | user tool `search` + `Tool{search}` + `JsonSchema` | `Ok`, `toolChoice` null, `outputConfig` present |
| `snap_native_json_schema_plus_required` | user tool `search` + `Required` + `JsonSchema` | `toolChoice` null, `outputConfig` present |
| `snap_native_user_tools_plus_json_schema` | tool-round history + user tool `get_balance` + `JsonSchema` | tools are exactly `[get_balance]` (no synthesized tool), `synthesizing == false` |

Unit tests (no snapshot):

- `native_tool_choice_required_no_tools_is_error` — `sonnet55_cfg()`, `Required`, no tools, no format → `Err`, message contains `requires at least one tool`.
- `native_tool_choice_tool_unknown_name_is_error` — `Tool{missing}`, user tool `search` → `Err`, message contains `not defined in the tool list`.
- `native_path_reports_no_notices_for_plain_requests` — plain text on `sonnet55_cfg()` → `p.notices.is_empty()`.

Anti-goal snapshots (forced path kept), same request as `snap_structured_output_json_schema_on_claude_synthesizes`:

- `snap_forced_path_kept_on_sonnet_5` — `make_cfg("global.anthropic.claude-sonnet-5")`.
- `snap_forced_path_kept_on_haiku_5_5` — `make_cfg("global.anthropic.claude-haiku-5-5")`.

Each asserts `toolChoice == {"tool":{"name":"__paigasus_structured_output__"}}` and that no `outputConfig` key is present, then snapshots.

- [ ] **Step 2: Run them and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib translate::tests`
Expected: compile errors (`build`, `output_config`, `notices`, `has_forced_tool_choice` do not exist).

- [ ] **Step 3: Implement** `build_native`, the new `PreparedConverse` fields, `has_forced_tool_choice`, the `build_request` branch and the `to_wire_json` projection, per the rules above. In `model.rs:49`, call `build_request(&self.config, &request, self.policy.path())`, add `.set_output_config(prepared.output_config)` to the fluent builder, and call `self.policy.note(n, &self.config.model_id)` for each notice. Task 5 replaces this code.

- [ ] **Step 4: Reword the capabilities premise.** In `capabilities.rs`, the module doc and the `caps_for` doc say that `structured_output` is set for families that support forced tool choice **or native output** (Sonnet 5.5 and Opus 5.5 use native output; `JsonObject` degrades to text there). In `structured_output_matches_forced_tool_choice_support`, keep the name and the assertion. Change the comment and the message to "structured_output != (forced tool choice or native output)". `caps_for` does not change.

- [ ] **Step 5: Accept the new snapshots and prove that the old ones did not change**

Run: `INSTA_UPDATE=always cargo test -p paigasus-helikon-providers-bedrock --lib translate::`
Then: `git diff --exit-code -- crates/paigasus-helikon-providers-bedrock/src/translate/snapshots/`
Expected: tests PASS, `git diff` exits 0 (the nine existing `.snap` files are byte-identical). `git status --short crates/paigasus-helikon-providers-bedrock/src/translate/snapshots/` shows only the 13 new `snap_native_*` / `snap_forced_path_*` files and no `.snap.new`. Read each new snapshot: no `"any"` or `"tool"` key under `toolChoice` in the native files, and `outputConfig.textFormat.type == "json_schema"`.

- [ ] **Step 6: Verify the lowest SDK version that compiles.** The current pin `1.135.0` already has `OutputConfig::text_format`, `OutputFormat`, `OutputFormatType::JsonSchema`, `OutputFormatStructure::JsonSchema`, `JsonSchemaDefinition` and `ConverseStreamFluentBuilder::set_output_config`; `1.148.0` adds only `OutputConfig::effort` (a non-goal).

```bash
cargo update -p aws-sdk-bedrockruntime --precise 1.135.0
cargo build -p paigasus-helikon-providers-bedrock
cargo update -p aws-sdk-bedrockruntime --precise 1.157.0
git diff --exit-code -- Cargo.lock
```

Expected: the build at 1.135.0 succeeds, so `Cargo.toml:181` stays at `version = "1.135.0"`. `git diff` on `Cargo.lock` exits 0. If the build fails, try 1.140.0, 1.144.0 and 1.148.0, bisect to the first version that builds, set that version on `Cargo.toml:181`, restore the lock to 1.157.0 the same way, and check that the only `Cargo.lock` change is none (the locked version stays 1.157.0). Record the result in the commit message body.

- [ ] **Step 7: Run the crate tests, fmt, clippy, commit**

```bash
cargo test -p paigasus-helikon-providers-bedrock
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-bedrock --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-bedrock/src/translate/native.rs crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs crates/paigasus-helikon-providers-bedrock/src/model.rs crates/paigasus-helikon-providers-bedrock/src/capabilities.rs crates/paigasus-helikon-providers-bedrock/src/translate/snapshots/
# add Cargo.toml only if Step 6 raised the pin
git commit -m "$(cat <<'EOF'
fix(providers): SMA-839 send native structured output to models that reject a forced tool choice

aws-sdk-bedrockruntime 1.135.0 already has every OutputConfig type in use, so the pin stays.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Rule B — the classifier and `send_with_fallback`

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/error.rs:1-9` (module doc, ADR-10 note), add the classifier after `classify` (line 60) and `send_failure` after `map_sdk_http_error` (line 134); tests in `mod tests`
- Create: `crates/paigasus-helikon-providers-bedrock/src/fallback.rs`
- Modify: `crates/paigasus-helikon-providers-bedrock/src/lib.rs` (add `mod fallback;`)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/model.rs:42-129` (`invoke`)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/builder.rs` (add the test-only config helper), `crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs:450-474` (`make_cfg` delegates to it)

**Interfaces:**
- Consumes: `ForcedChoicePolicy::{family, path, record_rejection, note}` (Task 2); `build_request(cfg, req, path)`, `PreparedConverse::{has_forced_tool_choice, synthesizing, notices, output_config}`, `to_wire_json` (Task 4).
- Produces:
  - `pub(crate) fn is_forced_choice_rejection(code: Option<&str>, message: &str) -> bool` in `error.rs`. Body:

    ```rust
    code == Some("ValidationException")
        && (message.contains(r#"type "tool" and "any" are not supported"#)
            || message.contains("doesn't support the toolConfig.toolChoice."))
    ```

  - `pub(crate) struct SendFailure { pub(crate) forced_choice_rejected: bool, pub(crate) error: ModelError }` in `error.rs`.
  - `pub(crate) fn send_failure<E: ProvideErrorMetadata + std::fmt::Debug>(err: SdkError<E, aws_smithy_runtime_api::http::Response>) -> SendFailure` — reads the raw `svc.err().code()` / `message()` for a `ServiceError` **before** it calls `map_sdk_http_error(err)`; other variants give `forced_choice_rejected: false`.
  - `pub(crate) struct Sent<T> { pub(crate) output: T, pub(crate) synthesizing: bool }` in `fallback.rs`.
  - In `fallback.rs`:

    ```rust
    pub(crate) async fn send_with_fallback<T, B, S, Fut>(
        policy: &ForcedChoicePolicy,
        model_id: &str,
        first: PreparedConverse,
        rebuild: B,
        mut send: S,
    ) -> Result<Sent<T>, ModelError>
    where
        B: FnOnce(OutputPath) -> Result<PreparedConverse, ModelError>,
        S: FnMut(PreparedConverse) -> Fut,
        Fut: std::future::Future<Output = Result<T, SendFailure>>,
    ```

    Flow: log `policy.note` for each notice of `first`; remember `first.synthesizing` and `first.has_forced_tool_choice()`; send `first`. On `Ok` return `Sent { output, synthesizing }`. On `Err(f)`: fall back only when `policy.family() == ModelFamily::Anthropic && first_was_forced && f.forced_choice_rejected`; else return `Err(f.error)`. Fallback: if `policy.record_rejection()` is `true`, log one `tracing::warn!` (target `paigasus::bedrock::model`, field `model_id`, text "the model rejected a forced tool choice; native structured output is now in use for this handle"). Call `rebuild(OutputPath::Native)?`, log its notices, send it once, and return its result (an error goes back as `f.error`, no further fallback). `rebuild` is consumed, so the owned `ModelRequest` it holds is dropped when the function returns.
  - `#[cfg(test)] pub(crate) fn offline_config(model_id: &str) -> Config` in `builder.rs`, with the body of the current `make_cfg` (`translate/mod.rs:450-474`). `make_cfg` calls it.

- [ ] **Step 1: Write the failing classifier tests** in `error.rs` `mod tests`:

```rust
const MODEL_LAYER: &str = "The model returned the following errors: tool_choice: type \"tool\" and \"any\" are not supported for this model.";
const BEDROCK_LAYER: &str = "This model doesn't support the toolConfig.toolChoice.any field. Remove toolConfig.toolChoice.any and try again.";

#[test]
fn forced_choice_rejection_matches_both_patterns() {
    assert!(is_forced_choice_rejection(Some("ValidationException"), MODEL_LAYER));
    assert!(is_forced_choice_rejection(Some("ValidationException"), BEDROCK_LAYER));
}

#[test]
fn forced_choice_rejection_ignores_near_misses() {
    for msg in [
        "Input is too long for requested model.",
        "output_config: Extra inputs are not permitted",
        "This model doesn't support the outputConfig field.",
        "The toolConfig field must be defined when using toolUse and toolResult content blocks.",
    ] {
        assert!(!is_forced_choice_rejection(Some("ValidationException"), msg), "{msg}");
    }
    assert!(!is_forced_choice_rejection(Some("ThrottlingException"), MODEL_LAYER));
    assert!(!is_forced_choice_rejection(None, MODEL_LAYER));
}
```

- [ ] **Step 2: Write the failing fallback tests** in `fallback.rs` `mod tests`. Use plain `#[test]` functions: build configs with `offline_config` first (it starts its own runtime), then drive the future with a new current-thread runtime and `block_on`. The fake send closure pushes `to_wire_json(&p)` into an `Arc<Mutex<Vec<Value>>>` and pops the next scripted `Result<(), SendFailure>` from a `VecDeque`. `rejected()` is `SendFailure { forced_choice_rejected: true, error: ModelError::Other(anyhow!("bedrock ValidationException: ...")) }`. The request is `JsonSchema Transaction` with user text unless a row says otherwise. Each policy is `ForcedChoicePolicy::without_static_rule(family)`, so that the first request uses the forced path:

| Test | Config / script | Assertions |
| --- | --- | --- |
| `rejection_falls_back_once_to_native` | `anthropic.claude-3-5-sonnet-20241022-v2:0`; `[rejected, Ok]` | 2 sends; send 1 `toolConfig.toolChoice == {"tool":…}`; send 2 has `outputConfig` and `toolConfig` null; `policy.path() == Native`; `Ok` |
| `second_request_on_same_handle_goes_native_at_once` | Anthropic; run the `[rejected, Ok]` fallback first, then on the same policy a second call with `first = build_request(cfg, req, policy.path())` and script `[Ok]` | the second call makes 1 send, and it has `outputConfig` and no forced choice |
| `non_anthropic_family_does_not_fall_back` | `mistral.mistral-large-2407-v1:0`; `[rejected]` | 1 send; `Err`; `policy.path() == ForcedTool` |
| `other_errors_do_not_fall_back` | Anthropic; `[SendFailure { forced_choice_rejected: false, error: ModelError::RateLimited { retry_after_ms: Some(1000) } }]` | 1 send; `Err(RateLimited { retry_after_ms: Some(1000) })`; `policy.path() == ForcedTool` |
| `second_rejection_is_returned` | Anthropic; `[rejected, rejected]` | 2 sends; `Err(ModelError::Other(_))` |
| `request_without_forced_choice_does_not_fall_back` | Anthropic, plain text request (no format, no tools); `[rejected]` | 1 send; `Err`; `policy.path() == ForcedTool` |
| `fallback_returns_synthesizing_of_the_sent_request` | Anthropic; `[rejected, Ok]` | send 1 had `"synthesizing": true`; `Sent.synthesizing == false` |

- [ ] **Step 3: Run them and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib error::tests` and then `cargo test -p paigasus-helikon-providers-bedrock --lib fallback::` (Cargo accepts one test filter for each command)
Expected: compile errors (`is_forced_choice_rejection`, `SendFailure`, `send_with_fallback` do not exist).

- [ ] **Step 4: Implement** the classifier, `SendFailure`, `send_failure`, `offline_config` and `send_with_fallback` per the Interfaces block.

- [ ] **Step 5: Rewire `invoke`** in `model.rs`:
  1. Before the stream: `let first = build_request(&self.config, &request, self.policy.path())?;` This keeps the early error (`invoke_rejects_blank_only_request_before_network` must stay green).
  2. Move `request`, `Arc<Config>`, `Arc<ForcedChoicePolicy>` and the client into the `stream!`. The `rebuild` closure is `move |path| build_request(&cfg, &request, path)`. Nothing else in the stream holds `request`.
  3. Add a private `fn send_converse_stream(client: &Client, p: PreparedConverse) -> impl Future<Output = Result<ConverseStreamOutput, SendFailure>>`. It builds the same fluent request as today, plus `.set_output_config(p.output_config)`, and maps errors with `send_failure`.
  4. Wrap the whole `send_with_fallback(...)` call in the existing `tokio::select! { biased; _ = cancel.cancelled() => return, r = ... => r }`. A cancel during either send ends the stream with no event.
  5. On `Err(e)` yield `Err(e)` and return. On `Ok(Sent { output, synthesizing })` use `StreamTranslator::new(synthesizing)` and the existing receive loop, unchanged.
  6. Remove the temporary Task 4 code (direct `note` calls, direct `set_output_config`).

- [ ] **Step 6: Write the ADR-10 note.** In the `error.rs` module doc, after the ADR-10 sentence, and in the `fallback.rs` module doc, state the four reasons from spec 7.4: the fallback sends a *different* request; it occurs at most once for each request; it is logged; it never follows a transient error. Also state the weak contract: if Bedrock changes the text, the result is the same `ValidationException` as before, and rule A still covers the known models. Use prose, with no links to `pub(crate)` items from `pub` docs.

- [ ] **Step 7: Run the crate tests and the conformance suite**

Run: `cargo test -p paigasus-helikon-providers-bedrock && cargo test -p paigasus-helikon-provider-stream-conformance --all-features`
Expected: PASS. The conformance suite drives a real `BedrockModel` (`anthropic.claude-3-5-sonnet-20240620-v1:0`) through the local server, so the forced path and the stream are unchanged.

- [ ] **Step 8: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-bedrock --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-bedrock/src/error.rs crates/paigasus-helikon-providers-bedrock/src/fallback.rs crates/paigasus-helikon-providers-bedrock/src/lib.rs crates/paigasus-helikon-providers-bedrock/src/model.rs crates/paigasus-helikon-providers-bedrock/src/builder.rs crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs
git commit -m "$(cat <<'EOF'
fix(providers): SMA-839 fall back to native output once when bedrock rejects a forced tool choice

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Live tests (manual, `#[ignore]`)

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/tests/live.rs` (module doc table at `:1-20`, new tests after `structured_output_synthesis` at `:232-295`)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/model.rs` `mod tests` (the rule B live test)

**Interfaces:**
- Consumes: `BedrockModel::from_env`, the existing helpers `skip_if_missing_env`, `user`, `tool_call`, `tool_result`; in-crate `BedrockModel { config, policy }` and `ForcedChoicePolicy::without_static_rule` (Task 2).
- Env vars (existing names): `BEDROCK_MODEL_ID`, `AWS_DEFAULT_REGION` (or `AWS_REGION`), and the AWS credential chain (`AWS_PROFILE=maip-dev-admin`). An unset `BEDROCK_MODEL_ID` gives the existing loud skip.

- [ ] **Step 1: Add the live tests.** All are `#[tokio::test] #[ignore]`. All skip unless the family is `Anthropic`. All set `max_output_tokens = Some(256)`. All collect `TokenDelta` text, assert no `Err` event, and parse the text with `serde_json::from_str::<Value>`. Factor the common part into `async fn collect_json(model_id: &str, req: ModelRequest) -> serde_json::Value`.
  - `native_output_schemars_style_schema` — prompt `Give a transaction: deposit of 100 USD.`, `JsonSchema` name `Transaction` with this schema: `{"type":"object","properties":{"count":{"type":"integer","format":"uint32","minimum":0},"money":{"type":"object","properties":{"amount":{"type":"number"},"currency":{"type":"string"}},"required":["amount","currency"]},"kind":{"anyOf":[{"$ref":"#/$defs/Kind"},{"type":"null"}]}},"required":["count","money","kind"],"$defs":{"Kind":{"oneOf":[{"type":"string","const":"deposit"},{"type":"string","const":"withdrawal"}]}}}`. Assert that `money.amount` is a number and `count` is a non-negative integer.
  - `structured_output_finalizing_turn_after_tool_round` — messages: `user("What is my balance?")`, `tool_call("tu_1", "get_balance")`, `tool_result("tu_1", "1234.56 USD")`, `user("Report the balance.")`; **no tools**; `JsonSchema` `Balance` `{"type":"object","properties":{"amount":{"type":"number"},"currency":{"type":"string"}},"required":["amount","currency"]}`. Assert that `amount` is a number.
  - Extend the module doc table with the run commands of Task 8 Step 3.
- [ ] **Step 2: Add the in-crate rule B test** in `model.rs` `mod tests`: `live_rule_b_learns_native_output_after_rejection`, `#[tokio::test] #[ignore]`, env-gated on `BEDROCK_MODEL_ID` (loud skip). It builds `BedrockModel::from_env(&id)`. It replaces the policy with `Arc::new(ForcedChoicePolicy::without_static_rule(family))`, so that rule A is off. It asserts `path() == ForcedTool`, then sends the `structured_output_synthesis` request (`Transaction` with `operation`/`amount`). After that it asserts `path() == Native` (the only way to set the flag is one classified rejection) and that the text parses as JSON with a string `operation`. Doc: run it only with `BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5`.
- [ ] **Step 3: Check that the tests compile and skip offline**

Run: `cargo test -p paigasus-helikon-providers-bedrock --all-targets -- --ignored native_output_schemars_style_schema structured_output_finalizing_turn_after_tool_round live_rule_b` with `BEDROCK_MODEL_ID` unset.
Expected: PASS with the `[live] BEDROCK_MODEL_ID not set` skip lines.

- [ ] **Step 4: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-bedrock --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-bedrock/tests/live.rs crates/paigasus-helikon-providers-bedrock/src/model.rs
git commit -m "$(cat <<'EOF'
test(providers): SMA-839 add live tests for bedrock native structured output and the fallback

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Crate README

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/README.md:3`, `:61-63`, `:73-79`

**Interfaces:**
- Consumes: the behavior of Tasks 1-5. The README is not `include_str!`'d into rustdoc (only the facade README is), so no fence becomes a doctest.

- [ ] **Step 1: Edit the README** (STE prose):
  - Line 3: "structured output (family-gated via forced-tool synthesis)" → "structured output (forced-tool synthesis, or native output on Claude Sonnet 5.5 and Opus 5.5)".
  - Family table, `Anthropic` row: Structured output "yes (forced-tool synthesis; native output on Sonnet 5.5 and Opus 5.5)"; Forced tool choice "yes, except Sonnet 5.5 and Opus 5.5".
  - Line 75: remove "This approach works without Bedrock native structured-output support." and say that Sonnet 5.5 and Opus 5.5 use native output instead (next section).
  - Line 77: limit the sentence to **tool input schemas** ("For tool input schemas, Bedrock's Converse validator rejects …"). The native output schema accepts `$ref`/`$defs` and `anyOf`.
  - New section `### Native structured output (Claude Sonnet 5.5 and Opus 5.5)` after line 79. It states: these models reject a forced `toolChoice`, so `JsonSchema` goes in `outputConfig.textFormat`. A model ID that is not on the list, but that also rejects the forced choice, costs one rejected request for each `BedrockModel` handle. After that, the handle and its clones use native output, and the crate logs one warning. The schema rules: `additionalProperties: false` is added to object nodes; `oneOf` becomes `anyOf`; `minimum`/`maximum`/`exclusiveMinimum`/`exclusiveMaximum`/`multipleOf`, `maxItems`, `uniqueItems`, `minItems` above 1, and `not`/`if`/`then`/`else` are removed. The server does not enforce the removed limits; the agent loop checks the result against the original schema and asks for a repair. Bedrock rejects maps (`HashMap`, `additionalProperties` true or a schema), `patternProperties`, `propertyNames`, tuples (`prefixItems`), the schemas `true` and `{}`, and recursive types. `JsonObject` degrades to text. User tools stay callable when `JsonSchema` is set. `ToolChoice::Required` and `ToolChoice::Tool` are omitted with a warning (`Required` with no tools is still an error).
- [ ] **Step 2: Lint**

Run: `npm ci` (only if `node_modules` is missing), then `npx markdownlint-cli2 crates/paigasus-helikon-providers-bedrock/README.md`
Expected: `0 error(s)`.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/README.md
git commit -m "$(cat <<'EOF'
docs(providers): SMA-839 document bedrock native structured output in the crate readme

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: mdBook page

**Files:**
- Modify: `docs/book/src/concepts/model-providers.md:133-135` (Bedrock paragraph), `:175-184` ("Family-gated structured output"), `:201` (tool-input schema rewriter sentence)

**Interfaces:**
- Consumes: the README text of Task 7 (keep the two consistent; the book is shorter).

- [ ] **Step 1: Edit the page** (STE prose). Lines 133-135: structured output is forced-tool synthesis on Claude, Nova and Mistral, and native output on Claude Sonnet 5.5 and Opus 5.5. "Family-gated structured output": add one paragraph on the native path. It covers the two models, the one-time runtime fallback for other models that reject a forced choice, the schema rewrite in short, the shapes that Bedrock rejects, `JsonObject` as text, and the omitted `Required`/`Tool`. Line 201: say "for tool input schemas".
- [ ] **Step 2: Build and lint**

Run: `mdbook build docs/book && npx markdownlint-cli2 docs/book/src/concepts/model-providers.md`
Expected: the build ends with no error (linkcheck `warning-policy = "error"`), and the lint reports `0 error(s)`.

- [ ] **Step 3: Commit**

```bash
git add docs/book/src/concepts/model-providers.md
git commit -m "$(cat <<'EOF'
docs(docs): SMA-839 add bedrock native structured output to the model providers page

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: Final verification

**Files:** none (verification only). If a gate fails, fix the cause in the task that owns the file, as a new commit.

- [ ] **Step 1: Run the CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo build -p paigasus-helikon-providers-bedrock
cargo test -p paigasus-helikon-providers-bedrock --all-features
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
npx markdownlint-cli2
bash scripts/check-markdownlint-config.sh
mdbook build docs/book
git diff --diff-filter=MD --exit-code main -- crates/paigasus-helikon-providers-bedrock/src/translate/snapshots/ crates/paigasus-helikon-providers-bedrock/tests/snapshots/
git diff main --stat -- Cargo.lock Cargo.toml crates/*/Cargo.toml
```

Expected: every command exits 0. The `cargo doc` output has no `private_intra_doc_links` warning. The `--diff-filter=MD` diff is empty: no pre-existing snapshot is modified or deleted. The `Cargo.lock` / `Cargo.toml` stat is empty, unless Task 4 Step 6 raised the pin. Then the only change is the `aws-sdk-bedrockruntime` line in the root `Cargo.toml`. No crate `version` line changed. Optional, if the nightly is installed: `DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh`.

- [ ] **Step 2: Check the credentials for the live tests**

Run: `aws sts get-caller-identity --profile maip-dev-admin`
Expected: an account JSON. If it fails (expired SSO session), stop and ask Sven to run `aws sso login --profile maip-dev-admin`. Do not report the live tests as passed without a real run.

- [ ] **Step 3: Run the live acceptance tests** (each must print `test result: ok. 1 passed` and no `[live] ... skipping` line):

```bash
AWS_PROFILE=maip-dev-admin AWS_DEFAULT_REGION=us-east-1 BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5 \
  cargo test -p paigasus-helikon-providers-bedrock --test live -- --ignored structured_output_synthesis
AWS_PROFILE=maip-dev-admin AWS_DEFAULT_REGION=us-east-1 BEDROCK_MODEL_ID=us.anthropic.claude-opus-5-5 \
  cargo test -p paigasus-helikon-providers-bedrock --test live -- --ignored structured_output_synthesis
AWS_PROFILE=maip-dev-admin AWS_DEFAULT_REGION=us-east-1 BEDROCK_MODEL_ID=us.anthropic.claude-sonnet-5 \
  cargo test -p paigasus-helikon-providers-bedrock --test live -- --ignored structured_output_synthesis
AWS_PROFILE=maip-dev-admin AWS_DEFAULT_REGION=us-east-1 BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5 \
  cargo test -p paigasus-helikon-providers-bedrock --test live -- --ignored native_output_schemars_style_schema structured_output_finalizing_turn_after_tool_round
AWS_PROFILE=maip-dev-admin AWS_DEFAULT_REGION=us-east-1 BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5 \
  cargo test -p paigasus-helikon-providers-bedrock --lib -- --ignored live_rule_b_learns_native_output_after_rejection
```

Expected: all PASS. The rule B run logs the "native structured output is now in use" warning one time (add `RUST_LOG=paigasus=warn` if the test prints logs).

- [ ] **Step 4: Confirm the branch contents**

Run: `git log --oneline main..HEAD` and `git status --short`
Expected: the spec commit, this plan's commit (if made), and the eight task commits; a clean tree with no `.snap.new`, `.env` or `.claude/` files staged.
