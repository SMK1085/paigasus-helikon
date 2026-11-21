# SMA-585 Responses `[DONE]` provenance fix: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the `data: [DONE]` sentinel from every OpenAI Responses fixture and test body, make each fixture header record every edit, and write the provenance rule into `CONTRIBUTING.md`.

**Architecture:** Test-only and documentation-only change. Five SSE fixtures lose their last line and get corrected `:` comment headers. Two test helpers stop sending the sentinel. Two new tests pin the result: a guard that no fixture contains the sentinel, and a labelled compatibility test that a trailing sentinel is still tolerated.

**Tech Stack:** Rust, tokio tests, wiremock, async-openai 0.43.0, eventsource-stream SSE (`:` lines are comments).

**Spec:** `docs/superpowers/specs/2026-11-21-sma-585-responses-done-sentinel-design.md`. Read it first. Its Appendix A has the 2026-11-21 capture evidence and the key-order table that the headers cite.

## Global Constraints

- No existing test assertion changes. No asserted id, model or token count changes.
- No production code (`src/`) changes.
- Do not edit dated design documents under `docs/superpowers/**` other than this plan and the spec.
- Chat Completions fixtures and tests (`chat_*.txt`, `cancellation.rs`, `chat_wire.rs`) do not change.
- If a test changes behaviour when the sentinel is removed, stop and report. Do not add the line back.
- No fixture header line, with its leading `:` removed, may be equal to `data: [DONE]`.
- Commit messages: `<type>(<scope>): SMA-585 <lowercase verb> ...`, ending with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Scopes: `test(providers-openai)`, `test(providers)`, `docs(contributing)`, `docs(claude)`, `docs(plan)`.
- Stage files by explicit path. Never `git add -A` (`.env` and `.claude/` are not ignored).
- Work in `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-585`. Guard every `cd` with `|| exit 1`. Do not run `git checkout`, `git switch`, `git reset`, `git stash` or any command that moves HEAD or another branch.
- Run cargo in the foreground, and do not end the task until each command has a final status.

## Review Focus

1. **A fixture with CRLF line endings** (Windows checkout; the OpenAI fixtures have no `.gitattributes` rule). The guard test must still find a `data: [DONE]\r` line: compare `line.trim_end()`. Owned by Task 1.
2. **A header line that loses its `:`** becomes a real SSE data line. The guard test catches a sentinel line; the existing behaviour tests catch other damage. Owned by Task 1.
3. **`response.failed` with no trailing sentinel.** The stream must still end with exactly one `Err` and nothing after it. `failed_event_terminates_stream_with_error` already pins this; it must pass unchanged. Owned by Task 1.
4. **A server that sends `[DONE]` after `response.completed`** (OpenAI-compatible proxies via `base_url`). Events must be identical to the stream without it. Owned by Task 2.
5. **A conformance scenario whose bytes become identical to a declined scenario.** `CleanStop` must still differ from `TruncatedAfterStopReason` in its documentation: `response.completed` is the terminator. Owned by Task 3.

---

