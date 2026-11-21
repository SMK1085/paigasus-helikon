# SMA-845 Fixture Provenance Audit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make each in-scope wire-format fixture and wire helper state its provenance under `CONTRIBUTING.md` → "Wire-format fixtures", and add a test that stops new drift.

**Architecture:** A pure header-check function in `tests/workspace-lints` plus a walker test with a shrinking allowlist. Then header and doc-comment edits, crate by crate, each of which removes its files from the allowlist. Phase A (Tasks 1-8) runs on `main` now. Phase B (Tasks 9-12) runs only after SMA-840 merges.

**Tech Stack:** Rust 1.94, cargo test, SSE fixture files, `eventsource-stream` 0.2.3.

**Spec:** `docs/superpowers/specs/2026-11-21-sma-845-fixture-provenance-audit-design.md`. Each task names the spec decisions (D1-D11) it implements. Read those sections before you start the task. The spec is the source of the header content; this plan does not repeat it.

## Global Constraints

- **No fixture data change.** Only the leading `:` header block of a fixture changes. The one exception needs a check with Sven first (spec, "Data changes").
- **Every claim has evidence.** Each statement in a header cites a ticket, PR, design-doc line or fixture line. If the record is silent, write "not recorded" and the evidence that exists (spec D2). Never write "nothing" for a part that nobody can check.
- **No commit hashes in headers.** Cite PRs and tickets (spec, Commits).
- **Header form** is spec D1. The first header line is `: Provenance: CAPTURED ...` or `: Provenance: HAND-AUTHORED ...`.
- **`//` on a literal inside a function, `///` on a function** (spec D10). A `///` on a `let` fails clippy.
- **Commit types:** `test(providers)`, `test(tools)`, `docs(contributing)`, `docs(plan)`. Message format `<type>(<scope>): SMA-845 <lowercase subject>`. End each message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Git safety:** work only in `/Users/smaschek/dev/paigasus/paigasus-helikon/.worktrees/sma-845`. Never run `git checkout`, `switch`, `reset`, `stash`, `rebase` or `merge` except where Task 9 says so. Never `git add -A`; add explicit paths. Read the SMA-840 branch only with `git show <rev>:<path>`.
- **Work in the foreground.** Do not background a cargo run. Do not end a task before its commands have a final result.
- **Style:** ASD-STE100 Simplified Technical English in all new header and doc text: short sentences, active voice, no idiom.

## Review Focus

1. **A header line without its leading `:`.** Every test still passes, because the parser ignores an unknown field. Task 1's walker test pins it.
2. **A final newline added to `body_cut_inside_message_stop.txt`.** No streaming test sees it. Task 1's walker test pins its last byte.
3. **A claim that the record does not support** (the defect this audit exists to remove; the first spec draft had one). No test can pin it. Each task's reviewer checks every new claim against its cited evidence, and each task lists its claims in a claim-to-evidence table in its report.
4. **A data change hidden in a header edit.** Task 8 (and Task 12) run the data-unchanged check over every fixture.
5. **A stale allowlist entry** (a fixed file still allowlisted). Task 1's walker test fails on an allowlisted file that already passes.

---

## Phase A (on `main`, now)

### Task 1: Fixture header check test

**Files:**
- Create: `tests/workspace-lints/src/fixture_header.rs`
- Modify: `tests/workspace-lints/src/lib.rs` (add `pub mod fixture_header;` near the top, after the crate doc)
- Create: `tests/workspace-lints/tests/fixture_provenance_header.rs`

**Interfaces:**
- Produces: `paigasus_helikon_workspace_lints::fixture_header::check(text: &str) -> Result<(), HeaderError>`.
- Produces: `pub enum HeaderError { MissingProvenanceLine, UnknownLabel, NonCommentHeaderLine { line: usize } }` (derive `Debug, PartialEq, Eq`).
- Produces: the const `ALLOWLIST: &[&str]` in the walker test, with repo-relative paths. Tasks 3-5, 10 and 11 remove entries from it.

