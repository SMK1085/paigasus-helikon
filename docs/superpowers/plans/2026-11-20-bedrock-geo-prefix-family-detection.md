# SMA-832 Bedrock geographic-prefix family detection: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `ModelFamily::from_model_id` detects the family for every geographic inference profile prefix (today's and future ones) and for ARNs whose resource ID holds the model ID.

**Architecture:** Replace the prefix allowlist in `family.rs` with provider-segment detection: classify the first `.` segment; if it is not a known provider, classify the second. ARNs are reduced to the text after the last `/` first. The builder logs at `debug!` when the result is `Unknown`.

**Tech Stack:** Rust (edition and MSRV 1.94 from the workspace), `tracing`, `aws-sdk-bedrockruntime`.

**Spec:** `docs/superpowers/specs/2026-11-20-bedrock-geo-prefix-family-detection-design.md`

## Global Constraints

- No public API change: `ModelFamily` (still `#[non_exhaustive]`) and `pub fn from_model_id(model_id: &str) -> Self` keep their signatures.
- No hand-bumped crate version and no CHANGELOG edit. release-plz does both.
- Commit format: `<type>(<scope>): SMA-832 <lowercase message>`. Use scope `providers` for crate code, `docs` for the mdBook, `spec`/`plan` for design files.
- Every `tracing` call names a `target:`; the builder uses `"paigasus::bedrock::builder"`.
- Run `cargo fmt --all` and clippy before each commit.
- Work synchronously in the foreground. Do not background a `cargo` run. Do not end a task until each command has a terminal result.
- Do not run any git command that moves HEAD or switches branches. Stage files by explicit path only, never `git add -A` or `git add .`.
- All work happens in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-832`.

## Review Focus

1. A known provider in the first segment must win and be final: `amazon.anthropic.x` is `Unknown` (pinned in Task 1).
2. Uppercase input, including an uppercase `ARN:` prefix, behaves like lowercase (pinned in Task 1 with `ARN:aws:bedrock:us-east-1::foundation-model/anthropic.claude-x`).
3. A trailing `/` in an ARN gives an empty resource ID → `Unknown`, with no panic (pinned in Task 1).
4. An ID made only of dots or with a leading dot (`..anthropic.x`) gives `Unknown`, with no panic (pinned in Task 1).
5. Real IDs that are correct today stay correct; every current test row stays unchanged (Task 1).

---

### Task 1: Provider-segment detection in `family.rs`

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/family.rs` (whole `from_model_id`, the doc comment and doctest at lines 28-41, remove `strip_region_prefix` at lines 86-95, extend `mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: unchanged `pub fn ModelFamily::from_model_id(model_id: &str) -> ModelFamily`. New private `fn classify(provider: &str, rest: &str) -> Option<ModelFamily>`. `strip_region_prefix` no longer exists.

- [ ] **Step 1: Write the failing tests.** Add `#[test] fn detects_family_behind_any_leading_segment()` to `mod tests`, as a table loop in the style of `detects_families_from_bedrock_model_ids` (keep that test and its rows unchanged). Rows (`id` → expected):

```rust
("us.anthropic.claude-sonnet-4-5-20250929-v1:0", Anthropic),
("eu.anthropic.claude-haiku-4-5-20251001-v1:0", Anthropic),
("apac.amazon.nova-pro-v1:0", AmazonNova),
("jp.anthropic.claude-sonnet-4-5-20250929-v1:0", Anthropic),
("au.anthropic.claude-sonnet-5-5", Anthropic),
("ca.anthropic.claude-sonnet-5-5", Anthropic),
("in.anthropic.claude-haiku-4-5-20251001-v1:0", Anthropic),
("global.anthropic.claude-sonnet-4-5-20250929-v1:0", Anthropic),
("global.anthropic.claude-sonnet-5-5", Anthropic),
("ap.anthropic.claude-x", Anthropic),
("us-gov.anthropic.claude-3-5-sonnet-20240620-v1:0", Anthropic),
("global.amazon.nova-2-lite-v1:0", AmazonNova),
("us.meta.llama3-2-90b-instruct-v1:0", Llama),
("eu.mistral.pixtral-large-2502-v1:0", Mistral),
("zz.anthropic.claude-x", Anthropic),
("us.deepseek.r1-v1:0", Unknown),
("us.writer.palmyra-x5-v1:0", Unknown),
("amazon.anthropic.x", Unknown),
("us.llama3-x", Llama),
("GLOBAL.Anthropic.claude-x", Anthropic),
("US.anthropic.x", Anthropic),
("", Unknown),
(".", Unknown),
("..anthropic.x", Unknown),
("global.", Unknown),
("global.anthropic", Anthropic),
("arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-3-5-sonnet-20241022-v2:0", Anthropic),
("ARN:aws:bedrock:us-east-1::foundation-model/anthropic.claude-x", Anthropic),
("arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-3-5-sonnet-20241022-v2:0", Anthropic),
("arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/a1b2c3d4e5f6", Unknown),
("arn:aws:bedrock:us-east-1:123456789012:inference-profile/", Unknown),
("arn:aws:bedrock", Unknown),
```

Assert with `assert_eq!(ModelFamily::from_model_id(id), want, "id={id}")`.

- [ ] **Step 2: Run the tests and see them fail.**
  Run: `cargo test -p paigasus-helikon-providers-bedrock --lib family`
  Expected: FAIL in `detects_family_behind_any_leading_segment` (first failing row is `jp.` or `au.` → got `Unknown`).

- [ ] **Step 3: Implement.** Rewrite `from_model_id`; add `classify`; delete `strip_region_prefix`. Algorithm (fixed by the spec):

```text
id  = model_id.to_ascii_lowercase()
if id starts with "arn:":  id = text after the last '/' ; if there is no '/', return Unknown
(first, rest1)  = split id on the first '.'   (missing part = "")
(second, rest2) = split rest1 on the first '.' (missing part = "")
classify(first, rest1)
  .or_else(|| classify(second, rest2))
  .unwrap_or(if first.contains("llama") || second.contains("llama") { Llama } else { Unknown })
```

`classify` holds the only copy of the provider names: `anthropic` → `Anthropic`; `amazon` → `AmazonNova` if `rest` starts with `nova`, `AmazonTitan` if it starts with `titan`, else `Unknown`; `meta` → `Llama`; `mistral` → `Mistral`; `cohere` → `Cohere`; anything else → `None`. `classify` returns `Some(Unknown)` for `amazon.<other>`, so the second segment is not examined.

Doc comment on `from_model_id`: state the rule. When the first segment is not a known provider name, detection reads the second segment. So a geographic prefix such as `us.`, `eu.`, `apac.`, `jp.`, `au.`, `ca.`, `in.` or `global.` is skipped, and a future prefix is skipped too. An ARN is classified by its resource ID (the text after the last `/`). Opaque IDs such as application inference profile ARNs give `Unknown`. Extend the doctest with:

```rust
assert_eq!(
    ModelFamily::from_model_id("global.anthropic.claude-sonnet-5-5"),
    ModelFamily::Anthropic,
);
```

- [ ] **Step 4: Run the tests and see them pass.**
  Run: `cargo test -p paigasus-helikon-providers-bedrock --lib family && cargo test -p paigasus-helikon-providers-bedrock --doc`
  Expected: PASS, 0 failed.

- [ ] **Step 5: Format, lint and commit.**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-bedrock --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-bedrock/src/family.rs
git commit -m "fix(providers): SMA-832 detect the bedrock family behind any geographic prefix and in ARNs"
```

### Task 2: Builder `Unknown` signal and request-level regression tests

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/builder.rs` (`build()`, step 3 near line 196; `mod tests`)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs` (`mod tests` only)

**Interfaces:**
- Consumes: `ModelFamily::from_model_id` from Task 1. Test helpers already in the files: `offline_client()` in `builder.rs` tests; `make_cfg(model_id: &str) -> Config`, `user_text(s: &str) -> Item` and `SYNTHESIZED_TOOL_NAME` in `translate/mod.rs` tests.
- Produces: no new interface.

- [ ] **Step 1: Write the tests.**
  - In `translate/mod.rs` tests: `#[test] fn json_schema_on_global_claude_profile_synthesizes()`. Same body and assertions as the existing `json_schema_on_claude_synthesizes`, but with `build_request(&make_cfg("global.anthropic.claude-sonnet-5-5"), &req)`. Assert `p.synthesizing`, one tool named `SYNTHESIZED_TOOL_NAME`, and `tool_choice()` `is_tool()` with that name.
  - In `builder.rs` tests: `#[test] fn global_claude_profile_has_structured_output()`: `converse("global.anthropic.claude-sonnet-5-5").client(offline_client()).build()`, then `assert!(model.capabilities().structured_output)` and `assert!(model.capabilities().parallel_tool_calls)`.

- [ ] **Step 2: Run them.**
  Run: `cargo test -p paigasus-helikon-providers-bedrock --lib global_claude`
  Expected: PASS (Task 1 already fixed detection). To prove that the tests protect the fix, temporarily change `make_cfg`'s model ID in the new translate test to `"zz-unknown.model"` and the builder test ID to the same. Both tests must then FAIL. Restore the IDs.

- [ ] **Step 3: Add the `debug!` signal in `build()`.** Directly after `let family = ModelFamily::from_model_id(&self.model_id);`, when `family == ModelFamily::Unknown`, emit:

```rust
tracing::debug!(
    target: "paigasus::bedrock::builder",
    model_id = %self.model_id,
    "model family not detected from the model id; structured-output synthesis and forced tool choice are off",
);
```

- [ ] **Step 4: Run the crate tests.**
  Run: `cargo test -p paigasus-helikon-providers-bedrock`
  Expected: PASS, 0 failed (the live tests show as ignored).

- [ ] **Step 5: Format, lint and commit.**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-bedrock --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-bedrock/src/builder.rs crates/paigasus-helikon-providers-bedrock/src/translate/mod.rs
git commit -m "fix(providers): SMA-832 log an undetected bedrock family and pin global profile behavior"
```

### Task 3: README and mdBook

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/README.md:63` (family table example) and `:71` (prefix sentence)
- Modify: `docs/book/src/concepts/model-providers.md:185-188`

**Interfaces:** none.

- [ ] **Step 1: Edit the README.** Line 63: keep `us.anthropic.*` and add `global.anthropic.*` as an example. Line 71: replace the four-prefix sentence with the rule from Task 1's doc comment (second segment when the first is not a provider; the eight prefixes; future prefixes; ARNs by resource ID; opaque ARNs give `Unknown`).
- [ ] **Step 2: Edit the mdBook paragraph** at lines 185-188 with the same rule, and use `global.anthropic.claude-sonnet-5-5` as the example ID. Write in short, plain sentences.
- [ ] **Step 3: Verify.**
  Run: `npx markdownlint-cli2 crates/paigasus-helikon-providers-bedrock/README.md docs/book/src/concepts/model-providers.md && mdbook build docs/book`
  Expected: 0 markdownlint errors; `mdbook build` exits 0 with no linkcheck error. If `mdbook` is not installed, say so and rely on the `book-build` CI gate.
- [ ] **Step 4: Commit.**

```bash
git add crates/paigasus-helikon-providers-bedrock/README.md docs/book/src/concepts/model-providers.md
git commit -m "docs(providers): SMA-832 describe bedrock family detection behind any prefix"
```

### Task 4: Full gates and live acceptance (coordinator)

- [ ] **Step 1: Run the CI gates** from `CLAUDE.md`: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`, `cargo test --workspace --all-features`, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`. Expected: each exits 0.
- [ ] **Step 2: Live run.**
  Run: `AWS_PROFILE=maip-dev-admin AWS_DEFAULT_REGION=us-east-1 BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5 cargo test -p paigasus-helikon-providers-bedrock --test live -- --ignored --nocapture 2>&1 | tee <scratchpad>/live.txt`
  Expected: `tool_with_tagged_enum_and_nested_generic` and `structured_output_synthesis` report `ok`, and the output has no `[live] ... skipping` line for them. Save the output for the PR body. A skip does not count as a pass.
- [ ] **Step 3: Commit the spec and plan.**

```bash
git add docs/superpowers/specs/2026-11-20-bedrock-geo-prefix-family-detection-design.md docs/superpowers/plans/2026-11-20-bedrock-geo-prefix-family-detection.md
git commit -m "docs(spec): SMA-832 add the bedrock geographic prefix design and plan"
```
