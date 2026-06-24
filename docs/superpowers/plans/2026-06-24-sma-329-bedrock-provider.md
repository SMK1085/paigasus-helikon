# Bedrock Provider (`paigasus-helikon-providers-bedrock`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a new crate `paigasus-helikon-providers-bedrock` — an Amazon Bedrock **Converse** `paigasus_helikon_core::Model` implementation — behind a facade `bedrock` feature, with a per-model-capable tool-schema rewriter and a wire-format test suite of equivalent depth to the OpenAI/Anthropic providers.

**Architecture:** Mirror the existing Anthropic provider's shape (builder → `Config` → `Model::invoke` returns a `BoxStream<ModelEvent>` produced by an `async_stream` wrapper over a streaming SDK call, fed through a pure `StreamTranslator`). Transport is the official `aws-sdk-bedrockruntime` SDK (`converse_stream`). All translation/schema/family/capability/error logic is pure and unit-tested directly; the SDK client is injected (DI) so no transport mock is needed.

**Tech Stack:** Rust, `aws-sdk-bedrockruntime` + `aws-config` (rustls), `aws-smithy-types::Document`, `async-trait`, `async-stream`, `futures-*`, `tokio`, `serde_json`, `insta` (snapshots), `thiserror`.

**Spec:** `docs/superpowers/specs/2026-06-24-sma-329-bedrock-provider-design.md` (GATE-1 approved). Read it before starting; this plan implements it.

## Global Constraints

