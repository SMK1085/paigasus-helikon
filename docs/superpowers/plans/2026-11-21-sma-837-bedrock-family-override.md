# SMA-837 Bedrock family override Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `BedrockModelBuilder::family(ModelFamily)` so that a caller with an opaque model ID (for example an application inference profile ARN) can choose the family that the request translation, the default capabilities and the forced-choice policy use.

**Architecture:** `build()` resolves one effective family (override, else detection) through one function and gives that value to `Config.family`, `caps_for` and `ForcedChoicePolicy::new`. The static rule A in the policy is gated on `family.supports_forced_tool_choice()`. Docs move users from `.capabilities(...)` to `.family(...)`.

**Tech Stack:** Rust (MSRV 1.94), `aws-sdk-bedrockruntime`, `tracing`, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-11-21-sma-837-bedrock-family-override-design.md`

## Global Constraints

- Crate: `crates/paigasus-helikon-providers-bedrock`. No other crate changes code.
- No hand version bump, no CHANGELOG edit (release-plz does both).
- Commit format: `<type>(<scope>): SMA-837 <lowercase message>`. Code commits use `feat(providers)`, test-only commits `test(providers)`, docs commits `docs(providers)`. `providers-bedrock` is NOT in the `.versionrc` scope allowlist; the commit-msg hook rejects it.
- User-facing text (rustdoc, README, mdBook) is in ASD-STE100 Simplified Technical English.
- Gates to keep green: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`, `cargo test -p paigasus-helikon-providers-bedrock --all-features`, `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-providers-bedrock --no-deps`, `npx markdownlint-cli2`, `mdbook build docs/book`.
- Do not run any git command that moves HEAD or switches branches. Work synchronously in the foreground.
- Test ARN constant: `arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/a1b2c3d4e5f6`.

## Review Focus

- An override equal to the detected family must not change anything (rule A stays on for a Sonnet 5.5 ID with `.family(Anthropic)`). Pinned in Task 1.
- `.family(Unknown)` on a Sonnet 5.5 ID must not send `outputConfig`. Pinned in Task 1.
- `.capabilities(...)` called before or after `.family(...)` must still win. Pinned in Task 1 (call order: `.capabilities` first, then `.family`).
- A second `.family(..)` call replaces the first. Pinned in Task 1.
- Rule B must work on a handle the builder made for an opaque ARN. Pinned in Task 2.

---

### Task 1: The `family` builder method and the effective family

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/builder.rs` (struct field, setter, `build()` step 3, `offline_config`, doc comments, tests)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/policy.rs:36-40` (rule A gate, doc)
- Test: `crates/paigasus-helikon-providers-bedrock/src/builder.rs` (`mod tests`)

**Interfaces:**
- Produces: `pub fn family(mut self, f: ModelFamily) -> Self` on `BedrockModelBuilder`.
- Produces: `pub(crate) fn resolve_family(model_id: &str, family_override: Option<ModelFamily>) -> ModelFamily` in `builder.rs`. `build()` and `offline_config` both call it (`offline_config` passes `None`).
- Produces: `#[cfg(test)] pub(crate) fn offline_client() -> Client` in `builder.rs` (move the existing private helper from `mod tests` up to module level so Task 2 can use it).
- Changes: `ForcedChoicePolicy::new(model_id, family)` sets `rejects_static = family.supports_forced_tool_choice() && rejects_forced_tool_choice(model_id)`.

- [ ] **Step 1: Write the failing tests in `builder.rs` `mod tests`**

Add a helper `fn json_schema_request() -> ModelRequest` (one user message, `ResponseFormat::JsonSchema` with a small object schema, `strict: false`) and a helper that builds the wire JSON from a built model:
`fn wire(model: &BedrockModel, req: &ModelRequest) -> serde_json::Value` = `to_wire_json(&build_request(&model.config, req, model.policy.path()).unwrap())`.
Add a helper `fn assert_one_family(model: &BedrockModel, want: ModelFamily)` that asserts `model.config.family == want`, `model.policy.family() == want`, and `model.capabilities().structured_output == caps_for(want).structured_output`.

