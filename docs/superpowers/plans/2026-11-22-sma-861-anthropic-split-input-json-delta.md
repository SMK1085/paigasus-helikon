# SMA-861 Split `input_json_delta` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The two `anthropic` tool scripts in the stream conformance suite send the captured split `input_json_delta` (an empty first delta, then three fragments).

**Architecture:** One `const CAPTURED_ARGS` in the `anthropic` module of `tests/provider-stream-conformance/tests/conformance.rs`. Both tool scripts push one `input_json_delta` frame for each value. The module doc and comments change to match. No harness or translator change.

**Tech Stack:** Rust, `tokio` tests, `serde_json`.

**Spec:** `docs/superpowers/specs/2026-11-22-sma-861-anthropic-split-input-json-delta-design.md`

## Global Constraints

- Fragment values, verbatim: `""`, `{"city"`, `: "Pari`, `s"}` (Rust: `&["", "{\"city\"", ": \"Pari", "s\"}"]`).
- Capture citations: `thinking_text_tool_use.txt` lines 143-153, `empty_thinking_tool_use.txt` lines 94-104, `redacted_thinking_tool_use.txt` lines 95-105.
- Tool name stays `get_weather`, tool id stays `tu_conformance_0`. No `caller` key, no `ping`.
- Change only the `anthropic` module. Do not change `src/lib.rs` or `stream.rs` (the mutation check reverts its edits).
- Doc text is in ASD-STE100 Simplified Technical English. Keep labels CAPTURED / HAND-AUTHORED in capitals.
- Commit prefix: `test(providers): SMA-861 <lowercase verb> ...`. Stage files by explicit path; never `git add -A`.
- Work in the foreground. Do not end the task until each command has a final status.

## Review Focus

- The empty first delta is the name-carrying one. A reviewer must confirm the script does not drop or merge it (mutation (b) in Task 1 pins this).
- A translator that names every delta must now fail (mutation (a) in Task 1). The old one-delta script could not catch it.
- Stale prose: the section title "never splits" is referenced in three places. Task 2's grep step pins this.
- Provenance labels: the two other captures move out of the "no helper copies a shape from" list (Task 2).
- No other test in the module depends on the Berlin argument text (the spec checked lines 2206, 2239, 2269, 2422; Task 1 changes all four).

---

### Task 1: Script the captured fragments

**Files:**
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (the `anthropic` module: constants near line 1848, scripts at about 2202-2250, `request()` at about 2269, `scan_finds_only_a_populated_stop_reason` at about 2421)

**Interfaces:**
- Produces: `const CAPTURED_ARGS: &[&str]` in `mod anthropic`. Task 2 cites it in docs.
- Consumes: existing `input_json_delta(&str)` and `input_json_delta_at(u32, &str)`.

- [ ] **Step 1: Prove the gap (baseline mutation (a))**

  In `crates/paigasus-helikon-providers-anthropic/src/stream.rs` lines 175-180, make the translator emit `Some(name.clone())` on every `input_json_delta` (ignore `name_emitted`). Run:
  `cargo test -p paigasus-helikon-provider-stream-conformance --test conformance anthropic::conforms`
  Expected: PASS (the one-delta script cannot catch this). Keep the mutation in place for Step 3.

- [ ] **Step 2: Add `CAPTURED_ARGS` and change both scripts**

  - Add `const CAPTURED_ARGS: &[&str] = &["", "{\"city\"", ": \"Pari", "s\"}"];` next to `TOOL_USE_ID`, with a `///` doc that says: the four `input_json_delta` values that each of the three CAPTURED SMA-840 fixtures sends for its tool call, in order; the first is empty; it cites the three line ranges above.
  - `ToolCallCleanStop`: replace `input_json_delta("{\"city\":\"Berlin\"}")` with one `input_json_delta(a)` per `a` in `CAPTURED_ARGS`, between `tool_use_start` and `content_block_stop`.
  - `ThinkingToolCallCleanStop`: same with `input_json_delta_at(1, a)`.
  - `request()`: user message becomes `"What is the weather in Paris?"`.
  - `scan_finds_only_a_populated_stop_reason`: the `"input_json_delta"` entry uses `input_json_delta(CAPTURED_ARGS[1])`.

