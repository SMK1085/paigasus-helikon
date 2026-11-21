# SMA-565: Remove the retired Gemini 1.x and 2.x model ids — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** No Gemini 1.x or 2.x model id stays in code, tests or docs. The id-based request branch goes, and `gemini-3.8-flash` is the only known model.

**Architecture:** Delete `is_gemini_3_or_later` so that `items_to_contents` always applies the dummy-signature and trailing-turn rules. Reduce `KNOWN_MODELS` to one row. Change every test id and doc example to `gemini-3.8-flash`, and add a CHANGELOG upgrade note.

**Tech Stack:** Rust (edition from workspace), `insta` snapshots, `wiremock`, `tokio`, mdBook.

**Spec:** `docs/superpowers/specs/2026-11-21-sma-565-gemini-retired-model-ids-design.md` (read it first; this plan does not repeat its rationale).

## Global Constraints

- Worktree: `/Users/smaschek/dev/paigasus/paigasus-helikon-sma-565`. Every path below is relative to it. Use absolute paths for Write/Edit. Guard each `cd` with `|| exit 1`.
- Do not run git commands that move HEAD or switch branches. Do not push.
- Stage files by explicit path. Never `git add -A` or `git add .` (`.env` is not ignored).
- The only new model id is `gemini-3.8-flash`. Do not invent ids (no `gemini-3.8-pro`). `gemini-3.6-flash`, `gemini-4-pro` and `my-tuned-model` stay where they already are.
- Do not edit files under `docs/superpowers/` other than this plan, and do not edit existing `CHANGELOG.md` entries.
- Do not run `cargo insta accept` and do not set `INSTA_UPDATE`. Accept a snapshot only by the rule in Task 1.
- Do not add or remove lines above line 86 in `crates/paigasus-helikon-providers-gemini/tests/gemini_streaming.rs` (cited by line in `tests/provider-stream-conformance/tests/conformance.rs`).
- Do not bump any crate version.
- Commit titles: `<type>(providers): SMA-565 <lowercase subject>`. Allowed types: `fix`, `test`, `docs`. End each commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Run `cargo fmt --all` before each commit.
- Work synchronously in the foreground. Do not background a cargo run. Do not end the task before the command reaches a final status.
- Prose in docs and comments: ASD-STE100 Simplified Technical English (short sentences, active voice, simple tenses).

## Review Focus

1. A model id with a `models/` prefix or a Vertex resource path must get the same request shape as a bare id. Pinned by `every_model_id_gets_the_dummy` (Task 1), which keeps `models/gemini-3.8-flash`.
2. A current-turn model content with one signed and one unsigned call must be sent as received (no dummy). Pinned by the existing `mixed_content_is_sent_as_received`; Task 1 must keep it green and unchanged.
3. A tuned or unknown id must report the conservative capability set. Pinned by the existing `unknown_model_falls_back_to_conservative` (Task 2), which must stay.
4. A Vertex request sent to a wrong URL must fail its wire test. Pinned by the stronger `vertex_url_and_bearer_header` (Task 3).
5. A live run that gets an HTTP error must fail with the error text, not with a bare assertion. Pinned by the unwrap in `live_developer_text_turn` (Task 4) and the negative control (Task 6).

---