- **MSRV = 1.91** (raised from 1.85 this PR — Task 1). Every crate inherits `rust-version.workspace`.
- **Workspace inheritance mandatory:** the new crate's `Cargo.toml` sets only `name`, `description`, `version = "0.1.0"`; everything else `*.workspace = true`. `[lints] workspace = true`.
- **`publish = true`, `version = "0.1.0"`** — a real crate, **not** a `0.0.0` stub. No `release = false` block. Name-claim pre-publish is a pre-merge step (see Release Checklist), not a code change.
- **Third-party deps pinned in root `[workspace.dependencies]`** (exact/caret — `deny.toml` has `wildcards = "deny"` for registry deps). Members reference via `dep.workspace = true`.
- **`missing_docs` is `-D warnings`:** every `pub` item (incl. the facade re-export) needs a `///` doc. Doc-coverage gate ≥ 80%.
- **Feature naming:** kebab in `[features]` (`bedrock`), snake in `pub use` alias (none needed — module alias is `bedrock`). Pair the facade `Cargo.toml` feature and `src/lib.rs` re-export.
- **Commit prefix:** `<type>(<scope>): SMA-329 <lowercase subject>`. Scopes seen here: `feat(providers-bedrock)`, `chore(workspace)` (MSRV), `docs(book)`/`docs(readme)`. Verify scope against `.versionrc` before committing (the local `commit-msg` hook enforces it).
- **Run `cargo fmt --all` + `cargo clippy --workspace --all-features --all-targets -- -D warnings` before every commit** (pre-commit hook is a no-op; pre-push catches it but late).
- **Commits are signed (1Password SSH).** If a commit fails with "failed to fill whole buffer", the vault is locked — ask Sven to unlock, don't bypass signing.
- **Worktree paths:** all file ops use the worktree-absolute root `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/feature+sma-329-providers-bedrock/`. Subagents must NOT run HEAD/branch-moving git (shared tree); only `git add`/`git commit` of their task's files.
- **Reference implementations to mirror** (read, don't reinvent): `crates/paigasus-helikon-providers-anthropic/src/{builder,model,error,stream,capabilities,lib}.rs` + `translate/{mod,request,tools,response_format}.rs`; `crates/paigasus-helikon-providers-openai/src/translate/tools.rs` (delegates to `paigasus_helikon_core::schema::strict`); the core contract `crates/paigasus-helikon-core/src/model.rs`.

---

## File Structure

```
crates/paigasus-helikon-providers-bedrock/
  Cargo.toml          # name/description/version=0.1.0; workspace-inherited; [lints] workspace=true
  README.md           # crates.io page (disambiguates vs runtime-agentcore)
  CHANGELOG.md        # Keep-a-Changelog: [Unreleased] + [0.1.0]
  src/
    lib.rs            # crate docs + pub use {BedrockModel, BedrockModelBuilder, BuildError, ModelFamily, Ruleset}
    family.rs         # ModelFamily enum + from_model_id()                     [Task 2, pure]
    document.rs       # value_to_document(&Value)->Document                    [Task 3, pure]
    capabilities.rs   # caps_for(ModelFamily)->(ModelCapabilities,u32)         [Task 5, pure]
    error.rs          # classify + map → ModelError                           [Task 6, pure-ish]
    stream.rs         # StreamTranslator::consume(event)->Vec<Result<ModelEvent,ModelError>>  [Task 10, pure]
    builder.rs        # BedrockModel::converse(), BedrockModelBuilder, Config, BuildError, from_env  [Task 11]
    model.rs          # BedrockModel: impl Model (invoke/cancel/provider/model/capabilities)  [Task 12]
    translate/
      mod.rs          # build_request() + to_wire_json() projection           [Task 9]
      request.rs      # items_to_messages() + alternating-turn discipline     [Task 7, pure]
      tools.rs        # tool_specs() (runs rewriter) + reserved-name guard     [Task 8, pure]
      response_format.rs  # synthesize_structured_output() + conflict guard   [Task 8, pure]
      schema.rs       # rewrite_tool_schema(&Value, Ruleset)->Value + Ruleset  [Task 4, pure — AC centerpiece]
      snapshots/      # insta .snap files
  tests/
    schema_rewriter.rs    # [Task 4]
    converse_request.rs   # wire-JSON projection snapshots [Task 9]
    converse_streaming.rs # translator unit tests [Task 10]
    structured_output.rs  # forced-tool synthesis [Task 8/9]
    cancellation.rs       # cancel mid-stream → no Finish [Task 12]
    live.rs               # env-gated [Task 12]
```

Modify: root `Cargo.toml`, `crates/paigasus-helikon/Cargo.toml`, `crates/paigasus-helikon/src/lib.rs`, `crates/paigasus-helikon/README.md`, root `README.md`, `docs/book/src/concepts/model-providers.md`, `.github/workflows/ci.yml`, `.github/workflows/msrv.yml` (if it hardcodes a version), `CLAUDE.md` (MSRV text), `deny.toml` (license allowlist if needed).

---

## Task 1: MSRV 1.91 bump + crate scaffold + AWS-SDK spike

**Files:**
- Modify: root `Cargo.toml` (`[workspace.package] rust-version`, `[workspace.dependencies]`, `[workspace] members`)
- Modify: `.github/workflows/ci.yml` (test matrix `1.85`→`1.91`), `.github/workflows/msrv.yml` (only if it hardcodes 1.85; it uses `rust-version` so likely no change — verify), `CLAUDE.md` (MSRV note), root `README.md` (MSRV badge/text if present)
- Create: `crates/paigasus-helikon-providers-bedrock/Cargo.toml`, `src/lib.rs` (skeleton with empty modules), `README.md`, `CHANGELOG.md`
- Modify: `crates/paigasus-helikon/Cargo.toml` (optional dep + `bedrock` feature), `crates/paigasus-helikon/src/lib.rs` (re-export)
- Modify: `deny.toml` (only if license check requires)

**Interfaces — Produces:** a workspace that builds on 1.91 with an empty `paigasus-helikon-providers-bedrock` behind the `bedrock` feature; the verified `ConverseStreamOutput` taxonomy (recorded into the spec §6 and this plan's Task 10).

- [ ] **Step 1: Bump MSRV.** In root `Cargo.toml` set `[workspace.package] rust-version = "1.91"`. In `.github/workflows/ci.yml`, change every `1.85` in the `test` job matrix to `"1.91"` (keep the `stable` entries). Grep `1.85` across `.github/` + `CLAUDE.md` + `README.md` and update each MSRV mention to `1.91`. Verify `msrv.yml` runs `cargo msrv --path crates/paigasus-helikon-core verify` (no `--workspace`) and needs no version edit (it reads `rust-version`).

- [ ] **Step 2: Confirm local toolchain ≥ 1.91.** Run `rustc --version`. If < 1.91: `rustup toolchain install 1.91` and use `cargo +1.91` for all build/test commands in this plan. Record which toolchain is the default.

- [ ] **Step 3: Add AWS deps to `[workspace.dependencies]` (rustls).** Resolve the latest compatible versions (`cargo info aws-sdk-bedrockruntime` etc.) and add, configured onto rustls with default features off:
```toml
aws-config             = { version = "1", default-features = false, features = ["rustls", "rt-tokio", "credentials-process", "sso"] }
aws-sdk-bedrockruntime = { version = "1", default-features = false, features = ["rt-tokio"] }
aws-smithy-types       = "1"
aws-smithy-runtime-api = "1"          # for SdkError / event-stream types in error.rs + stream.rs
```
Pin the exact resolved versions (replace `"1"` with the concrete `"1.x"` after `cargo update`). **Note:** confirm the rustls feature names against the resolved crates (`aws-config` exposes a `rustls` / `client-hyper-rustls`-style feature — use whatever the resolved version names it; the goal is no `aws-lc-sys`). Add the new crate to `[workspace] members` and `[workspace.dependencies]`:
```toml
paigasus-helikon-providers-bedrock = { path = "crates/paigasus-helikon-providers-bedrock", version = "0.1.0" }
```

- [ ] **Step 4: Scaffold the crate.** Create `crates/paigasus-helikon-providers-bedrock/Cargo.toml`:
```toml
[package]
name        = "paigasus-helikon-providers-bedrock"
description = "Amazon Bedrock (Converse API) provider for the Paigasus Helikon AI SDK."
version                = "0.1.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true

[dependencies]
paigasus-helikon-core  = { workspace = true }
aws-config             = { workspace = true }
aws-sdk-bedrockruntime = { workspace = true }
aws-smithy-types       = { workspace = true }
aws-smithy-runtime-api = { workspace = true }
async-trait            = { workspace = true }
async-stream           = { workspace = true }
futures-core           = { workspace = true }
futures-util           = { workspace = true }
serde                  = { workspace = true }
serde_json             = { workspace = true }
thiserror              = { workspace = true }
anyhow                 = { workspace = true }
tokio                  = { workspace = true }
tracing                = { workspace = true }

[dev-dependencies]
insta = { workspace = true, features = ["json", "yaml"] }
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time"] }

[lints]
workspace = true
```
Create `src/lib.rs` (skeleton — modules added empty so it compiles):
```rust
//! Amazon Bedrock (Converse API) provider for the Paigasus Helikon SDK.
//!
//! The public surface is [`BedrockModel`] (a [`paigasus_helikon_core::Model`])
//! and its [`BedrockModelBuilder`]. This is the Bedrock **Converse model
//! provider** — distinct from the `runtime-agentcore` crate (the Bedrock
//! *AgentCore runtime*).
//!
//! ```ignore
//! use paigasus_helikon_providers_bedrock::BedrockModel;
//! # async fn f() -> Result<(), Box<dyn std::error::Error>> {
//! let _model = BedrockModel::from_env("anthropic.claude-3-5-sonnet-20241022-v2:0").await?;
//! # Ok(()) }
//! ```
mod builder;
mod capabilities;
mod document;
mod error;
mod family;
mod model;
mod stream;
mod translate;

pub use builder::{BedrockModelBuilder, BuildError};
pub use family::ModelFamily;
pub use model::BedrockModel;
pub use translate::schema::Ruleset;
```
Create minimal empty module files so it compiles (each with a `//!` doc line). Create `README.md` and `CHANGELOG.md` (full content lands in Task 13; for now a one-line description + `[Unreleased]`).

- [ ] **Step 5: Wire the facade.** In `crates/paigasus-helikon/Cargo.toml` add `paigasus-helikon-providers-bedrock = { workspace = true, optional = true }` and `bedrock = ["dep:paigasus-helikon-providers-bedrock"]`. In `crates/paigasus-helikon/src/lib.rs` add:
```rust
/// Bedrock provider (Converse model). Enabled via the `bedrock` feature.
/// Distinct from `runtime-agentcore` (the Bedrock AgentCore runtime).
#[cfg(feature = "bedrock")]
pub use paigasus_helikon_providers_bedrock as bedrock;
```

- [ ] **Step 6: Spike — build on MSRV + record the real SDK taxonomy.** Run `cargo +1.91 build -p paigasus-helikon-providers-bedrock` and `cargo +1.91 build -p paigasus-helikon --features bedrock`. Then capture the **actual** `ConverseStreamOutput` enum + its event structs and the `TokenUsage` struct from the resolved SDK (`cargo doc -p aws-sdk-bedrockruntime --open`, or read `~/.cargo/registry/.../aws-sdk-bedrockruntime-*/src/types/`). Paste the real variant names, the location of `content_block_index`, the tool-use start id/name accessors, the delta `tool_use().input()` accessor, the reasoning-content delta shape, and whether `TokenUsage` has `cache_read_input_tokens` into Task 10's reference block (and correct spec §6 if it differs).

- [ ] **Step 7: License/deny gate.** Run `cargo deny check licenses` and `cargo deny check bans`. If a transitive AWS crate carries a non-allowlisted license, add it to `deny.toml`'s `[licenses] allow` with a one-line justification comment (matching the existing style). Confirm no `aws-lc-sys` in the tree (`cargo tree -p paigasus-helikon-providers-bedrock | grep -i aws-lc` returns nothing); if present, fix the rustls feature flags. Run `cargo deny check` clean across targets.

- [ ] **Step 8: Verify + commit.** Run `cargo +1.91 build --workspace --all-features`, `cargo fmt --all`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`.
```bash
git add Cargo.toml Cargo.lock .github/ CLAUDE.md README.md deny.toml crates/paigasus-helikon-providers-bedrock crates/paigasus-helikon/Cargo.toml crates/paigasus-helikon/src/lib.rs
git commit -m "chore(workspace): SMA-329 raise MSRV to 1.91 and scaffold bedrock provider crate"
```
> Split note: if you prefer, commit the MSRV bump (`chore(workspace): SMA-329 raise MSRV to 1.91`) separately from the scaffold (`feat(providers-bedrock): SMA-329 scaffold crate + facade wiring`). Two commits are fine.

---

## Task 2: `family.rs` — model-family detection

**Files:** Create `src/family.rs`; tests inline (`#[cfg(test)]`).

**Interfaces — Produces:**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelFamily { Anthropic, AmazonNova, AmazonTitan, Llama, Mistral, Cohere, Unknown }
impl ModelFamily {
    pub fn from_model_id(model_id: &str) -> Self;
    pub(crate) fn supports_forced_tool_choice(self) -> bool; // Anthropic|Mistral|AmazonNova => true
}
```

- [ ] **Step 1: Failing tests.**
```rust
#[cfg(test)]
mod tests {
    use super::ModelFamily::*;
    use super::*;
    #[test]
    fn detects_families_from_bedrock_model_ids() {
        for (id, want) in [
            ("anthropic.claude-3-5-sonnet-20241022-v2:0", Anthropic),
            ("us.anthropic.claude-3-7-sonnet-20250219-v1:0", Anthropic), // cross-region inference profile prefix
            ("amazon.nova-pro-v1:0", AmazonNova),
            ("amazon.titan-text-express-v1", AmazonTitan),
            ("meta.llama3-1-70b-instruct-v1:0", Llama),
            ("mistral.mistral-large-2407-v1:0", Mistral),
            ("cohere.command-r-plus-v1:0", Cohere),
            ("some.future-model", Unknown),
        ] {
            assert_eq!(ModelFamily::from_model_id(id), want, "id={id}");
        }
    }
    #[test]
    fn forced_tool_choice_support() {
        assert!(Anthropic.supports_forced_tool_choice());
        assert!(Mistral.supports_forced_tool_choice());
        assert!(AmazonNova.supports_forced_tool_choice());
        assert!(!Llama.supports_forced_tool_choice());
        assert!(!AmazonTitan.supports_forced_tool_choice());
    }
}
```
- [ ] **Step 2: Run — expect FAIL** (`cargo test -p paigasus-helikon-providers-bedrock family`).
- [ ] **Step 3: Implement.** Match on the provider segment; strip a leading cross-region prefix (`us.`/`eu.`/`apac.`) before matching. `from_model_id` lowercases and inspects the first dotted segment after any region prefix: `anthropic`→Anthropic, `amazon`+`nova`→AmazonNova, `amazon`+`titan`→AmazonTitan, `meta`/`*llama*`→Llama, `mistral`→Mistral, `cohere`→Cohere, else Unknown. `supports_forced_tool_choice` returns true for Anthropic/Mistral/AmazonNova.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Commit** `feat(providers-bedrock): SMA-329 add model-family detection`.

---

## Task 3: `document.rs` — `serde_json::Value` → `aws_smithy_types::Document`

**Files:** Create `src/document.rs`; tests inline.

**Interfaces — Produces:** `pub(crate) fn value_to_document(v: &serde_json::Value) -> aws_smithy_types::Document;`

- [ ] **Step 1: Failing tests** (number edges are the point):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_types::Document;
    use serde_json::json;
    #[test] fn null_bool_string() {
        assert!(matches!(value_to_document(&json!(null)), Document::Null));
        assert!(matches!(value_to_document(&json!(true)), Document::Bool(true)));
        assert!(matches!(value_to_document(&json!("x")), Document::String(s) if s == "x"));
    }
    #[test] fn positive_negative_and_float_numbers() {
        use aws_smithy_types::Number;
        assert!(matches!(value_to_document(&json!(7u64)), Document::Number(Number::PosInt(7))));
        assert!(matches!(value_to_document(&json!(-7i64)), Document::Number(Number::NegInt(-7))));
        assert!(matches!(value_to_document(&json!(1.5f64)), Document::Number(Number::Float(f)) if (f-1.5).abs()<f64::EPSILON));
    }
    #[test] fn u64_above_i64_max_stays_posint() {
        use aws_smithy_types::Number;
        let big = json!(u64::MAX);
        assert!(matches!(value_to_document(&big), Document::Number(Number::PosInt(n)) if n == u64::MAX));
    }
    #[test] fn nested_object_and_empty_array() {
        let d = value_to_document(&json!({"a":[1], "b":{}}));
        let Document::Object(m) = d else { panic!() };
        assert!(matches!(m.get("a"), Some(Document::Array(a)) if a.len()==1));
        assert!(matches!(m.get("b"), Some(Document::Object(o)) if o.is_empty()));
    }
}
```
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** Recurse: `Null`→`Document::Null`; `Bool`→`Document::Bool`; `String`→`Document::String`; `Number`→ if `as_u64()` `PosInt`, else if `as_i64()` `NegInt`, else `as_f64()` `Float` (if all `None` under `arbitrary_precision`, fall back to `Float(0.0)` is wrong — instead parse via `n.as_f64()`; document that `arbitrary_precision` is not enabled in this workspace); `Array`→`Document::Array(map)`; `Object`→`Document::Object` (a `aws_smithy_types::Document` object is a `HashMap<String, Document>`).
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Commit** `feat(providers-bedrock): SMA-329 add Value→Document adapter`.

---

## Task 4: `translate/schema.rs` — the schema rewriter (AC centerpiece)

**Files:** Create `src/translate/schema.rs`; create `src/translate/mod.rs` with `pub mod schema;` (the rest of `mod.rs` lands in Task 9); integration tests `tests/schema_rewriter.rs`; snapshots under `src/translate/snapshots/`.

**Interfaces — Produces:**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)] #[non_exhaustive]
pub enum Ruleset { Strict }
impl Ruleset { pub fn for_family(_f: crate::ModelFamily) -> Self { Ruleset::Strict } }
pub fn rewrite_tool_schema(schema: &serde_json::Value, ruleset: Ruleset) -> serde_json::Value;
```

> **Do NOT call `paigasus_helikon_core::schema::strict`** — see spec §4.0. This is a Bedrock-specific transform: inline `$ref`, collapse `oneOf`/`anyOf`/`allOf`, strip keywords. It does NOT add `additionalProperties:false` and does NOT promote `required`.

- [ ] **Step 1: Failing unit tests for `$ref` inlining** (inline in `schema.rs`):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn strict(v: serde_json::Value) -> serde_json::Value { rewrite_tool_schema(&v, Ruleset::Strict) }
    fn has_key_anywhere(v: &serde_json::Value, key: &str) -> bool {
        match v {
            serde_json::Value::Object(m) => m.contains_key(key) || m.values().any(|c| has_key_anywhere(c, key)),
            serde_json::Value::Array(a) => a.iter().any(|c| has_key_anywhere(c, key)),
            _ => false,
        }
    }
    #[test] fn inlines_defs_ref() {
        let input = json!({
            "type":"object",
            "properties": {"inner": {"$ref":"#/$defs/Inner"}},
            "$defs": {"Inner": {"type":"object","properties":{"x":{"type":"string"}}}}
        });
        let out = strict(input);
        assert!(!has_key_anywhere(&out, "$ref"));
        assert!(!has_key_anywhere(&out, "$defs"));
        assert_eq!(out["properties"]["inner"]["properties"]["x"]["type"], json!("string"));
    }
    #[test] fn inlines_ref_inside_items_and_chained_refs() {
        let input = json!({
            "type":"object",
            "properties": {"list": {"type":"array","items":{"$ref":"#/$defs/A"}}},
            "$defs": {"A": {"type":"object","properties":{"b":{"$ref":"#/$defs/B"}}},
                      "B": {"type":"object","properties":{"v":{"type":"integer"}}}}
        });
        let out = strict(input);
        assert!(!has_key_anywhere(&out, "$ref"));
        assert_eq!(out["properties"]["list"]["items"]["properties"]["b"]["properties"]["v"]["type"], json!("integer"));
    }
    #[test] fn ref_with_sibling_keywords_merges() {
        let input = json!({
            "type":"object",
            "properties": {"p": {"$ref":"#/$defs/T","description":"doc"}},
            "$defs": {"T": {"type":"string"}}
        });
        let out = strict(input);
        assert_eq!(out["properties"]["p"]["type"], json!("string"));
        assert_eq!(out["properties"]["p"]["description"], json!("doc"));
    }
    #[test] fn unresolvable_external_ref_becomes_permissive_object() {
        let input = json!({"type":"object","properties":{"p":{"$ref":"https://example/x"}}});
        let out = strict(input);
        assert!(!has_key_anywhere(&out, "$ref"));
        assert_eq!(out["properties"]["p"], json!({"type":"object"}));
    }
    #[test] fn recursive_type_terminates_and_is_idempotent() {
        let input = json!({
            "type":"object",
            "properties":{"child":{"$ref":"#/$defs/Node"}},
            "$defs":{"Node":{"type":"object","properties":{"child":{"$ref":"#/$defs/Node"}}}}
        });
        let once = strict(input.clone());
        let twice = rewrite_tool_schema(&once, Ruleset::Strict);
        assert!(!has_key_anywhere(&once, "$ref"));
        assert_eq!(once, twice, "rewrite must be idempotent on recursive types");
    }
    #[test] fn collapses_tagged_enum_oneof() {
        // serde adjacently-tagged: {"t": "A"|"B", "c": payload}
        let input = json!({
            "oneOf": [
                {"type":"object","properties":{"t":{"const":"A"},"c":{"type":"object","properties":{"a":{"type":"string"}}}}},
                {"type":"object","properties":{"t":{"const":"B"},"c":{"type":"object","properties":{"b":{"type":"integer"}}}}}
            ]
        });
        let out = strict(input);
        assert!(!has_key_anywhere(&out, "oneOf"));
        assert!(!has_key_anywhere(&out, "anyOf"));
        assert!(!has_key_anywhere(&out, "allOf"));
        assert_eq!(out["type"], json!("object"));
        // tag became an enum of variant tags; properties non-empty
        assert_eq!(out["properties"]["t"]["enum"], json!(["A","B"]));
        assert!(out["properties"].as_object().unwrap().len() >= 1);
        assert!(out.get("required").is_none(), "Strict must not promote required");
        assert!(out.get("additionalProperties").is_none(), "Strict must not inject additionalProperties");
    }
    #[test] fn strips_unsupported_keywords() {
        let input = json!({"type":"object","$schema":"...","$id":"x","format":"email","examples":[1],
            "properties":{"p":{"type":"string","format":"uri"}}});
        let out = strict(input);
        for k in ["$schema","$id","format","examples"] { assert!(!has_key_anywhere(&out, k), "{k} not stripped"); }
        assert_eq!(out["properties"]["p"]["type"], json!("string"));
    }
}
```
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement.** A struct `Rewriter<'a>{ defs: &'a Map, chain: Vec<&str>, depth: usize }`. Entry `rewrite_tool_schema`: extract top-level `$defs`/`definitions` into a borrowed map, then `rewrite_node(root)` with a fresh chain, then ensure the orphaned `$defs`/`definitions` keys are removed from the output root. `rewrite_node`:
  1. If object has `$ref`: resolve. For `#/$defs/X` or `#/definitions/X`, look up; if found, clone target, recurse, then merge any sibling keys (siblings win); push to `chain`/check cycle+`MAX_DEPTH`(64) → on cycle/over-depth return `{"type":"object"}`. For external/unresolvable → return `{"type":"object"}`.
  2. Else if object has `oneOf`/`anyOf`/`allOf`: collect candidate variant objects (rewrite each first), build merged object: detect a shared tag key (a property present in all variants whose schema is `const`/single-`enum` string) → emit `{"type":"string","enum":[tags…]}` for it; union the other properties (rewrite each, first-wins on key collision); result `{"type":"object","properties":<union>}`; if union empty → `{"type":"object"}`. Drop the combinator key + everything else on the node.
  3. Else: strip unsupported keys (`$schema`,`$id`,`$anchor`,`format`,`examples`,`default`,`$comment`), then recurse into `properties.*`, `items` (object or array elems), `additionalProperties` (if object).
  Keep `MAX_DEPTH` as a `const`. Make the function total (never panic).
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Snapshot integration tests** `tests/schema_rewriter.rs`: a realistic `schemars`-style adjacently-tagged enum + deeply-nested-generic schema (build the `json!` inputs), `insta::assert_json_snapshot!(rewrite_tool_schema(&input, Ruleset::Strict))`. Run `cargo insta test --review` (accept), or `INSTA_UPDATE=always cargo test`. Commit the `.snap` files.
- [ ] **Step 6: fmt + clippy + Commit** `feat(providers-bedrock): SMA-329 add Bedrock tool-schema rewriter`.