### Task 1: Fixtures and the sentinel guard test

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/tests/fixtures/responses_tool_call.txt`
- Modify: `crates/paigasus-helikon-providers-openai/tests/fixtures/responses_tool_call_zero_args.txt`
- Modify: `crates/paigasus-helikon-providers-openai/tests/fixtures/responses_reasoning_then_text.txt`
- Modify: `crates/paigasus-helikon-providers-openai/tests/fixtures/responses_incomplete_length.txt`
- Modify: `crates/paigasus-helikon-providers-openai/tests/fixtures/responses_incomplete_filter.txt`
- Modify: `crates/paigasus-helikon-providers-openai/tests/fixtures/responses_failed.txt`
- Test: `crates/paigasus-helikon-providers-openai/tests/responses_streaming.rs`

**Interfaces:**
- Consumes: the six `include_str!` constants at `responses_streaming.rs:15-20` (`REASONING`, `LENGTH`, `FILTER`, `FAILED`, `TOOL_CALL`, `ZERO_ARGS`).
- Produces: fixture headers that Task 3 cites by `sequence_number`; the "Removed / Added / Changed" header structure that Task 4 writes into `CONTRIBUTING.md`.

- [ ] **Step 1: Write the failing guard test** in `responses_streaming.rs`, after the constants:

```rust
/// No Responses fixture may carry a `data: [DONE]` line: the OpenAI
/// Responses API does not send one (SMA-585; captures on 2026-08-22 and
/// 2026-11-21). Behaviour tests cannot notice a re-added sentinel, because
/// async-openai ends the stream the same way on `[DONE]` and on end of body.
/// `trim_end` keeps the check true on a CRLF checkout.
#[test]
fn no_responses_fixture_carries_a_done_sentinel() {
    for (name, fixture) in [
        ("responses_reasoning_then_text.txt", REASONING),
        ("responses_incomplete_length.txt", LENGTH),
        ("responses_incomplete_filter.txt", FILTER),
        ("responses_failed.txt", FAILED),
        ("responses_tool_call.txt", TOOL_CALL),
        ("responses_tool_call_zero_args.txt", ZERO_ARGS),
    ] {
        assert!(
            !fixture.lines().any(|line| line.trim_end() == "data: [DONE]"),
            "{name} carries a `data: [DONE]` line, which the Responses API does not send"
        );
    }
}
```

- [ ] **Step 2: Run it and see it fail**

Run: `cargo test -p paigasus-helikon-providers-openai --all-features --test responses_streaming no_responses_fixture_carries_a_done_sentinel`
Expected: FAIL, naming `responses_reasoning_then_text.txt` (the first in the list).

- [ ] **Step 3: Remove the sentinel from five fixtures.** Delete the trailing `data: [DONE]` line from `responses_tool_call.txt`, `responses_reasoning_then_text.txt`, `responses_incomplete_length.txt`, `responses_incomplete_filter.txt` and `responses_failed.txt`. Each file must end with its terminal event line followed by one blank line (`}\n\n`), as `responses_tool_call_zero_args.txt` does.

- [ ] **Step 4: Run the guard test and the whole file**

Run: `cargo test -p paigasus-helikon-providers-openai --all-features --test responses_streaming`
Expected: PASS, every test, including `failed_event_terminates_stream_with_error`. If any other test changes result, stop and report.

- [ ] **Step 5: Rewrite the `responses_tool_call.txt` header** (spec, Design §1). Required content:
  - Remove the word "literal" from the `event:` paragraph. Change "unlike this directory's other `responses_*.txt` fixtures" to say that `responses_tool_call_zero_args.txt` keeps `event:` lines too.
  - Replace "Three kinds of trimming from the raw capture, all deliberate:" with "Edits from the raw capture" in three parts: **Removed:** the three existing items, text kept. **Added:** nothing. **Changed:** JSON key order, with the facts from spec Appendix A (file: `type`, `sequence_number` first, `item.type` before `item.id`, `response` keys in the sibling layout; captures on 2026-08-22 and 2026-11-21: `type` first, other event keys alphabetical, `item.id` before `item.type`; the 2026-08-20 order is not on record; the parser does not depend on key order).
  - A dated correction note: until 2026-11-21 the file ended with a `[DONE]` sentinel line and had a changed key order, and the header recorded neither; no capture of this endpoint contains the sentinel; see `docs/superpowers/specs/2026-11-21-sma-585-responses-done-sentinel-design.md`.
  - Keep every existing fact in the header that this list does not change.

- [ ] **Step 6: Rewrite the `responses_tool_call_zero_args.txt` header** (spec, Design §4). Rename "Two deliberate edits from the raw capture" to the Removed / Added / Changed structure: Removed: the opening events (existing item 1). Added: nothing. Changed: nothing. Rewrite existing item 2 so that it no longer says "Unlike every sibling fixture here" and no longer calls `responses_tool_call.txt`'s sentinel "unexplained" or "a follow-up"; say that SMA-585 removed it.

- [ ] **Step 7: Add headers to the four hand-authored fixtures** (spec, Design §2). Each header is `:` comment lines at the top, then one blank line, then the existing data. Content:
  - All four: HAND-AUTHORED under SMA-316, not captured; event shapes follow the async-openai `ResponseStreamEvent` types.
  - `responses_reasoning_then_text.txt`, `responses_incomplete_length.txt`: the stream ends on the terminal event with no sentinel, as the 2026-11-21 captures show (`response.completed` from `gpt-5-mini` with a reasoning summary; `response.incomplete` with `max_output_tokens`).
  - `responses_failed.txt`, `responses_incomplete_filter.txt`: no capture of this terminal event exists; the sentinel is omitted because no captured Responses stream ends with it.

- [ ] **Step 8: Run the whole crate's tests**

Run: `cargo test -p paigasus-helikon-providers-openai --all-features`
Expected: PASS. Then confirm no header line is a sentinel: `grep -n 'DONE' crates/paigasus-helikon-providers-openai/tests/fixtures/responses_*.txt` shows only lines that start with `:`.

- [ ] **Step 9: Commit**

```bash
git add crates/paigasus-helikon-providers-openai/tests/responses_streaming.rs crates/paigasus-helikon-providers-openai/tests/fixtures/responses_tool_call.txt crates/paigasus-helikon-providers-openai/tests/fixtures/responses_tool_call_zero_args.txt crates/paigasus-helikon-providers-openai/tests/fixtures/responses_reasoning_then_text.txt crates/paigasus-helikon-providers-openai/tests/fixtures/responses_incomplete_length.txt crates/paigasus-helikon-providers-openai/tests/fixtures/responses_incomplete_filter.txt crates/paigasus-helikon-providers-openai/tests/fixtures/responses_failed.txt
git commit -m "test(providers-openai): SMA-585 drop the done sentinel from responses fixtures"
```

### Task 2: `responses_wire.rs` stops sending the sentinel

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/tests/responses_wire.rs:46-55` (`sse()`)
- Test: `crates/paigasus-helikon-providers-openai/tests/responses_wire.rs`

