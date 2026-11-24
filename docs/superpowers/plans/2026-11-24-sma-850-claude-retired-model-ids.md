# SMA-850 Remove the Retired Claude 3.x Model Ids — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** No retired Claude 3.x id stays in code, tests or docs. The Anthropic capability table gains a `claude-haiku-5-5` row, and every user-facing Bedrock example names `global.anthropic.claude-sonnet-5-5`.

**Architecture:** String and table edits in two provider crates, the stream conformance suite, two READMEs and the mdBook. No public API changes. One new capability row, which a unit test pins.

**Tech Stack:** Rust (edition per workspace, MSRV 1.94), `insta` snapshots, mdBook, AWS CLI and the Anthropic API for the live runs.

**Spec:** `docs/superpowers/specs/2026-11-24-sma-850-claude-retired-model-ids-design.md`. Read it first. Its "Test ids" table and its Facts section are the source for every id and flag in this plan.

## Global Constraints

- Work only in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-850`, on branch `feature/sma-850-replace-the-retired-claude-3x-model-ids-in`.
- Commit subject form: `<type>(<scope>): SMA-850 <lowercase subject>`. Use `fix(providers)` for code, `docs(providers)` for doc-only commits, `test(providers)` for test-only commits. End each message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Do not use `--no-verify`. Do not amend or reset a commit you did not make in this task.
- Do not read, print or commit `.env`. It is git-ignored. Load it only for a live run with `set -a; . ./.env; set +a` in the same shell command.
- No test may change its code path. Only id strings change, except in Task 1 (new row and its test) and the one allowed limit change in Task 2.
- A test id must never be `claude-sonnet-5-5` or `claude-opus-5-5` (these switch on the Bedrock native output path).
- No manual version bumps. No edits to existing `CHANGELOG.md` entries or to files under `docs/superpowers/` other than this plan.
- Do not install software on the host.

## Review Focus

1. A snapshot diff that changes more than the one `"modelId"` line. Expected: stop and report. Task 3 checks each `.snap.new` diff.
2. A rustfmt reflow after a shorter id. Expected: `cargo fmt --all -- --check` is clean. Task 3 and Task 4 run `cargo fmt --all`.
3. A live negative control that fails for the wrong reason (expired SSO, compile error). Expected: a stop, not a pass. Task 5 greps for the Bedrock error text.
4. Haiku 5.5 thinking by default and using the 64-token budget of `messages_smoke`. Expected: the only allowed change is the limit to 1024, recorded. Task 2 and Task 5.
5. A leftover retired id in prose that the id grep cannot see (for example "Claude 3.5"). Expected: none. Task 5 Step 1 runs a second grep for prose forms.

---

### Task 1: Anthropic capability table

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/capabilities.rs:129-226` (rows), `:261-274` (tests)

**Interfaces:**
- Consumes: `lookup(model_id: &str) -> ModelEntry`, `ModelCapabilities` builder methods (`with_streaming`, `with_tools`, `with_parallel_tool_calls`, `with_structured_output`, `with_vision`, `with_reasoning`, `with_prompt_caching`).
- Produces: a `KNOWN_MODELS` row keyed `"claude-haiku-5-5"`. Task 2 relies on that id.

- [ ] **Step 1: Write the failing test** in the `tests` module. Replace `haiku_3_5_has_no_vision` with:

```rust
#[test]
fn haiku_5_5_row_matches_the_observed_capabilities() {
    let e = lookup("claude-haiku-5-5");
    assert!(e.caps.streaming);
    assert!(e.caps.tools);
    assert!(e.caps.parallel_tool_calls);
    assert!(e.caps.structured_output);
    assert!(e.caps.vision);
    assert!(e.caps.reasoning);
    assert!(e.caps.prompt_caching);
    assert_eq!(e.max_output_default, 32_768);
}
```

Delete `old_3_sonnet_lacks_prompt_caching`. Keep every other test.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib capabilities::tests::haiku_5_5_row_matches_the_observed_capabilities`
Expected: FAIL (`assertion failed: e.caps.parallel_tool_calls`, because the id falls through to the conservative defaults).

- [ ] **Step 3: Edit `KNOWN_MODELS`**

Add a `claude-haiku-5-5` row in the Claude 4/5 section, directly before the `claude-haiku-4-5` row, with all seven flags and `max_output_default: 32_768`. Change the section comment `// Claude 4 family — primary lineup as of 2026-05.` to `// Claude 4 and 5 families.`. Delete the 8 rows from `"claude-3-5-sonnet-latest"` to `"claude-3-haiku-20240307"` and the two comments `// Claude 3.5 family` and `// Claude 3 family — older; …`. Do not change any Claude 4 row.