- [ ] **Step 1: Write the failing unit tests** in `fixture_header.rs` (`#[cfg(test)] mod tests`):
  - `captured_header_passes`: `check(": Provenance: CAPTURED x\n: more\n\ndata: {}\n") == Ok(())`
  - `hand_authored_header_passes`: `check(": Provenance: HAND-AUTHORED x\nevent: a\ndata: {}\n") == Ok(())`
  - `missing_header_fails`: `check("data: {}\n") == Err(HeaderError::MissingProvenanceLine)`
  - `lowercase_label_fails`: `check(": Provenance: captured 2026\ndata: {}\n") == Err(HeaderError::UnknownLabel)`
  - `line_without_colon_fails`: `check(": Provenance: CAPTURED x\nProvenance continues\ndata: {}\n") == Err(HeaderError::NonCommentHeaderLine { line: 2 })` (1-based)
  - `only_lines_before_first_frame_are_checked`: `check(": Provenance: CAPTURED x\nevent: a\n# --- turn 2 ---\nevent: b\n") == Ok(())`

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p paigasus-helikon-workspace-lints --lib fixture_header`
Expected: FAIL (`check` not defined).

- [ ] **Step 3: Implement `check`.** Rules: the first line must start with `: Provenance: `, else `MissingProvenanceLine`. The text after that prefix must start with `CAPTURED` or `HAND-AUTHORED`, else `UnknownLabel`. Each later line up to (not including) the first line that starts with `event:` or `data:` must start with `:` or be empty, else `NonCommentHeaderLine`. Give the module a `//!` doc that cites `CONTRIBUTING.md` → "Wire-format fixtures" and SMA-845, and `///` docs on each pub item.

- [ ] **Step 4: Run the unit tests**

Run: `cargo test -p paigasus-helikon-workspace-lints --lib fixture_header`
Expected: PASS (6 tests).

- [ ] **Step 5: Write the walker test** `tests/workspace-lints/tests/fixture_provenance_header.rs`. Reuse the `repo_root()` pattern and the symlink-safe walk of `tests/tracing_target_syntax.rs`. Walk `crates/*/tests/fixtures/` recursively. Do not walk the repo root (`.claude/worktrees/` holds other checkouts). Collect files that end in `.txt` or `.sse`. Two tests:
  - `every_fixture_has_a_provenance_header`: for each file not in `ALLOWLIST`, `check` must be `Ok`. For each file in `ALLOWLIST`, `check` must be `Err` (a passing allowlisted file is a stale entry: the message tells the reader to remove it). Collect all failures and assert once, so one run lists them all. Each `ALLOWLIST` entry must exist on disk.
  - `cut_fixture_keeps_its_missing_final_newline`: the last byte of `crates/paigasus-helikon-providers-anthropic/tests/fixtures/body_cut_inside_message_stop.txt` is not `b'\n'`. The message cites the SMA-531 design.
  
  Initial `ALLOWLIST` (the files that fail `check` on `main` today):
  - the nine `crates/paigasus-helikon-providers-anthropic/tests/fixtures/*.txt`
  - `crates/paigasus-helikon-providers-gemini/tests/fixtures/parallel_signed.sse`
  - `crates/paigasus-helikon-providers-litellm/tests/fixtures/text_then_trailing_usage.txt`, `truncated_no_finish.txt`, `unknown_finish_reason.txt`, `unparseable_frame.txt`
  - `crates/paigasus-helikon-providers-openai/tests/fixtures/chat_text_usage_trailing.txt` (its label is lowercase "captured")
  
  Put a doc comment above `ALLOWLIST`: it may only shrink, and phase B empties it.

- [ ] **Step 6: Run the walker test**

Run: `cargo test -p paigasus-helikon-workspace-lints --test fixture_provenance_header`
Expected: PASS (2 tests). If a file not listed above fails, or a listed file passes, stop and report: the spec's inventory is wrong.

- [ ] **Step 7: Commit**

```bash
git add tests/workspace-lints/src/fixture_header.rs tests/workspace-lints/src/lib.rs tests/workspace-lints/tests/fixture_provenance_header.rs
git commit -m "test(lints): SMA-845 check the provenance header of each wire-format fixture"
```

Note: `lints` is in the `.versionrc` scope allowlist. `tests/workspace-lints` has `publish = false`, so this commit gives no release bump.

### Task 2: Rule 2 sentence and the worked example

Spec: D2, Q1.

**Files:**
- Modify: `CONTRIBUTING.md` (rule 2 in "Wire-format fixtures", about line 207)
- Modify: `crates/paigasus-helikon-providers-openai/tests/fixtures/responses_tool_call.txt` (the `Added:` part, about line 81)