---

## Task 5: `capabilities.rs` — family → capabilities table

**Files:** Create `src/capabilities.rs`; tests inline.

**Interfaces — Produces:** `pub(crate) fn caps_for(family: crate::ModelFamily) -> (paigasus_helikon_core::ModelCapabilities, u32);` (caps + max_output default).

- [ ] **Step 1: Failing tests** — assert Anthropic has `streaming+tools+parallel+structured+vision`, Llama has `streaming+tools` but NOT `structured_output`, Unknown has `streaming+tools` only, and each `max_output` default is > 0.
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** with `ModelCapabilities::empty().with_streaming().with_tools()…` per family (mirror `anthropic/src/capabilities.rs` style). Structured-output flag = `family.supports_forced_tool_choice()`. Conservative `Unknown` = streaming+tools, max_output 4096.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Commit** `feat(providers-bedrock): SMA-329 add capability table`.

---

## Task 6: `error.rs` — error mapping

**Files:** Create `src/error.rs`; tests inline.

**Interfaces — Produces:**
```rust
// Map any bedrockruntime SdkError to ModelError. Keep the public-ish surface a small
// classifier on (Option<&str> code, Option<u16> status, &str message, Option<u64> retry_after_ms)
// so it is unit-testable WITHOUT constructing real SdkError values.
pub(crate) fn classify(code: Option<&str>, status: Option<u16>, message: &str, retry_after_ms: Option<u64>) -> paigasus_helikon_core::ModelError;
pub(crate) fn map_sdk_error<E: std::fmt::Debug, R>(err: aws_smithy_runtime_api::client::result::SdkError<E, R>) -> paigasus_helikon_core::ModelError;
```