- [ ] **Step 4: Run the crate's unit tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib`
Expected: PASS, including `known_models_have_no_duplicate_ids`.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/capabilities.rs
git commit -m "fix(providers): SMA-850 drop the claude 3.x capability rows and add haiku 5.5"
```

---

### Task 2: Anthropic live tests, docs and CHANGELOG

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/tests/live.rs:41,65`
- Modify: `crates/paigasus-helikon-providers-anthropic/README.md` (end of the `## Example` section, before `## Native tool types` at line 24)
- Modify: `docs/book/src/concepts/model-providers.md` (Anthropic section, lines 119-148)
- Modify: `crates/paigasus-helikon-providers-anthropic/CHANGELOG.md` (under `## [Unreleased]`)

**Interfaces:**
- Consumes: the `claude-haiku-5-5` row from Task 1.

- [ ] **Step 1: Change the ids** in `messages_smoke` (`live.rs:41`) and `structured_output_smoke` (`live.rs:65`) from `"claude-haiku-4-5"` to `"claude-haiku-5-5"`. Do not touch `src/live_round_trip.rs`, `settings.rs`, or any `Enabled`-thinking text: Haiku 4.5 stays there.

- [ ] **Step 2: Add the support text.** In the README and in the book's Anthropic section, add one paragraph in the same form as the Gemini sentence at `model-providers.md:344`:

> Claude 3.x models are not supported. A model id that the crate does not know reports a conservative capability set (streaming and tools). Use `.with_capabilities()` on the builder to set other values.

- [ ] **Step 3: Add the CHANGELOG entry** under `## [Unreleased]`:

```markdown
### Upgrade notes

- Claude 3.x models are not supported. A `claude-3-*` id now reports the conservative capability set (streaming and tools). For a `claude-3-5-*` id, the default `max_tokens` drops from 8192 to 4096. Claude 3 ids already had 4096. If you still call such a model, set `max_output_tokens` per request and `.with_capabilities()` on the builder, or stay on 0.3.5.
- `claude-haiku-5-5` now reports streaming, tools, parallel tool calls, structured output, vision, reasoning and prompt caching, with a default `max_tokens` of 32768.
```

- [ ] **Step 4: Check that the crate builds and the docs lint**

Run: `cargo test -p paigasus-helikon-providers-anthropic --no-run && npx markdownlint-cli2`
Expected: build succeeds, markdownlint reports 0 errors.

(The live run of these two tests is in Task 5, Step 5. If `messages_smoke` then ends with no text because thinking used the 64-token budget, the one allowed change is `max_output_tokens = Some(1024)` in that test.)

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic docs/book/src/concepts/model-providers.md
git commit -m "fix(providers): SMA-850 move the anthropic live smokes to haiku 5.5 and document the claude 3.x removal"
```

---

### Task 3: Bedrock crate ids, snapshots and docs

**Files:**
- Modify: `crates/paigasus-helikon-providers-bedrock/src/{builder.rs,fallback.rs,model.rs,translate/mod.rs,family.rs,lib.rs}`
- Modify: the 8 files in `crates/paigasus-helikon-providers-bedrock/src/translate/snapshots/` that contain `claude-3`
- Modify: `crates/paigasus-helikon-providers-bedrock/tests/live.rs:6`
- Modify: `crates/paigasus-helikon-providers-bedrock/README.md:31` and the `## Example` section
- Modify: `docs/book/src/concepts/model-providers.md:171,246,507` and the Bedrock `#### Construction` section

**Interfaces:**
- Consumes: none.
- Produces: none (string changes only).

- [ ] **Step 1: User-facing examples** → `global.anthropic.claude-sonnet-5-5`: `README.md:31`, `src/lib.rs:11`, `src/builder.rs:42`, `:57`, `:388`, book `:171`, `:246` (replaces the LEGACY `global.anthropic.claude-sonnet-4-5-20250929-v1:0`), `:507`, and `tests/live.rs:6` (replaces the LEGACY Sonnet 4.5 id).

- [ ] **Step 2: `family.rs`.** The doctest at `:57` uses `anthropic.claude-haiku-5-5`. In the tests, apply the spec's "Test ids" table rows for `:164`, `:165`, `:166`, `:221`, `:237`, `:239` exactly. Leave `:212` and `:217` (Haiku 4.5) unchanged.

- [ ] **Step 3: Offline test ids.** In `builder.rs`, `fallback.rs`, `model.rs` and `translate/mod.rs` tests, replace `anthropic.claude-3-5-sonnet-20241022-v2:0` with `anthropic.claude-sonnet-4-6`. Then run `cargo fmt --all`.

- [ ] **Step 4: Run the tests and review the snapshots**