**Interfaces:**
- Consumes: `text_delta_event(seq: u64, delta: &str) -> String`, `completed_event(seq: u64, input_tokens: u32, output_tokens: u32) -> String`, `sse(events: &[String]) -> String`, `user(text: &str) -> Item`, all in `responses_wire.rs`.
- Produces: nothing used by later tasks.

- [ ] **Step 1: Write the compatibility test** `trailing_done_sentinel_is_tolerated` (`#[tokio::test]`). Serve two bodies, each from its own `MockServer`, through `OpenAiModel::responses("gpt-5")` as `happy_path_text_completion` does:
  - `plain = sse(&[text_delta_event(1, "hi"), completed_event(2, 5, 1)])`
  - `with_done = format!("{plain}data: [DONE]\n\n")`

  Collect each stream to `Vec<Result<ModelEvent, _>>`. Assert:

```rust
assert!(with_done_events.iter().all(Result::is_ok), "{with_done_events:?}");
assert_eq!(format!("{plain_events:?}"), format!("{with_done_events:?}"));
```

  Doc comment: a compatibility check for OpenAI-compatible servers reached through `base_url` that may append `[DONE]`; it is NOT a claim that OpenAI sends one (it does not, see SMA-585). Factor the server-and-collect code into one local `async fn` if it would otherwise be written twice.

- [ ] **Step 2: Change `sse()`** so it no longer appends `data: [DONE]\n\n`. Its doc comment says: SSE wire representation of the given data events; no `[DONE]` sentinel, because the Responses API ends the stream on the terminal event (SMA-585).

- [ ] **Step 3: Run the file**