Tests (ARN = the Global Constraints constant, `SONNET55 = "global.anthropic.claude-sonnet-5-5"`):
- `family_override_turns_on_synthesis_for_opaque_arn`: `.family(Anthropic)`; `assert_one_family(.., Anthropic)`; `wire["toolConfig"]["toolChoice"] == json!({"tool": {"name": SYNTHESIZED_TOOL_NAME}})`; the `toolConfig.tools` array has a tool named `SYNTHESIZED_TOOL_NAME`; `wire.get("outputConfig").is_none()`.
- `opaque_arn_without_override_is_unchanged`: no override; `assert_one_family(.., Unknown)`; `wire.get("toolConfig").is_none()`; `wire.get("outputConfig").is_none()`.
- `family_override_maps_required_tool_choice_to_any`: ARN, `.family(Anthropic)`, one user tool, `ToolChoice::Required`, no response format; `wire["toolConfig"]["toolChoice"] == json!({"any": {}})`.
- `capabilities_override_still_wins_over_family_override`: `.capabilities(ModelCapabilities::empty()).family(Anthropic)`; `!model.capabilities().structured_output`; `model.config.family == Anthropic`.
- `second_family_call_replaces_first`: `.family(Mistral).family(Anthropic)` → `assert_one_family(.., Anthropic)`.
- `unknown_override_turns_off_synthesis_on_anthropic_id`: `anthropic.claude-3-5-sonnet-20241022-v2:0` with `.family(Unknown)`; `assert_one_family(.., Unknown)`; no `toolConfig`, no `outputConfig`.
- `unknown_override_on_sonnet55_sends_no_output_config`: `SONNET55` with `.family(Unknown)`; `model.policy.path() == OutputPath::ForcedTool`; no `toolConfig`, no `outputConfig`; `!model.capabilities().structured_output`.
- `anthropic_override_on_sonnet55_keeps_native_path`: `SONNET55` with `.family(Anthropic)`; `model.policy.path() == OutputPath::Native`; `wire["outputConfig"]` is present.
- `mistral_override_on_sonnet55_selects_native_path`: `SONNET55` with `.family(Mistral)`; `model.policy.path() == OutputPath::Native`.
- `resolve_family_prefers_override`: `resolve_family(ARN, None) == Unknown`; `resolve_family(ARN, Some(Anthropic)) == Anthropic`; `resolve_family("anthropic.claude-x", Some(Unknown)) == Unknown`.

- [ ] **Step 2: Run the tests and see them fail**

Run: `cargo test -p paigasus-helikon-providers-bedrock --lib builder::tests`
Expected: compile error, no method `family` / no function `resolve_family`.

- [ ] **Step 3: Implement**

- Field `family_override: Option<ModelFamily>` (default `None`) with a doc line.
- `family()` rustdoc (STE): it sets the family and turns off detection; it controls the request translation, the default capabilities and the forced-choice policy, like a detected family; `.capabilities(...)` still wins for the reported flags; use it for opaque IDs (application inference profile, provisioned-throughput and custom-model ARNs); with `Anthropic`, `Mistral` or `AmazonNova`, `ToolChoice::Required`/`Tool` are sent, and some requests become local errors (`JsonSchema`/`JsonObject` with `ToolChoice::Tool` or `ToolChoice::None`, `Required` with no tools, `Tool` with an undefined name); a profile on Claude Sonnet 5.5 or Opus 5.5 costs one rejected request per handle, so build one handle and clone it. Include a `no_run`/`ignore` example: `BedrockModel::converse(arn).sdk_config(&cfg).family(ModelFamily::Anthropic).build()?`.
- `build()` step 3: `let family = resolve_family(&self.model_id, self.family_override);` Logs:
  - no override and family is `Unknown`: keep the existing `debug!`, and append `; set BedrockModelBuilder::family to choose the family` to its text.
  - override set, detected family is not `Unknown`, override is not `Unknown`, and they differ: `tracing::warn!(target: "paigasus::bedrock::builder", model_id, detected = ?d, family = ?f, "the family override differs from the family that the model id names")`.
  - other override cases: `debug!` with `model_id` and `family`, text `"the model family comes from BedrockModelBuilder::family"`.
- `offline_config` uses `resolve_family(model_id, None)`.
- Doc updates in this file: `Config.family` ("Effective model family: the builder override, else the family detected from the model id."); `.capabilities()` rustdoc gets: "It changes only what `capabilities()` reports. It does not change the request. To turn on the family-gated request behavior for an opaque model ID, use [`BedrockModelBuilder::family`]."
- `policy.rs`: the gate above; doc of `new`: "The static rule comes from the model id. It applies only when `family` supports a forced tool choice."

- [ ] **Step 4: Run the tests and see them pass**

Run: `cargo test -p paigasus-helikon-providers-bedrock --all-features`
Expected: all tests pass, including the existing `policy::tests` and `family::tests`.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/src/builder.rs crates/paigasus-helikon-providers-bedrock/src/policy.rs
git commit -m "feat(providers): SMA-837 add a family override to the bedrock builder"
```

### Task 2: Rule B on a built handle, and the live profile test

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/fallback.rs` (`mod tests`)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/model.rs` (`mod tests`, one `#[ignore]` live test)

**Interfaces:**
- Consumes: `BedrockModelBuilder::family`, `crate::builder::offline_client()` (Task 1); `drive`, `rejected`, `transaction_request`, `assert_forced` in `fallback.rs` tests.

- [ ] **Step 1: Write the tests**

In `fallback.rs` tests:
- `rule_b_recovers_on_built_handle_with_family_override`: `let model = BedrockModel::converse(ARN).client(offline_client()).family(ModelFamily::Anthropic).build().unwrap();` `drive(&model.policy, &model.config, &transaction_request(), vec![Err(rejected()), Ok(())])` → result is `Ok`; `wires.len() == 2`; `assert_forced(&wires[0])`; `wires[1]["outputConfig"]` is present and `wires[1]["toolConfig"]["toolChoice"]` is absent; `model.policy.path() == OutputPath::Native`.
- `opaque_arn_without_override_sends_one_request`: same ARN, no override, script `vec![Ok(())]` → `wires.len() == 1`; `wires[0].get("toolConfig").is_none()`; `model.policy.path() == OutputPath::ForcedTool`.