- [ ] **Step 1: Failing tests for `classify`** — table: `ThrottlingException`→`RateLimited`; `ServiceUnavailableException`/`ModelNotReadyException`/`InternalServerException`/`ModelStreamErrorException`/`ModelTimeoutException`→`Unavailable`; `AccessDeniedException`→`Refused`; `ValidationException` + message containing "too long"/"maximum context"→`ContextLengthExceeded`; other `ValidationException`→`Other`; unknown→`Other`. (Assert via `matches!`.)
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** `classify` (string-match the code; ctx-length string-match like `anthropic/src/error.rs`). Implement `map_sdk_error` by extracting the modeled error code/metadata from `SdkError` (`.code()` via `ProvideErrorMetadata` when `ServiceError`; `DispatchFailure`/`TimeoutError`/`ConstructionFailure`/`ResponseError`→`Transport(format!("{err:?}"))`) and delegating to `classify`. Parse `Retry-After`/`x-amzn-…` from response headers when reachable.
- [ ] **Step 4: Run — expect PASS.**
- [ ] **Step 5: Commit** `feat(providers-bedrock): SMA-329 add error mapping`.

---

## Task 7: `translate/request.rs` — items → Converse messages

**Files:** Create `src/translate/request.rs`; tests inline + contribute to `tests/converse_request.rs` (Task 9).