Run: `cargo test -p paigasus-helikon-providers-openai --all-features --test responses_wire`
Expected: PASS, every test. This test passes before and after Step 2, because tolerance already exists in async-openai (`client.rs:846-873`); it pins that tolerance, it does not drive a change.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-providers-openai/tests/responses_wire.rs
git commit -m "test(providers-openai): SMA-585 stop sending done in responses wire tests"
```

### Task 3: Conformance suite `openai_responses` module

**Files:**
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (module `openai_responses`, about lines 2780-3300)

**Interfaces:**
- Consumes: `responses_tool_call.txt` `sequence_number` values from Task 1 (unchanged by it): `output_item.added` = 2, argument deltas = 3-7, `function_call_arguments.done` = 8, `output_item.done` = 9, `response.completed` = 10.
- Produces: nothing used by later tasks.

- [ ] **Step 1: Remove `done()`** (about line 2946-2950) and its three uses: the `CleanStop` arm (about 3046), the `ToolCallCleanStop` script (about 3112), and the `("[DONE]", done())` row in `scan_finds_only_the_terminal_event` (about 3266). Delete that row. Do not touch the `gemini` module's own `done()`.

- [ ] **Step 2: Correct the doc comments:**
  - Module doc "Fixture provenance" (about 2804-2816): drop the `[DONE]` sentinel from the list of transcribed shapes; say `responses_reasoning_then_text.txt` is HAND-AUTHORED (SMA-316); say the Responses wire has no `[DONE]` (SMA-585).
  - `CleanStop` arm comment: "deltas, then the one event that carries the stop reason and usage, then a clean end of body". Add: on this wire `response.completed` is both the stop reason and the terminator; there is no separate sentinel. This keeps it consistent with `Scenario::CleanStop` in `tests/provider-stream-conformance/src/lib.rs:38-41`.
  - `frame()` doc (about 2890-2893): six `responses_*.txt` fixtures, two of which (`responses_tool_call.txt`, `responses_tool_call_zero_args.txt`) keep `event:` lines.
  - Replace each `responses_tool_call.txt` line citation (lines 78, 81-93, 96, 99 at about 2954, 2974, 2989, 3004) with its `sequence_number` from the Interfaces block, for example "`responses_tool_call.txt`'s `sequence_number: 2` frame".

- [ ] **Step 3: Check that no Responses reference remains**

Run: `awk 'NR>=2780 && NR<=3320' tests/provider-stream-conformance/tests/conformance.rs | grep -n 'DONE\|done()\|line [0-9]'`
Expected: only lines that say the wire has no `[DONE]`. No `done()` call, no fixture line-number citation.

- [ ] **Step 4: Run the suite**

Run: `cargo test -p paigasus-helikon-provider-stream-conformance`
Expected: PASS, with the same set of declined scenarios as before.

- [ ] **Step 5: Commit**

```bash
git add tests/provider-stream-conformance/tests/conformance.rs
git commit -m "test(providers): SMA-585 end the openai responses scripts on the terminal event"
```

### Task 4: The contributor rule

**Files:**
- Modify: `CONTRIBUTING.md` (new `## Wire-format fixtures` section; the builder picks the place, for example after `## Docstring coverage`)
- Modify: `CLAUDE.md` (section `## Fixture line endings`)

**Interfaces:**
- Consumes: the Removed / Added / Changed header structure from Task 1.
- Produces: nothing.

- [ ] **Step 1: Write the `CONTRIBUTING.md` section** with the five rules in spec Design §5: Label; Edits (Removed / Added / Changed, "nothing" when empty); Hand-authored shapes (name the source of each shape; do not copy framing from a different endpoint or API); No conformity edits; Where the header goes (`:` header for SSE and text; the loading test's doc comment for formats with no comment syntax; inline wire helpers cite their source; a synthetic addition in a test script is allowed when labelled, for example the Gemini `done()` in `tests/provider-stream-conformance/tests/conformance.rs`). Cite SMA-585 and `responses_tool_call.txt` as the example of what the rule prevents.

- [ ] **Step 2: Add one pointer line to `CLAUDE.md`** under `## Fixture line endings`: provenance headers for wire-format fixtures follow `CONTRIBUTING.md` → "Wire-format fixtures". Do not copy the rules.

- [ ] **Step 3: Lint**

Run: `npx markdownlint-cli2 CONTRIBUTING.md CLAUDE.md`
Expected: `0 error(s)`. (Run `npm ci` once first if `node_modules` is missing.)

- [ ] **Step 4: Commit** as two commits, because the scopes differ:

```bash
git add CONTRIBUTING.md
git commit -m "docs(contributing): SMA-585 add the wire-format fixture provenance rule"
git add CLAUDE.md
git commit -m "docs(claude): SMA-585 point to the fixture provenance rule"
```

### Task 5: Full gates

- [ ] **Step 1: Run the CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-providers-openai --all-features
cargo test -p paigasus-helikon-provider-stream-conformance
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
npx markdownlint-cli2
```

Expected: each exits 0.

- [ ] **Step 2: Final scan**

Run: `git grep -n 'data: \[DONE\]' -- crates/paigasus-helikon-providers-openai/tests/fixtures/responses_* crates/paigasus-helikon-providers-openai/tests/responses_wire.rs`
Expected: only the `with_done` line in `trailing_done_sentinel_is_tolerated`, its doc comment, and `:` header lines.

- [ ] **Step 3: Commit this plan** if it is not committed yet: `git add docs/superpowers/plans/2026-11-21-sma-585-responses-done-sentinel.md && git commit -m "docs(plan): SMA-585 plan the responses done-sentinel fix"`.