- [ ] **Step 1: Add the D2 sentence to rule 2**, verbatim from the spec: "Write "nothing" only when the record shows that a part is empty. If the raw capture is not available, write "not recorded" for a part that you cannot check, and say what evidence exists." It replaces the clause "Write "nothing" for an empty part."
- [ ] **Step 2: Correct the worked example.** Change `: Added: nothing.` to a "not recorded" statement. Give the evidence that exists: no raw copy of the 2026-08-20 capture exists (line 99), and the later captures of 2026-08-22 and 2026-11-21 (SMA-585) contain no line that this fixture lacks, if the SMA-585 design shows that; otherwise state only that no raw copy exists. Check the SMA-585 design (`docs/superpowers/specs/2026-11-21-sma-585-responses-done-sentinel-design.md`) before you write the second clause.
- [ ] **Step 3: Check `responses_tool_call_zero_args.txt`** for the same defect ("nothing" for a part with no raw copy). Do not edit it. If it has the defect, report it (it goes into the PR body as a follow-up).
- [ ] **Step 4: Verify**

Run: `npx markdownlint-cli2 CONTRIBUTING.md && cargo test -p paigasus-helikon-providers-openai --test responses_streaming`
Expected: 0 markdownlint issues; tests PASS.

- [ ] **Step 5: Commit** both files. Use two commits: `docs(contributing): SMA-845 say when to write not recorded in a fixture edit list` and `test(providers): SMA-845 mark the unchecked added part of the worked example as not recorded`.

### Task 3: LiteLLM fixtures

Spec: D5, D11 (for `streaming.rs`), and the D10 rows for LiteLLM.

**Files:**
- Modify: the six files in `crates/paigasus-helikon-providers-litellm/tests/fixtures/`
- Modify: `crates/paigasus-helikon-providers-litellm/tests/streaming.rs` (module doc lines 6-7, comment at lines 70-72, inline body near line 54)
- Modify: `crates/paigasus-helikon-providers-litellm/tests/litellm_wire.rs` (`sse_ok()` and the inline bodies)
- Modify: `crates/paigasus-helikon-providers-litellm/tests/cancellation.rs` (the `[DONE]` body)
- Modify: `tests/workspace-lints/tests/fixture_provenance_header.rs` (remove the four LiteLLM entries)

- [ ] **Step 1: Remove the four LiteLLM entries from `ALLOWLIST`** and run `cargo test -p paigasus-helikon-workspace-lints --test fixture_provenance_header`. Expected: FAIL, naming those four files.
- [ ] **Step 2: Write the six headers** per the D5 table. Read the cited lines first: SMA-451 design lines 783 and 1136-1144 and Appendix B config; SMA-547 design lines 44-131. Keep the two existing CAPTURED headers and extend them.
- [ ] **Step 3: Correct `streaming.rs`** per D11: the module doc and the lines 70-72 comment name which fixtures are captured and which are hand-authored. Add a `//` source line to the inline body near line 54.
- [ ] **Step 4: Annotate the D10 items** in `litellm_wire.rs` and `cancellation.rs`.
- [ ] **Step 5: Verify**

Run: `cargo test -p paigasus-helikon-providers-litellm && cargo test -p paigasus-helikon-workspace-lints --test fixture_provenance_header`
Expected: PASS.

- [ ] **Step 6: Commit** with `test(providers): SMA-845 state the provenance of the litellm fixtures and wire helpers`. Report a claim-to-evidence table.

### Task 4: OpenAI Chat fixtures and wire helpers

Spec: D6, and the D10 rows for OpenAI and the facade.

**Files:**
- Modify: the four `crates/paigasus-helikon-providers-openai/tests/fixtures/chat_*.txt`
- Modify: `crates/paigasus-helikon-providers-openai/tests/chat_wire.rs` (SSE body lines 30-44: relabel HAND-AUTHORED; the 429 body)
- Modify: `crates/paigasus-helikon-providers-openai/tests/chat_streaming.rs` (inline body near line 225)
- Modify: `crates/paigasus-helikon-providers-openai/tests/cancellation.rs` (`[DONE]` body; body near line 98)
- Modify: `crates/paigasus-helikon-providers-openai/tests/responses_wire.rs` (`text_delta_event`, `completed_event`, `sse`)
- Modify: `crates/paigasus-helikon/tests/openai_litellm_message_parity.rs` (body near line 147)
- Modify: `tests/workspace-lints/tests/fixture_provenance_header.rs` (remove `chat_text_usage_trailing.txt`)

- [ ] **Step 1: Remove the `chat_text_usage_trailing.txt` entry** and run the walker test. Expected: FAIL on that file.
- [ ] **Step 2: Rewrite the four headers** per the D6 table. Read SMA-522 design lines 255-310 and 439-488, and the SMA-316 plan line 3170. Do not write a full response id: the trace shows only `chatcmpl-1cc5e8c0-`.
- [ ] **Step 3: Annotate the D10 items.** For the `responses_wire.rs` helpers, name the `responses_*.txt` fixture whose shape each copies and that fixture's label.
- [ ] **Step 4: Verify**