### Task 1: Remove the id-based request branch

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/src/translate/request.rs` (predicate at 395-406, call sites at 252 and 263, doc at 282 and 408-412, tests at 1455-1500 and 1718-1765)
- Modify: `crates/paigasus-helikon-providers-gemini/src/translate/mod.rs:113` (`snap::cfg()`)
- Modify: `crates/paigasus-helikon-providers-gemini/src/translate/snapshots/paigasus_helikon_providers_gemini__translate__snap__snap_tool_call_and_result.snap`
- Modify: `crates/paigasus-helikon-providers-gemini/src/translate/snapshots/paigasus_helikon_providers_gemini__translate__snap__snap_parallel_same_name_tool_calls.snap`

**Interfaces:**
- Consumes: nothing.
- Produces: `items_to_contents(items, model_id, api)` keeps its signature. It no longer branches on `model_id` (the parameter stays, because `Pending::valid_signature` at `request.rs:166` uses it).

- [ ] **Step 1: Change the tests.** In `request.rs`:
  - Delete `gemini_2_history_gets_no_dummy`, `gemini_2_real_signature_is_still_sent_back` and `gemini_2_trailing_model_turn_is_unchanged`.
  - Rename `gemini_3_and_unknown_ids_keep_the_dummy` to `every_model_id_gets_the_dummy`. Keep its body and its ids.
  - Rename `gemini_3_trailing_model_turn_gets_a_user_turn` to `trailing_model_turn_gets_a_user_turn`. Keep its body.
  - In `mod.rs`, set `snap::cfg()` `model_id` to `"gemini-3.8-flash"`.

- [ ] **Step 2: Run the snapshot tests to see them fail.**
  Run: `cargo test -p paigasus-helikon-providers-gemini --lib translate::snap`
  Expected: FAIL in exactly `snap_tool_call_and_result` and `snap_parallel_same_name_tool_calls`. Two `.snap.new` files appear.

- [ ] **Step 3: Check the snapshot diffs.** For each `.snap.new`, run `diff <old>.snap <old>.snap.new`. Accept only this result:
  - `snap_tool_call_and_result`: 1 added line `"thoughtSignature": "skip_thought_signature_validator"`.
  - `snap_parallel_same_name_tool_calls`: 2 added lines with the same text.
  - The line before each added line gains a trailing comma (`}` becomes `},`).
  - No "Continue." text, and no other change.
  If a diff matches, move the `.snap.new` over the `.snap` with `mv`. If a diff does not match, stop and report the diff.

- [ ] **Step 4: Remove the predicate.** Delete `fn is_gemini_3_or_later` and its doc comment. Call `add_dummy_signatures(&mut merged)` without a condition. Push the trailing user turn whenever `contents.last()` has role `"model"`. Change the `TRAILING_MODEL_TURN_PROMPT` doc so that it says "the translator appends this turn" with no "for Gemini 3 and later". Change "Gemini 3 starts the current turn" (`request.rs:282`) to "Gemini starts the current turn".

- [ ] **Step 5: Run the crate tests.**
  Run: `cargo test -p paigasus-helikon-providers-gemini`
  Expected: PASS. No `.snap.new` file exists (`git status --porcelain | grep snap.new` prints nothing). `mixed_content_is_sent_as_received` passes unchanged.

- [ ] **Step 6: Check that the predicate is gone.**
  Run: `git grep -nE 'is_gemini_3_or_later|starts_with\("gemini-' -- crates/paigasus-helikon-providers-gemini`
  Expected: no output.

- [ ] **Step 7: Commit.**
  `git add` the four files above, then commit:
  `fix(providers): SMA-565 send the current gemini request shape for every model id`

### Task 2: Reduce the capability table to `gemini-3.8-flash`

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/src/capabilities.rs:31-36` (table) and `:49-65` (tests)

**Interfaces:**
- Consumes: nothing.
- Produces: `KNOWN_MODELS` holds exactly `[("gemini-3.8-flash", full())]`. `lookup` and `conservative_defaults` do not change.

- [ ] **Step 1: Write the tests.** Replace `known_25_flash_has_tools_and_structured_output` with:

```rust
#[test]
fn known_38_flash_has_full_capabilities() {
    let e = lookup("gemini-3.8-flash");
    assert!(e.caps.streaming && e.caps.tools && e.caps.parallel_tool_calls);
    assert!(e.caps.structured_output && e.caps.vision);
    // Reasoning streaming is deferred (D3).
    assert!(!e.caps.reasoning);
}

#[test]
fn known_models_have_no_duplicate_ids() {
    let mut ids: Vec<&str> = KNOWN_MODELS.iter().map(|(id, _)| *id).collect();
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n);
}
```

  Keep `unknown_model_falls_back_to_conservative`.

- [ ] **Step 2: Run the tests to see the first one fail.**
  Run: `cargo test -p paigasus-helikon-providers-gemini --lib capabilities`
  Expected: `known_38_flash_has_full_capabilities` FAILS (the id has no row).

- [ ] **Step 3: Change the table.** `KNOWN_MODELS` has one row: `("gemini-3.8-flash", full())`. Delete the four 2.x rows. Keep the doc comment above the table.

- [ ] **Step 4: Run the tests.**
  Run: `cargo test -p paigasus-helikon-providers-gemini --lib capabilities`
  Expected: PASS (3 tests).

- [ ] **Step 5: Commit.**
  `fix(providers): SMA-565 list gemini-3.8-flash as the only known gemini model`