Run: `INSTA_UPDATE=new cargo test -p paigasus-helikon-providers-bedrock`
Expected: the 8 snapshot tests fail with a pending `.snap.new` each; all other tests pass. For each `.snap.new`, `diff` it against its `.snap`. The only accepted difference is the `"modelId"` line changing to `"anthropic.claude-sonnet-4-6"`. Any other difference: stop and report. Then move each `.snap.new` over its `.snap` with `mv` (`cargo-insta` is not installed; do not install it).

- [ ] **Step 5: Add the Bedrock README and book text.** After the example in the README `## Example` section and after the first code block in the book `#### Construction` section, add:

> The example uses a cross-region inference profile id (`global.`). Current Claude models on Bedrock need an inference profile id in place of a bare `anthropic.` id. Claude 3.x models are not supported.

- [ ] **Step 6: Run the tests again**

Run: `cargo test -p paigasus-helikon-providers-bedrock && cargo test --doc -p paigasus-helikon-providers-bedrock && find crates -name '*.snap.new'`
Expected: all PASS; `find` prints nothing.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-providers-bedrock docs/book/src/concepts/model-providers.md
git commit -m "fix(providers): SMA-850 replace the retired claude ids in the bedrock crate and docs"
```

---

### Task 4: Stream conformance suite

**Files:**
- Modify: `tests/provider-stream-conformance/src/eventstream.rs:118`
- Modify: `tests/provider-stream-conformance/tests/conformance.rs:373,2040-2042`

- [ ] **Step 1: Change the ids.** Both Bedrock sites: `anthropic.claude-3-5-sonnet-20240620-v1:0` → `anthropic.claude-sonnet-4-6`. Anthropic `MODEL_ID`: `claude-3-5-sonnet-latest` → `claude-sonnet-4-6`.

- [ ] **Step 2: Replace the `MODEL_ID` doc comment** (`conformance.rs:2040-2041`) with:

```rust
    /// Uses a current id from `KNOWN_MODELS`. The suite reads neither the
    /// model capabilities nor the request body, so this id only has to be
    /// one the crate accepts.
```

Then run `cargo fmt --all`.

- [ ] **Step 3: Run the suite**

Run: `cargo test -p paigasus-helikon-provider-stream-conformance`
Expected: PASS, same test count as on `origin/main`.

- [ ] **Step 4: Commit**

```bash
git add tests/provider-stream-conformance
git commit -m "test(providers): SMA-850 use current claude ids in the stream conformance suite"
```

---

### Task 5: Verification (no new code)

- [ ] **Step 1: No retired id remains**

Run: `git grep -nE 'claude-3|claude-instant|claude-v2' -- ':!docs/superpowers' ':!**/CHANGELOG.md'` and `git grep -nE -i 'claude 3|3\.5 (sonnet|haiku)|(sonnet|haiku|opus) 3' -- ':!docs/superpowers' ':!**/CHANGELOG.md'`
Expected: both print nothing.

- [ ] **Step 2: Full CI gate set** from the Helikon `CLAUDE.md` "Common commands" block (fmt, clippy with `--all-features --all-targets -D warnings`, `cargo test --workspace --all-features`, docs with `RUSTDOCFLAGS="-D warnings"`, doc coverage, markdownlint and its config check, the `scripts/check-*` scripts), plus `mdbook build docs/book`.
Expected: every command exits 0.

- [ ] **Step 3: Live Bedrock negative control**

Run: `AWS_PROFILE=maip-dev-admin AWS_DEFAULT_REGION=us-east-1 BEDROCK_MODEL_ID=anthropic.claude-3-5-sonnet-20241022-v2:0 cargo test -p paigasus-helikon-providers-bedrock --test live -- --ignored --nocapture smoke_text_turn 2>&1 | tee "$SCRATCH/neg.log"`
Expected: the test FAILS, and `grep -E 'end of its life|ResourceNotFoundException' "$SCRATCH/neg.log"` matches. A credential, SSO, network or compile error is a stop: report it. Keep the matched line for the PR body.

- [ ] **Step 4: Live Bedrock, example id**

Run: the same command with `BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5` and no test filter.
Expected: every test PASSES, and the output has no `[live]` skipping line. If a test fails, run it alone once more. If it fails again, stop and report.

- [ ] **Step 5: Live Anthropic API**

Run: `set -a; . ./.env; set +a; cargo test -p paigasus-helikon-providers-anthropic -- --ignored --nocapture 2>&1 | tee "$SCRATCH/anthropic.log"`
Expected: every test PASSES, including `messages_smoke`, `structured_output_smoke` and the `live_round_trip` lib tests; no test reports a skip for a missing key. If the permission rules block reading `.env`, stop and ask the coordinator. If `messages_smoke` fails only because thinking used the 64-token budget, apply the Task 2 limit change, commit it (`test(providers): SMA-850 give the haiku 5.5 smoke room to think`), and re-run. Any other failure: run once more, then stop and report.

`$SCRATCH` is the session scratchpad directory given by the coordinator. Never write logs into the repository.