**Interfaces — Produces:**
```rust
pub(crate) struct TranslatedMessages { pub system: Vec<SystemContentBlock>, pub messages: Vec<Message> }
pub(crate) fn items_to_messages(items: &[paigasus_helikon_core::Item]) -> Result<TranslatedMessages, BuildErrorOrModelError>;
// plus a serde_json::Value projection helper for snapshots: messages_to_wire_json(&TranslatedMessages) -> Value
```
(Use the SDK `aws_sdk_bedrockruntime::types::{Message, SystemContentBlock, ContentBlock, ToolResultBlock, ToolUseBlock, ConversationRole}`.)

- [ ] **Step 1: Read** `anthropic/src/translate/request.rs` for the flush/queue discipline. **Read** `core/src/item.rs` for the `Item` variants (System/UserMessage/AssistantMessage/ToolCall/ToolResult — confirm exact names/fields).
- [ ] **Step 2: Failing tests** (assert on the `messages_to_wire_json` projection): system items collect into `system`; a user→assistant→tool_call→tool_result→user sequence yields strictly alternating roles with the `toolResult` in the user turn after the `toolUse`; adjacent same-role items merge; a **leading assistant** item gets a synthesized/handled user turn; an **empty** conversation returns an error. Use `assert_json_snapshot!` for the happy path and explicit asserts for the structural rules.
- [ ] **Step 3: Run — expect FAIL.**
- [ ] **Step 4: Implement** the flush/queue port: maintain pending user/assistant content vecs, flush on role switch; map text→`ContentBlock::Text`, tool call→`ContentBlock::ToolUse(ToolUseBlock{tool_use_id,name,input: value_to_document(args)})`, tool result→`ContentBlock::ToolResult(ToolResultBlock{tool_use_id,content})` queued onto the next user turn; enforce first-turn-user (synthesize empty user turn if the convo leads with assistant) and non-empty conversation.
- [ ] **Step 5: Run — expect PASS; review the snapshot.**
- [ ] **Step 6: Commit** `feat(providers-bedrock): SMA-329 translate items to Converse messages`.