### Task 3: Change the remaining test ids

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/src/builder.rs:257-348` (test ids `gemini-2.5-flash`, `gemini-2.5-pro`)
- Modify: `crates/paigasus-helikon-providers-gemini/src/model.rs:207,212`
- Modify: `crates/paigasus-helikon-providers-gemini/src/transport.rs:62,72,85,91,108`
- Modify: `crates/paigasus-helikon-providers-gemini/tests/gemini_wire.rs:33,41,63,70,96` and `vertex_url_and_bearer_header`
- Modify: `crates/paigasus-helikon-providers-gemini/tests/gemini_streaming.rs:21,135`
- Modify: `tests/provider-stream-conformance/tests/conformance.rs:2315-2321`

**Interfaces:**
- Consumes: Task 1 (request shape) and Task 2 (capability table).
- Produces: no 1.x or 2.x id in any test.

- [ ] **Step 1: Make the Vertex wire test strict.** In `vertex_url_and_bearer_header`, replace `assert!(!evs.is_empty());` with the same check that `developer_url_and_api_key_header` uses: collect the `TokenDelta` texts with `.unwrap()` on each event, and `assert_eq!(texts, vec!["hi"]);`. Keep the old `gemini-2.5-pro` path for this step.

- [ ] **Step 2: Prove that the strict test detects a wrong URL.** Change only the model id in `GeminiModel::vertex(...)` (line 70) to `gemini-3.8-flash`, and leave the mock path at line 63 unchanged.
  Run: `cargo test -p paigasus-helikon-providers-gemini --test gemini_wire vertex_url_and_bearer_header`
  Expected: FAIL (wiremock returns 404, the unwrap panics). Then change the mock path at line 63 to `.../models/gemini-3.8-flash:streamGenerateContent` and run again. Expected: PASS.

- [ ] **Step 3: Change every other id to `gemini-3.8-flash`.** Apply it to all files in the list, together with the expected URLs (`transport.rs:85,91,108`, `gemini_wire.rs:33`), the `m.model()` assertions (`builder.rs:302,320`, `model.rs:212`), the `Debug` `contains` checks (`builder.rs:341,348`) and the `data[0].1["model"]` assertion (`gemini_streaming.rs:135`). Edit lines in place. Do not add or remove lines in `gemini_streaming.rs`.

- [ ] **Step 4: Change the conformance id.** Set `MODEL_ID` to `"gemini-3.8-flash"`. Replace the two sentences that start "Matches the constructor example in the task brief" with: "Uses the id the crate docs name. `PacedServer` ignores the request, so this id only has to be one the crate accepts." Keep the sentence that cites the `gemini-3.6-flash` live capture.

- [ ] **Step 5: Run the tests.**
  Run: `cargo test -p paigasus-helikon-providers-gemini && cargo test -p provider-stream-conformance`
  (If the package name differs, read it from `tests/provider-stream-conformance/Cargo.toml`.)
  Expected: PASS.

- [ ] **Step 6: Check the ids.**
  Run: `git grep -nE 'gemini-[12]\.' -- crates tests`
  Expected: output only from `README.md`, `src/lib.rs` and `tests/live.rs` (Tasks 4 and 5 change them).

- [ ] **Step 7: Commit.**
  `test(providers): SMA-565 use gemini-3.8-flash in gemini tests and check the vertex wire url`

### Task 4: One default id in the live tests

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/tests/live.rs` (module doc 1-6, `dev_model` 14-20, `live_developer_text_turn` 32-46, `live_vertex_text_turn` 197)

**Interfaces:**
- Consumes: nothing.
- Produces: `const DEFAULT_MODEL_ID: &str = "gemini-3.8-flash";` in `tests/live.rs`.

- [ ] **Step 1: Add the const and use it.** `dev_model()` and `live_vertex_text_turn` call `std::env::var("GEMINI_MODEL_ID").unwrap_or_else(|_| DEFAULT_MODEL_ID.into())`. The module doc says that the Vertex default is not live-tested.

- [ ] **Step 2: Unwrap each event in `live_developer_text_turn`.** Replace `if let Ok(ModelEvent::TokenDelta { .. }) = ev` with `if let ModelEvent::TokenDelta { .. } = ev.unwrap()`, as `live_vertex_text_turn` does.

- [ ] **Step 3: Compile the live tests.**
  Run: `cargo test -p paigasus-helikon-providers-gemini --all-features --test live --no-run`
  Expected: builds with no warnings. (`--all-features` compiles the `vertex-adc` test.)

- [ ] **Step 4: Commit.**
  `test(providers): SMA-565 share one default model id in the gemini live tests`

### Task 5: Docs, README and CHANGELOG

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/src/lib.rs:30,40,63-69`
- Modify: `crates/paigasus-helikon-providers-gemini/README.md:37,51,73,86,98,101` (plus the intro or Transports section and the Vertex section)
- Modify: `docs/book/src/concepts/model-providers.md:294,302,314,322,355-372,451`
- Modify: `crates/paigasus-helikon-providers-gemini/CHANGELOG.md` (under `## [Unreleased]`)