- [ ] **Step 3: Run with mutation (a) in place**

  Run: `cargo test -p paigasus-helikon-provider-stream-conformance --test conformance anthropic::conforms`
  Expected: FAIL with `ToolNameNotExactlyOnce` for `tu_conformance_0`, count 4. Record the output for the PR body.

- [ ] **Step 4: Mutations (b) and (c)**

  Revert (a). Apply (b): skip the push of an empty-`partial_json` `ToolCallDelta` but still set `name_emitted = true`. Expected: FAIL with the floor message "no ToolCallDelta carried the tool name". Revert. Apply (c): at the top of the `InputJsonDelta` arm for a real tool block, skip the whole delta when `partial_json` is empty (so `name_emitted` stays false). Expected: PASS. Record all three results. Revert, then confirm `git diff --stat crates/` is empty.

- [ ] **Step 5: Run the crate and the gates**

  Run: `cargo test -p paigasus-helikon-provider-stream-conformance` — Expected: all PASS.
  Run: `cargo fmt --all -- --check` and `cargo clippy --workspace --all-features --all-targets -- -D warnings` — Expected: clean.

- [ ] **Step 6: Commit**

  ```bash
  git add tests/provider-stream-conformance/tests/conformance.rs
  git commit -m "test(providers): SMA-861 send the captured split input_json_delta"
  ```

### Task 2: Rewrite the provenance docs

**Files:**
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (the `anthropic` module doc and comments; line numbers on `94bcf64a`, they shift after Task 1)

**Interfaces:**
- Consumes: `CAPTURED_ARGS` from Task 1 (link it as [`CAPTURED_ARGS`] in the module doc).

- [ ] **Step 1: Edit each stale passage**, per spec "Design" step 6:
  - 1667-1671: the three CAPTURED SMA-840 fixtures ground the `input_json_delta` values; `tool_use_start`'s shape stays HAND-AUTHORED.
  - 1712-1721 (SMA-317 bullet): they ground only `tool_use_start`'s shape; remove the old-title reference.
  - 1722-1734 (`thinking_text_tool_use.txt` bullet): also grounds [`CAPTURED_ARGS`].
  - 1744-1748: move `empty_thinking_tool_use.txt` and `redacted_thinking_tool_use.txt` out of the "no helper copies a shape from" list; say they ground [`CAPTURED_ARGS`].
  - 1805-1826: new section "Tool scripts send the captured split `input_json_delta`". Content: the captured shape and the three line ranges; the translator names the first delta, also when empty; the floor at `src/lib.rs` (the "no ToolCallDelta carried the tool name" check) catches zero name-carrying deltas and assertion 7 catches more than one; the suite does not check which delta carries the name. Remove the SMA-845 audit history (1819-1822).
  - 2065-2070 (`input_json_delta` helper doc): one `input_json_delta` fragment; remove "whole argument JSON", the old-title reference and "never called twice".
  - 2195-2201 (`ToolCallCleanStop` comment): the arguments arrive as the four [`CAPTURED_ARGS`] fragments; the single-call shape of `parallel_tool_use.txt` applies only to the block start.
  - 2223-2230 (`ThinkingToolCallCleanStop` comment): remove "one whole `input_json_delta`, not an empty one and three fragments" from the differences.

- [ ] **Step 2: Grep for stale text**

  Run: `grep -n -E "never splits|one whole|whole argument|never called twice" tests/provider-stream-conformance/tests/conformance.rs`
  Expected: no hit inside `mod anthropic` or its module doc (hits in other modules are fine; read each one).

- [ ] **Step 3: Tests and format**

  `cargo doc` does not document integration tests, so no gate checks the [`CAPTURED_ARGS`] links. Check by reading that each bracketed name exists in `mod anthropic`.
  Run: `cargo test -p paigasus-helikon-provider-stream-conformance` and `cargo fmt --all -- --check` — Expected: PASS / clean.

- [ ] **Step 4: Commit**

  ```bash
  git add tests/provider-stream-conformance/tests/conformance.rs
  git commit -m "test(providers): SMA-861 describe the captured split in the anthropic docs"
  ```