---

## Task 8: `translate/tools.rs` + `translate/response_format.rs`

**Files:** Create both; tests inline + `tests/structured_output.rs`.

**Interfaces — Produces:**
```rust
// tools.rs
pub(crate) const SYNTHESIZED_TOOL_NAME: &str = "__paigasus_structured_output__";
pub(crate) fn tool_specs(defs: &[paigasus_helikon_core::ToolDef], ruleset: Ruleset) -> Result<Vec<ToolSpecification>, ModelError>; // errors on reserved-name collision
// response_format.rs
pub(crate) struct Synthesized { pub tool: Option<ToolSpecification>, pub tool_choice: Option<ToolChoice>, pub synthesizing: bool }
pub(crate) fn synthesize(rf: Option<&ResponseFormat>, family: ModelFamily, ruleset: Ruleset) -> Result<Synthesized, ModelError>; // errors on JsonSchema+ToolChoice::Tool conflict (conflict checked in mod.rs where tool_choice is known)
```

- [ ] **Step 1: Read** `anthropic/src/translate/{tools,response_format}.rs`.
- [ ] **Step 2: Failing tests:** `tool_specs` runs each `ToolDef.schema` through `rewrite_tool_schema` and builds `ToolSpecification{name,description,input_schema:ToolInputSchema::Json(value_to_document(rewritten))}`; a user tool named `__paigasus_structured_output__` → `Err`. `synthesize`: `JsonSchema` on a forced-tool-capable family → `Some(tool)` + `tool_choice: Tool{name}` + `synthesizing=true`; on Llama/Titan → `synthesizing=false`, no tool, (caller degrades to Text).
- [ ] **Step 3: Run — FAIL → implement → PASS.** Mirror the Anthropic synthesis (reserved name + the schema becomes the synthesized tool's `input_schema`).
- [ ] **Step 4: Commit** `feat(providers-bedrock): SMA-329 translate tools + structured-output synthesis`.

---

## Task 9: `translate/mod.rs` — assemble the Converse request + wire projection

**Files:** Fill `src/translate/mod.rs`; tests `tests/converse_request.rs`.

**Interfaces — Produces:**
```rust
pub(crate) struct PreparedConverse { /* model_id, system, messages, tool_config: Option<ToolConfiguration>, inference_config, synthesizing: bool */ }
pub(crate) fn build_request(cfg: &crate::builder::Config, req: &ModelRequest) -> Result<PreparedConverse, ModelError>;
pub fn to_wire_json(p: &PreparedConverse) -> serde_json::Value; // snapshot projection — wire-stable, NOT SDK Debug
```

- [ ] **Step 1: Failing tests** — `assert_json_snapshot!(to_wire_json(build_request(cfg, &req)?))` for: plain text turn; tool call + result; structured-output (JsonSchema) on a Claude-family cfg (shows synthesized tool + `toolChoice`); `tool_choice` Auto/Required/Tool mapping; `temperature`/`top_p`/`max_tokens` in `inferenceConfig`; an unsupported-family JsonSchema degrades to no synthesis. Conflict guard: `JsonSchema` + `ToolChoice::Tool` → `Err`.
- [ ] **Step 2: Run — FAIL.**
- [ ] **Step 3: Implement** — call `items_to_messages`, `synthesize`, `tool_specs`; merge synthesized tool into the tool list; map `ModelSettings.tool_choice`→Converse `ToolChoice` (only if `family.supports_forced_tool_choice()`, else omit + `debug!`); build `inferenceConfig`; assemble `PreparedConverse`. Implement `to_wire_json` as a hand-written `serde_json::json!` projection of the prepared parts (so snapshots are SDK-`Debug`-independent).
- [ ] **Step 4: Run — PASS; review snapshots.**
- [ ] **Step 5: Commit** `feat(providers-bedrock): SMA-329 assemble Converse request + wire projection`.

---

## Task 10: `stream.rs` — Converse stream → ModelEvent translator

**Files:** Create `src/stream.rs`; tests `tests/converse_streaming.rs`.

> **Use the real SDK taxonomy captured in Task 1 Step 6.** Reference (verify against the pinned SDK): `aws_sdk_bedrockruntime::types::ConverseStreamOutput::{MessageStart, ContentBlockStart, ContentBlockDelta, ContentBlockStop, MessageStop, Metadata, Unknown}`; events expose `content_block_index()`; `ContentBlockStart.start()` → `ContentBlockStart::ToolUse(ToolUseBlockStart{ tool_use_id, name })`; `ContentBlockDelta.delta()` → `ContentBlockDelta::{Text(String), ToolUse(ToolUseBlockDelta{ input }), ReasoningContent(...)}`; `Metadata.usage()` → `TokenUsage{ input_tokens, output_tokens, cache_read_input_tokens? }`; `MessageStop.stop_reason()` → `StopReason`.

**Interfaces — Produces:**
```rust
#[derive(Default)] pub(crate) struct StreamTranslator { /* index→call_id map, synthesizing flag, real_tool_fired, ... */ }
impl StreamTranslator {
    pub(crate) fn new(synthesizing: bool) -> Self;
    pub(crate) fn consume(&mut self, ev: aws_sdk_bedrockruntime::types::ConverseStreamOutput)
        -> Vec<Result<paigasus_helikon_core::ModelEvent, paigasus_helikon_core::ModelError>>;
}
```

- [ ] **Step 1: Failing tests** (construct events via SDK builders, e.g. `ConverseStreamOutput::ContentBlockDelta(ContentBlockDeltaEvent::builder().delta(ContentBlockDelta::Text("hi".into())).content_block_index(0).build())`):
  - text-only: ContentBlockDelta(Text "Hel"), (Text "lo"), MessageStop(end_turn), Metadata(usage) → `[TokenDelta "Hel", TokenDelta "lo", Usage{..}, Finish(Stop)]` (order: emit Usage when Metadata seen, Finish when MessageStop — confirm Bedrock emits Metadata after MessageStop; if MessageStop precedes Metadata, hold the finish until Metadata then emit Usage, Finish — replicate the spec's "Usage before Finish" by buffering the finish reason).
  - parallel tool calls: two ContentBlockStart(ToolUse) at index 0 and 1, interleaved ToolUse input deltas → two `ToolCallDelta{name:Some}` then `ToolCallDelta{name:None,args_delta}` with correct `call_id`s; MessageStop(tool_use) → `Finish(ToolCalls)`.
  - reasoning: ContentBlockDelta(ReasoningContent) → `ReasoningDelta`.
  - max tokens: MessageStop(max_tokens) → `Finish(Length)`.
  - content filter: MessageStop(guardrail_intervened) → `Finish(ContentFilter)`.
  - synthesizing: with `new(true)`, a ToolUse-input delta for the synthesized tool → `TokenDelta` (not ToolCallDelta).
  - both-tools-fired: synthesized tool + a real tool both start → on MessageStop, a `ModelError::Other`.
  - `Unknown` event → no output.
- [ ] **Step 2: Run — FAIL.**
- [ ] **Step 3: Implement** the state machine per spec §6 table; buffer the `stop_reason` and emit `Usage` (from Metadata) before `Finish`. Maintain `HashMap<i32,String>` index→call_id; track whether a real (non-synth) tool fired; in synthesizing mode remap tool-input deltas to `TokenDelta`.
- [ ] **Step 4: Run — PASS.**
- [ ] **Step 5: Commit** `feat(providers-bedrock): SMA-329 add Converse stream translator`.

---

## Task 11: `builder.rs` — builder, Config, BuildError, from_env

**Files:** Create `src/builder.rs`; tests inline.

**Interfaces — Produces:**
```rust
#[derive(Debug, thiserror::Error)] #[non_exhaustive]
pub enum BuildError { #[error("no AWS client or SdkConfig provided")] MissingClient, #[error("model id is empty")] EmptyModelId }
pub struct BedrockModelBuilder { /* model_id, client?, sdk_config?, region?, caps_override?, max_out_override? */ }
#[derive(Clone)] pub(crate) struct Config { pub client: aws_sdk_bedrockruntime::Client, pub model_id: String, pub family: crate::ModelFamily, pub capabilities: ModelCapabilities, pub max_output_default: u32 }
impl BedrockModelBuilder {
    pub fn client(self, c: aws_sdk_bedrockruntime::Client) -> Self;
    pub fn sdk_config(self, c: &aws_config::SdkConfig) -> Self;
    pub fn region(self, r: impl Into<aws_config::Region>) -> Self;
    pub fn capabilities(self, c: ModelCapabilities) -> Self;
    pub fn max_output_tokens_default(self, n: u32) -> Self;
    pub fn build(self) -> Result<crate::BedrockModel, BuildError>;
}
impl crate::BedrockModel {
    pub fn converse(model_id: impl Into<String>) -> BedrockModelBuilder;
    pub async fn from_env(model_id: impl Into<String>) -> Result<Self, BuildError>;
}
```

- [ ] **Step 1: Failing tests** (no network): `converse("").build()` → `Err(EmptyModelId)`; `converse("anthropic.claude…").build()` with no client/config → `Err(MissingClient)`; injecting a `Client` (build one offline via `aws_sdk_bedrockruntime::Client::from_conf(...)` with a dummy region + static no-op creds, or via `aws_config::SdkConfig::builder()`) → `Ok`; `.region()` ignored when a client is injected (assert via a `debug!`-path or by documenting — at least assert build succeeds and the injected client is used). Capabilities/max-output come from `caps_for(family)` unless overridden.
- [ ] **Step 2: Run — FAIL.**
- [ ] **Step 3: Implement.** `build()`: validate non-empty id → resolve family → resolve client (injected `client` wins; else `Client::new(sdk_config)`; else `MissingClient`) → `caps_for` with overrides → `Config` → `BedrockModel(Arc<Config>)`. `from_env`: `aws_config::defaults(<pinned dated BehaviorVersion>).region(region_or_default).load().await` → `Client::new` → reuse `build()` logic. Doc-comment on `from_env`: **the AWS credential chain is lazy — credential/auth failures surface at `invoke()` as `ModelError`, not here.**
- [ ] **Step 4: Run — PASS.**
- [ ] **Step 5: Commit** `feat(providers-bedrock): SMA-329 add builder + from_env`.

---

## Task 12: `model.rs` — `impl Model` + cancellation + descriptors

**Files:** Create `src/model.rs`; tests `tests/cancellation.rs` + `tests/live.rs`.

**Interfaces — Produces:** `pub struct BedrockModel(Arc<Config>);` implementing `paigasus_helikon_core::Model`.

- [ ] **Step 1: Implement `invoke`** — build `PreparedConverse` via `build_request` (return early `Err(ModelError)` on translate error); construct the `converse_stream` fluent call from the prepared parts; `.send().await` → on `Err`, `map_sdk_error`. Wrap the resulting `EventReceiver` in `async_stream::stream!` that loops `tokio::select!{ _ = cancel.cancelled() => break, ev = receiver.recv() => match ev { Ok(Some(e)) => for m in translator.consume(e) { yield m }, Ok(None) => break, Ok-finish handling, Err(e) => { yield Err(map_stream_error(e)); break } } }`. `capabilities()`/`provider()`("bedrock")/`model()` from `Config`. Box-pin the stream as `BoxStream<'static, …>`.
- [ ] **Step 2: Cancellation test** `tests/cancellation.rs` — this needs a stream to cancel. Since we don't mock the transport, test the **cancellation wrapper** in isolation: extract the select-loop into a small `pub(crate) fn drive_stream(receiver, translator, cancel) -> impl Stream` and unit-test it by feeding a hand-rolled async source (a `futures::stream` of constructed events behind a channel) and asserting that firing `cancel` ends the stream with **no** `Finish`. (If extraction is impractical, assert the property via a `StreamTranslator`-level test that the loop emits no synthetic Finish on early break.)
- [ ] **Step 3: Descriptor + capability tests** — `model.provider() == "bedrock"`, `model.model() == id`, `model.capabilities()` matches `caps_for(family)`. Build the model with an injected offline client.
- [ ] **Step 4: `tests/live.rs`** — env-gated on `AWS_*` creds + `BEDROCK_MODEL_ID`; loud-`eprintln!`-skip + early-return when unset (mirror `anthropic/tests/live.rs`). When set: `from_env(model_id).await?`, send a one-turn request with one tool whose schema is a tagged enum + nested generic, drive the stream, assert it completes without a transport error (validates the §4.2 acceptance half).
- [ ] **Step 5: Run** `cargo test -p paigasus-helikon-providers-bedrock` (live loud-skips). fmt + clippy.
- [ ] **Step 6: Commit** `feat(providers-bedrock): SMA-329 implement BedrockModel (invoke + cancellation)`.

---

## Task 13: Documentation (crate + facade + root + mdBook)

**Files:** `crates/paigasus-helikon-providers-bedrock/{README.md,CHANGELOG.md}`; `crates/paigasus-helikon/README.md`; root `README.md`; `docs/book/src/concepts/model-providers.md`.

- [ ] **Step 1: Crate README** — mirror `anthropic/README.md`: title, one-paragraph description (**Converse model provider — distinct from `runtime-agentcore`, the Bedrock AgentCore runtime**), `cargo add paigasus-helikon-providers-bedrock` (no version), an `ignore`-fenced `from_env` example, links, `Apache-2.0 OR MIT`.
- [ ] **Step 2: CHANGELOG** — Keep-a-Changelog with `## [Unreleased]` and `## [0.1.0] - 2026-06-24` (Added: initial Bedrock Converse provider).
- [ ] **Step 3: Facade README + root README** — add a `bedrock` row to the crate-roster and the feature→module table (next to `anthropic`/`openai`), with the agentcore disambiguation.
- [ ] **Step 4: mdBook** `docs/book/src/concepts/model-providers.md` — add a Bedrock section paralleling the OpenAI/Anthropic sections (builder, `from_env`, family-gated structured output, schema-rewriter note). Run `mdbook build docs/book` — must be link-clean.
- [ ] **Step 5: Commit** `docs(readme): SMA-329 document bedrock provider` (+ a separate `docs(book): SMA-329 add bedrock to model-providers page` if you prefer per-scope commits).

---

## Task 14: Full-gate verification

- [ ] **Step 1:** `cargo +1.91 build --workspace --all-features`
- [ ] **Step 2:** `cargo fmt --all -- --check`
- [ ] **Step 3:** `cargo clippy --workspace --all-features --all-targets -- -D warnings`
- [ ] **Step 4:** `cargo test --workspace --all-features` (live tests loud-skip)
- [ ] **Step 5:** `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
- [ ] **Step 6:** `DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh` (install the nightly if needed)
- [ ] **Step 7:** `cargo deny check` ; `mdbook build docs/book`
- [ ] **Step 8:** `convco check <main>..HEAD` (commit-message gate)
- [ ] **Step 9:** Fix anything red, then this is ready for Stage 5 (open PR).

---

## Release Checklist (pre-merge — Sven-run, NOT a code task)

Per GATE-1 decision (spec §11/R1), `paigasus-helikon-providers-bedrock` is a brand-new crate never name-claimed on crates.io:

- [ ] **Name-claim pre-publish** (Sven, interactive — `cargo login` is interactive-only; never pass the token as an arg): once the crate builds clean, `cargo publish -p paigasus-helikon-providers-bedrock` to claim the name + ship `0.1.0`, **before/with** merging the PR, so the facade's `cargo publish --verify` (run by release-plz on the facade's own bump) finds `bedrock 0.1.0` on the registry.
- [ ] **After merge:** watch the release-plz `chore: release` PR (paigasusbot) — it should bump the facade (new optional dep) and NOT re-attempt to publish bedrock (already at 0.1.0). Confirm its CI is green (a release-PR `cargo update` can pull a fresh advisory → red `audit` on the bot PR only; fix with a `chore(deps)` pin if so).

---

## Self-Review (author)

- **Spec coverage:** §3 layout → Tasks 1–13; §4 rewriter (incl. §4.0 non-reuse, edge cases, invariants) → Task 4; §5 builder/from_env/lazy-chain/precedence → Task 11; §6 streaming/cancellation/both-tools → Tasks 10+12; §7 translation/Document/alternating-turns → Tasks 3,7,8,9; §8 testing (pure units, wire-JSON snapshots, no transport mock) → Tasks 4,9,10,12; §9 errors → Task 6; §10 facade/docs/deny/rustls → Tasks 1,13; §0/§11 MSRV → Task 1; R1 release → Release Checklist. No uncovered section.
- **Placeholder scan:** none — reference-to-existing-file ("mirror `anthropic/...`") points at concrete code, not "TODO"; all novel/AC-critical units carry full test code.
- **Type consistency:** `Ruleset`/`rewrite_tool_schema` (Tasks 4,8,9), `value_to_document` (Tasks 3,7,8), `caps_for` (Tasks 5,11), `classify`/`map_sdk_error` (Tasks 6,12), `StreamTranslator::{new,consume}` (Tasks 10,12), `build_request`/`to_wire_json`/`PreparedConverse` (Tasks 9,12), `Config`/`BuildError` (Tasks 11,12) — names consistent across tasks.
- **Open dependency on the spike:** Task 10's exact SDK field accessors depend on Task 1 Step 6's capture; the plan instructs reconciling them there. This is intentional (the SDK taxonomy is verified, not guessed) and called out in spec §6.