In `model.rs` tests, `live_profile_arn_with_family_override` (`#[tokio::test] #[ignore]`): read `BEDROCK_PROFILE_ARN`, skip with an `eprintln!` when unset or empty (same pattern as `live_rule_b_learns_native_output_after_rejection`). Load `aws_config::defaults(BehaviorVersion::v2026_11_17()).load().await`, build `BedrockModel::converse(&arn).sdk_config(&cfg).family(ModelFamily::Anthropic).build()`, send the same `JsonSchema` request as the rule-B live test, collect `TokenDelta` text, assert it parses as JSON with a string `operation`, and `eprintln!` the final `model.policy.path()`. Doc comment: how to run it, and that the dev account has no profile today, so it has not run.

- [ ] **Step 2: Run the tests**

Run: `cargo test -p paigasus-helikon-providers-bedrock --all-features`
Expected: PASS; the live test is listed as ignored. These tests check Task 1 wiring, so they pass at once. Mutation check: temporarily change `build()` to pass `ModelFamily::from_model_id(&self.model_id)` to `ForcedChoicePolicy::new`, confirm `rule_b_recovers_on_built_handle_with_family_override` fails, then revert.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock/src/fallback.rs crates/paigasus-helikon-providers-bedrock/src/model.rs
git commit -m "test(providers): SMA-837 cover rule b on a handle with a family override"
```

### Task 3: Docs and the stale-text sweep

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/capabilities.rs:8-9` (module doc)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/family.rs:3-7` (type doc) and the `from_model_id` doc (opaque IDs → point to `BedrockModelBuilder::family`)
- Modify: `crates/paigasus-helikon-providers-bedrock/README.md` ("Model families" section)
- Modify: `docs/book/src/concepts/model-providers.md` (Bedrock "Family-gated structured output", after the ARN paragraph)
- Modify: any file the sweep finds

- [ ] **Step 1: Rustdoc fixes**

`capabilities.rs`: replace "callers may override via `BedrockModelBuilder::capabilities`" with: unknown families get conservative defaults; to choose the family for an opaque model ID, use [`crate::BedrockModelBuilder::family`]; `.capabilities(...)` changes only the reported flags. `family.rs` type doc: replace "Used internally" with: the builder detects it from the model ID, or takes it from `BedrockModelBuilder::family`. Use intra-doc links only to `pub` items.

- [ ] **Step 2: README and mdBook**

After the detection paragraph in both files, add a subsection "Opaque model IDs" (README `###`, mdBook `####`) in STE with:
- which IDs are opaque (application inference profile, provisioned-throughput, custom-model ARNs) and that they give `Unknown`;
- the example `BedrockModel::converse(arn).sdk_config(&sdk_cfg).family(ModelFamily::Anthropic).build()?` (mdBook: `paigasus_helikon::bedrock::{BedrockModel, ModelFamily}`), fenced ` ```rust,ignore ` (mdBook) / ` ```rust,ignore ` (README);
- the override has the full effect of a detected family, also on `ToolChoice` and the local errors (name them);
- `.capabilities(...)` changes only the reported flags;
- a profile on Claude Sonnet 5.5 or Opus 5.5: the first structured-output request sends a forced choice, Bedrock rejects it once, then the handle uses native output; before that, some requests fail locally and cannot teach the handle; parallel first requests each get the rejection; a new handle pays again; build one handle and clone it;
- "This recovery is not verified live for application inference profiles."
In the README families table, the `Unknown` row: add "(use `BedrockModelBuilder::family` for an opaque ID)".

- [ ] **Step 3: Sweep**

Run: `git grep -n -i -E "opaque|application.inference.profile|BedrockModelBuilder::capabilities|\.capabilities\(" -- ':!target' ':!docs/superpowers'`
For each hit, decide whether it is now stale (says an opaque ID has no remedy, or sends users to `.capabilities(...)` for family-gated behavior) and correct it. List every hit and the decision in the task report.

- [ ] **Step 4: Verify**

Run each; all must pass:
`RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-providers-bedrock --no-deps`
`cargo test -p paigasus-helikon-providers-bedrock --all-features --doc`
`npx markdownlint-cli2 crates/paigasus-helikon-providers-bedrock/README.md docs/book/src/concepts/model-providers.md`
`mdbook build docs/book`

- [ ] **Step 5: Commit**

```bash
git add <the changed files, by explicit path>
git commit -m "docs(providers): SMA-837 document the bedrock family override"
```

### Final check (coordinator)

Run the full gate list from Global Constraints plus `cargo clippy --workspace --all-features --all-targets -- -D warnings`. Confirm the diff touches only the files above plus sweep hits.