**Interfaces:**
- Consumes: Tasks 1-2 (the behaviour the docs describe).
- Produces: docs only.

- [ ] **Step 1: Change the examples.** Every `gemini-2.5-flash` example in the three doc files becomes `gemini-3.8-flash`.

- [ ] **Step 2: Rewrite the signature paragraph** in `README.md:98`, `lib.rs:63-69` and `model-providers.md:355-360`. Use this text, with the file's own formatting:

> Tool loops work. The provider keeps each `thoughtSignature` as `ContentPart::ProviderData` (key `PROVIDER_DATA_KEY`, value `"gemini"`) and sends it back on its own call. It restores the call order that Gemini returned. A signature from another model id or another API counts as not valid. When no call in a model content of the current turn has a valid signature, the provider puts the documented dummy value `skip_thought_signature_validator` on each call of that content. A request that ends with a model turn gets a `user` text turn "Continue." appended, because Gemini rejects such a request. Gemini 1.x and 2.x models are not supported.

  In the limits list, the dummy bullet becomes: "**Dummy value** — it is live-tested only on `gemini-3.8-flash` (Developer API). It is not tested on Vertex." Keep the Vertex and text-level bullets.

- [ ] **Step 3: Add the support statement and the Vertex note.** In the README intro (or its Transports section) and in the book Gemini section, add:

> Gemini 1.x and 2.x models are not supported. A model id that the crate does not know reports a conservative capability set (streaming and tools). Use `.with_capabilities()` on the builder to set other values.

  In the README Vertex section and next to `model-providers.md:372`, use the same note: "Model ids are examples. Vertex model availability differs by region."

- [ ] **Step 4: Add the upgrade note.** Under `## [Unreleased]` in the gemini `CHANGELOG.md`:

```markdown
### Upgrade notes

- Gemini 1.x and 2.x models are not supported. The Developer API no longer serves them to new users.
- A request for any model id now gets the Gemini 3 request shape. When no call in a model content of the current turn has a valid signature, the request carries the dummy thought signature. A request that ends with a model turn gets a "Continue." user turn.
- `gemini-3.8-flash` reports the full capability set. `gemini-2.5-*` and `gemini-2.0-*` ids now report the conservative set.
- If you still call a Gemini 2.x model, stay on 0.3.1.
```

- [ ] **Step 5: Check the docs.**
  Run: `cargo test -p paigasus-helikon-providers-gemini --doc && RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-providers-gemini --all-features --no-deps && mdbook build docs/book && npx markdownlint-cli2`
  Expected: all pass. (If `mdbook` is not installed, report it; do not install it.)

- [ ] **Step 6: Run the verification greps.**
  Run: `git grep -nE 'gemini-[12]\.' -- ':!docs/superpowers' ':!**/CHANGELOG.md'`
  Expected: no output.
  Run: `git grep -nE 'is_gemini_3_or_later|starts_with\("gemini-|Gemini 3 and later|earlier shape' -- crates/paigasus-helikon-providers-gemini docs/book`
  Expected: no output.

- [ ] **Step 7: Commit.**
  `docs(providers): SMA-565 document gemini-3.8-flash and the end of gemini 2.x support`

### Task 6: Full gates and live verification (coordinator only)

This task needs the API key from the main checkout's `.env`. The coordinator runs it, not a subagent.

- [ ] **Step 1: Run every CI gate from `CLAUDE.md`** (fmt, clippy `--all-features --all-targets -D warnings`, `cargo test --workspace --all-features`, `cargo doc` with `-D warnings`, doc coverage, markdownlint, the script checks). Expected: all pass. No `.snap.new` file exists.

- [ ] **Step 2: Negative control.**
  Run: `GEMINI_MODEL_ID=gemini-2.5-flash cargo test -p paigasus-helikon-providers-gemini --test live -- --ignored --nocapture live_developer_text_turn`
  Expected: FAIL, and the output contains `gemini http 404`.

- [ ] **Step 3: Default live run.**
  Run: `cargo test -p paigasus-helikon-providers-gemini --test live -- --ignored --nocapture` (no `GEMINI_MODEL_ID`).
  Expected: PASS. If `live_agent_two_sequential_tool_rounds` fails, run it once more. A second failure is a stop.
  Run: `cargo test -p paigasus-helikon-providers-gemini --lib -- --ignored --nocapture live_signature_round_trip`
  Expected: PASS, and the output does not contain "skipped".

- [ ] **Step 4: Record the results** for the PR body: the negative-control error line, the pass counts, and "Vertex not verified".