Run: `cargo test -p paigasus-helikon-providers-openai && cargo test -p paigasus-helikon --all-features --test openai_litellm_message_parity && cargo test -p paigasus-helikon-workspace-lints --test fixture_provenance_header`
Expected: PASS.

- [ ] **Step 5: Commit** with `test(providers): SMA-845 state the provenance of the openai chat fixtures and wire helpers`. Report a claim-to-evidence table.

### Task 5: Gemini fixture and wire helpers

Spec: D4, and the D10 rows for Gemini.

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/tests/fixtures/parallel_signed.sse`
- Modify: `crates/paigasus-helikon-providers-gemini/tests/gemini_wire.rs` (`sse_ok()`; the 429 body, about line 86)
- Modify: `crates/paigasus-helikon-providers-gemini/tests/gemini_streaming.rs` (the inline `data:` bodies)
- Modify: `tests/workspace-lints/tests/fixture_provenance_header.rs` (remove the Gemini entry)

- [ ] **Step 1: Remove the Gemini entry** and run the walker test. Expected: FAIL on `parallel_signed.sse`.
- [ ] **Step 2: Write the header** per D4. Read SMA-827 design lines 266-335 and plan Task 7.
- [ ] **Step 3: Annotate the D10 items.** The `gemini_streaming.rs` inline bodies are cited by `conformance.rs` (lines 2196-2214) as sources; label them accurately, because Task 7 depends on these labels.
- [ ] **Step 4: Verify**

Run: `cargo test -p paigasus-helikon-providers-gemini && cargo test -p paigasus-helikon-workspace-lints --test fixture_provenance_header`
Expected: PASS. (`src/translate/mod.rs` reads the fixture and skips lines that do not start with `data: `.)

- [ ] **Step 5: Commit** with `test(providers): SMA-845 state the provenance of the gemini fixture and wire helpers`. Report a claim-to-evidence table.

### Task 6: Tools JSON fixtures

Spec: D7, D11.

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/web/backends/brave.rs` (`parses_brave_fixture`, `parse_brave_respects_limit`)
- Modify: `crates/paigasus-helikon-tools/src/web/backends/tavily.rs` (`parses_tavily_fixture`)

- [ ] **Step 1: Add the `///` doc comments** per D7. Cite SMA-412 design lines 307-318 and the contrary line 406 (D11).
- [ ] **Step 2: Verify**

Run: `cargo test -p paigasus-helikon-tools --all-features web::backends`
Expected: PASS.

- [ ] **Step 3: Commit** with `test(tools): SMA-845 state the provenance of the brave and tavily fixtures`.

### Task 7: Conformance suite, non-Anthropic modules

Spec: D9 items 1-7. Depends on Tasks 3-5 (it cites their labels).

**Files:**
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (crate doc; modules `bedrock`, `openai_chat`, `litellm`, `gemini`, `openai_responses`)

- [ ] **Step 1: Labels (D9.1).** In each non-Anthropic "Fixture provenance" section, give each cited source the label that its fixture header now states.
- [ ] **Step 2: Wire-claim sweep (D9.2).** Fix each listed claim except 1633 and 1689 (Anthropic, phase B). Then search the five modules for "real", "observed", "captured", "sends" and "emit", and check each further hit against a capture.
- [ ] **Step 3: `openai_chat` (D9.3), changed items (D9.4), key order (D9.5), `bedrock` (D9.6), `gemini::done` (D9.7).** Keep all helper data unchanged.
- [ ] **Step 4: Verify**

Run: `cargo test -p paigasus-helikon-provider-stream-conformance && cargo clippy -p paigasus-helikon-provider-stream-conformance --all-targets -- -D warnings`
Expected: PASS, no warnings.

- [ ] **Step 5: Commit** with `test(providers): SMA-845 label the sources of the conformance wire helpers`. Report a claim-to-evidence table.

### Task 8: Phase A verification

**Files:** none changed, unless a check fails.

- [ ] **Step 1: Data-unchanged check.** For each fixture changed on this branch, run this with `bash` (not zsh):

```bash
strip() { awk 'f || (!/^:/ && !/^$/) { f=1; print }'; }
for f in $(git diff --name-only origin/main -- 'crates/*/tests/fixtures/*'); do
  cmp -s <(git show "origin/main:$f" | strip) <(strip < "$f") || echo "DATA CHANGED: $f"
done
```

Expected: no output. (`responses_tool_call.txt` changes only inside its header.)

- [ ] **Step 2: Full gates**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-features --all-targets -- -D warnings && cargo test --workspace --all-features && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps && npx markdownlint-cli2`
Expected: all PASS.

- [ ] **Step 3: Commit the plan** if it is not committed: `docs(plan): SMA-845 add the fixture provenance audit plan`.

---

## Phase B (only after SMA-840 is merged to `main`)

Do not start phase B until `gh pr list --state merged --search SMA-840` shows the SMA-840 PR as merged. Before that, phase A is pushed and the PR is a draft.

### Task 9: Rebase onto SMA-840 and fix its loader

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/mod.rs` (`no_thinking_turn_writes_no_provider_data`)

- [ ] **Step 1: Rebase** with `git fetch origin && git rebase origin/main`. Resolve conflicts in `conformance.rs` by keeping both sides' content (SMA-840's new Anthropic text and this branch's non-Anthropic edits).
- [ ] **Step 2: Re-run the walker test.** Expected: FAIL on the five SMA-840 fixtures (not allowlisted) plus the nine allowlisted Anthropic files, which still fail as expected. This confirms the test sees the new files.
- [ ] **Step 3: Make the loader skip `:` lines.** In `no_thinking_turn_writes_no_provider_data`, skip each line that starts with `:` before the split logic. This is a test-only edit (spec, Constraints).
- [ ] **Step 4: Verify** `cargo test -p paigasus-helikon-providers-anthropic` (PASS) and `cargo test -p paigasus-helikon-provider-stream-conformance` (PASS).
- [ ] **Step 5: Commit** with `test(providers): SMA-845 let the no-thinking turn test skip fixture comment lines`.

### Task 10: Anthropic and SMA-840 fixture headers

Spec: D3, Q2.

**Files:**
- Modify: the nine SMA-317/SMA-531 fixtures and the five SMA-840 fixtures in `crates/paigasus-helikon-providers-anthropic/tests/fixtures/`
- Modify: `tests/workspace-lints/tests/fixture_provenance_header.rs` (remove the nine Anthropic entries; `ALLOWLIST` becomes empty)

- [ ] **Step 1: Empty `ALLOWLIST`** and run the walker test. Expected: FAIL on 14 files.
- [ ] **Step 2: Write the nine SMA-317/SMA-531 headers** per D3. Do the two-direction key diff against the comparison set on `main`. Keep `# --- turn 2 ---` out of the header of `tool_use_then_continuation.txt`.
- [ ] **Step 3: Write the five SMA-840 headers** from the SMA-840 design, plan and PR body. Two of them (`interleaved_thinking.txt`, `unsigned_thinking_tool_use.txt`) are synthetic: label them HAND-AUTHORED and name the capture each copies. The three transcriptions are CAPTURED with Removed, Added and Changed.
- [ ] **Step 4: Verify** `cargo test -p paigasus-helikon-providers-anthropic && cargo test -p paigasus-helikon-workspace-lints --test fixture_provenance_header`. Expected: PASS. Check that the last byte of `body_cut_inside_message_stop.txt` is not `0x0a` (the walker test does this).
- [ ] **Step 5: Commit** with `test(providers): SMA-845 state the provenance of the anthropic fixtures`. Report a claim-to-evidence table.

### Task 11: Anthropic conformance module and wire helpers

Spec: D9.2 (lines 1633, 1689 and the Anthropic sweep), D9.8, and the D10 rows for Anthropic.

**Files:**
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (module `anthropic`, SMA-840 version)
- Modify: `crates/paigasus-helikon-providers-anthropic/tests/messages_wire.rs`, `prompt_caching.rs`, `structured_output.rs`

- [ ] **Step 1: Labels and sweep** for the `anthropic` module per D9.2 and D9.8, including the SMA-840 helpers `thinking_start`, `thinking_delta`, `signature_delta` and `tool_use_start_at`.
- [ ] **Step 2: Annotate the D10 Anthropic items.**
- [ ] **Step 3: Verify** `cargo test -p paigasus-helikon-providers-anthropic && cargo test -p paigasus-helikon-provider-stream-conformance`. Expected: PASS.
- [ ] **Step 4: Commit** with `test(providers): SMA-845 label the sources of the anthropic wire helpers`. Report a claim-to-evidence table.

### Task 12: Phase B verification

- [ ] **Step 1:** Run Task 8 Step 1 and Step 2 again (data unchanged, all gates).
- [ ] **Step 2:** Check that each SMA-840 path cited in a header exists on `main`: `git ls-files <path>`.
- [ ] **Step 3:** Mark the PR ready for review.
