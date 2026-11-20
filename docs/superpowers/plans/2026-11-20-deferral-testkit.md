# Deferral testkit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `assert_tool_calls_paired`, `RequestLog` and `with_late_transfer_results` one source that the core and runtime-tokio integration tests both use.

**Architecture:** A new unpublished workspace member `tests/deferral-testkit` (crate `paigasus-helikon-deferral-testkit`) holds the helpers. Core and runtime-tokio use it as a path-only dev-dependency and delete their local copies. No test name, script or checked value changes.

**Tech Stack:** Rust 1.94 workspace, cargo, release-plz, mdBook.

**Spec:** `docs/superpowers/specs/2026-11-20-deferral-testkit-design.md`

## Global Constraints

- Worktree root: `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-828`. Run every command there. Use worktree-absolute paths in Write/Edit.
- Never run `git checkout`, `git switch`, `git reset`, `git rebase` or `git stash`. To read `main`, use `git show main:<path>`.
- Work in the foreground. Do not end a turn while a cargo command runs.
- Crate: `name = "paigasus-helikon-deferral-testkit"`, `version = "0.0.0"`, `publish = false`; `release-plz.toml` entry `publish = false`, `release = false`.
- Dev-dependency line, exactly: `paigasus-helikon-deferral-testkit = { path = "../../tests/deferral-testkit" }` (no `version`).
- Do not add the crate to `[workspace.dependencies]`.
- Testkit `[dependencies]`: `paigasus-helikon-core`, `async-trait`, `futures-core`, `futures-util`, `anyhow`, each `{ workspace = true }`, and nothing more unless the compiler demands it.
- Testkit docs: every `pub` item has `///`; no code blocks; no intra-doc link to `MockModel` or to `CANCELLED_TOOL_CALL_TEXT` (name it in backticks only).
- Test files: only deletions, `use` changes, the `RecordingModel` → `RequestLog` rename and the two `CANCELLED` → `LEGACY_CANCEL_TEXT` legacy-assertion changes. No test name, script or checked value changes.
- Commit scopes: `chore(workspace)`, `test(core)`, `test(runtime-tokio)`, `docs(docs)`. No other scope. Every commit message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Stage files by explicit path. Never `git add -A` or `git add .`.
- Run `cargo fmt --all` before each commit.

## Review Focus

1. A moved helper body differs from `main` in logic (for example a lost `assert!`). Tests stay green, so only the body diff in Task 4 Step 3 catches it.
2. One crate still uses a local copy after its task. Only the mutation check in Task 4 Step 4 proves that both crates use the testkit.
3. A test becomes ignored or disappears from a binary. The list-and-count diff in Task 4 Step 2 catches it; the `Running` lines must have their hash suffix removed before the diff.
4. A broken intra-doc link in the testkit. It fails only the `docs` gate, so Task 1 Step 5 runs `cargo doc` with `-D warnings`.
5. The published manifest of core or runtime-tokio keeps the testkit dev-dependency. Task 4 Step 6 checks the normalized `Cargo.toml` in the `.crate` file.

---

### Task 1: Record the baseline and create the testkit crate

**Files:**
- Create: `tests/deferral-testkit/Cargo.toml`
- Create: `tests/deferral-testkit/src/lib.rs`
- Modify: `Cargo.toml:3` (workspace `members`)
- Modify: `release-plz.toml:29-50` (comment "three" → "four", new `[[package]]`)
- Modify: `Cargo.lock` (generated)

**Interfaces:**
- Produces (crate `paigasus_helikon_deferral_testkit`):
  - `pub fn assert_tool_calls_paired(messages: &[Item])`
  - `pub struct RequestLog` with `pub fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self>`, `pub fn requests(&self) -> Vec<ModelRequest>`, and `impl Model for RequestLog`
  - `pub async fn with_late_transfer_results(session: &Arc<MemorySession>) -> Arc<MemorySession>`
  - `pub const LEGACY_CANCEL_TEXT: &str = "tool call did not complete (run cancelled/timed out)";`

- [ ] **Step 1: Record the baseline (before any change)**

Set `S=/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/137f6f3f-ed7a-4334-aac4-fb89aa6109c7/scratchpad`. For each crate `C` in `paigasus-helikon-core` and `paigasus-helikon-runtime-tokio`, on the stable toolchain:

```bash
cargo test -p $C -- --list 2>&1 | grep -E '^\s+Running|: test$' | sed -E 's/ \(.*\)$//' > $S/base-list-$C.txt
cargo test -p $C 2>&1 | grep -E '^\s+Running|^test result:' | sed -E 's/ \(.*\)$//; s/; finished in .*//' > $S/base-count-$C.txt
```

Expected: four non-empty files. `base-count-*` has no `FAILED` line. `trybuild_ui` is in the core list.

- [ ] **Step 2: Add the crate to the workspace**

In `Cargo.toml:3`, append `"tests/deferral-testkit"` to `members`. Write `tests/deferral-testkit/Cargo.toml` in the form of `tests/workspace-lints/Cargo.toml`, with the name, version, `publish = false`, a `description` that starts with `Internal:`, the dependencies from Global Constraints, and `[lints] workspace = true`. In `release-plz.toml`, change "these three workspace members" to "these four workspace members", and add a `[[package]]` block for the crate with a two-line comment in the style of its neighbours.

- [ ] **Step 3: Write `tests/deferral-testkit/src/lib.rs`**

- A `//!` module doc: internal, unpublished; holds the test helpers that the core and runtime-tokio integration tests share; used only as a path-only dev-dependency.
- `assert_tool_calls_paired`: copy the body from `crates/paigasus-helikon-core/tests/common/mod.rs:429-469` byte for byte. Doc: the rule paragraph from lines 421-425, plus one line naming its self-test `pairing_oracle_rejects_a_call_after_a_result_while_a_call_waits` in core `tests/deferred_resume.rs`. Drop the "A copy is in…" lines.
- `RequestLog`: fields `scripts: Mutex<VecDeque<Vec<ModelEvent>>>` and `requests: Mutex<Vec<ModelRequest>>`. `invoke` pushes `request.clone()` into `requests` first, then pops the front script, maps an empty queue to `ModelError::Other(anyhow::anyhow!("no more scripted responses"))`, and returns `Box::pin(futures_util::stream::iter(script.into_iter().map(Ok)))`. It ignores `cancel`. `capabilities()` returns `ModelCapabilities::default()`. New doc: "A scripted model that keeps every request it gets. Each call pops one script; an empty queue gives an error."
- `LEGACY_CANCEL_TEXT`: doc says it is the fixed core 0.6.0 cancel text, that today it equals `CANCELLED_TOOL_CALL_TEXT` in core `session.rs`, and that it must not change to follow a later change of that constant.
- `with_late_transfer_results`: copy the body from `crates/paigasus-helikon-core/tests/deferred_resume.rs:658-698` byte for byte, except `CANCELLED` → `LEGACY_CANCEL_TEXT`. Doc: lines 650-655 of that file (stop after "in call order."), with no caller and no copy note.
- A private `fn text_of(content: &[ContentPart]) -> String`, copied from `crates/paigasus-helikon-core/tests/deferred_resume.rs` (identical to the runtime-tokio one).

- [ ] **Step 4: Build and lint the crate**

Run: `cargo build -p paigasus-helikon-deferral-testkit && cargo clippy -p paigasus-helikon-deferral-testkit --all-targets -- -D warnings`
Expected: both succeed with no warning. `Cargo.lock` now has a `paigasus-helikon-deferral-testkit` entry.

- [ ] **Step 5: Check the docs**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-deferral-testkit --no-deps`
Expected: success, no warning.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add Cargo.toml Cargo.lock release-plz.toml tests/deferral-testkit
git commit -m "chore(workspace): SMA-828 add the unpublished deferral testkit crate"
```

### Task 2: Use the testkit in the core tests

**Files:**
- Modify: `crates/paigasus-helikon-core/Cargo.toml` (`[dev-dependencies]`)
- Modify: `crates/paigasus-helikon-core/tests/common/mod.rs:387-469` (delete `RequestLog`, `assert_tool_calls_paired`)
- Modify: `crates/paigasus-helikon-core/tests/deferred_resume.rs:9,644-698,~1654`
- Modify: `crates/paigasus-helikon-core/tests/handoff_transfer_result.rs:11`
- Modify: `crates/paigasus-helikon-core/tests/subagent_propagation.rs:10`

**Interfaces:**
- Consumes: everything from Task 1's Produces block.

- [ ] **Step 1: Delete the local copies (the failing step)**

In `tests/common/mod.rs`, delete `RequestLog` (with its impls and doc) and `assert_tool_calls_paired` (with its doc). In `tests/deferred_resume.rs`, delete `with_late_transfer_results` and the `CANCELLED` constant with their doc comments.

- [ ] **Step 2: Confirm that the tests no longer compile**

Run: `cargo test -p paigasus-helikon-core --no-run`
Expected: FAIL with unresolved `RequestLog` / `assert_tool_calls_paired` / `with_late_transfer_results` / `CANCELLED`.

- [ ] **Step 3: Wire in the testkit**

Add to `[dev-dependencies]` in `crates/paigasus-helikon-core/Cargo.toml` the line from Global Constraints, with a comment: path-only so cargo omits it from the published manifest (as for `paigasus-helikon-macros`); for integration tests only, because code in `src/` under `#[cfg(test)]` is a different copy of core and gets error E0308. Change the imports in the three test files: take `assert_tool_calls_paired`, `RequestLog`, `with_late_transfer_results` and `LEGACY_CANCEL_TEXT` from `paigasus_helikon_deferral_testkit`, and keep the rest from `common`. In `deferred_resume.rs`, the assertion `[CANCELLED, CANCELLED]` becomes `[LEGACY_CANCEL_TEXT, LEGACY_CANCEL_TEXT]`. Remove the imports in `common/mod.rs` that became unused.

- [ ] **Step 4: Run the core tests**

Run: `cargo test -p paigasus-helikon-core && cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings`
Expected: all pass (including `trybuild_ui`), no warning.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/Cargo.toml crates/paigasus-helikon-core/tests/common/mod.rs crates/paigasus-helikon-core/tests/deferred_resume.rs crates/paigasus-helikon-core/tests/handoff_transfer_result.rs crates/paigasus-helikon-core/tests/subagent_propagation.rs Cargo.lock
git commit -m "test(core): SMA-828 use the deferral testkit helpers"
```

### Task 3: Use the testkit in the runtime-tokio tests

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/Cargo.toml` (`[dev-dependencies]`)
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs` (delete lines ~73-107 `RecordingModel`, ~420-469 `assert_tool_calls_paired`, ~1577-1624 `with_late_transfer_results`; imports; rename; line ~1692)

**Interfaces:**
- Consumes: everything from Task 1's Produces block.

- [ ] **Step 1: Delete the local copies (the failing step)**

Delete `RecordingModel` (struct, impls, doc), `assert_tool_calls_paired` (with its doc and the "Copy of…" lines) and `with_late_transfer_results` (with its doc). Keep `const CANCELLED` (line 26): `cancelled_resume_closes_the_deferral` (~line 1012) uses it for the live cancel text.

- [ ] **Step 2: Confirm that the tests no longer compile**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test deferred --no-run`
Expected: FAIL with unresolved `RecordingModel` / `assert_tool_calls_paired` / `with_late_transfer_results`.

- [ ] **Step 3: Wire in the testkit**

Add the dev-dependency line from Global Constraints to `crates/paigasus-helikon-runtime-tokio/Cargo.toml`, with a comment: path-only so cargo omits it from the published manifest. Import `assert_tool_calls_paired`, `RequestLog`, `with_late_transfer_results` and `LEGACY_CANCEL_TEXT` from `paigasus_helikon_deferral_testkit`. Rename every `RecordingModel` to `RequestLog`. In the legacy assertion near line 1692, `format!("tool_returned(t1, {CANCELLED})")` becomes `format!("tool_returned(t1, {LEGACY_CANCEL_TEXT})")`. Remove the imports that became unused (for example `Mutex`, `CancellationToken`, `ModelCapabilities`, `ModelError`, `ModelRequest`, `BoxStream`, `MockModel` — let the compiler decide).

- [ ] **Step 4: Run the runtime-tokio tests**

Run: `cargo test -p paigasus-helikon-runtime-tokio && cargo clippy -p paigasus-helikon-runtime-tokio --all-targets -- -D warnings`
Expected: all pass, no warning.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-runtime-tokio/Cargo.toml crates/paigasus-helikon-runtime-tokio/tests/deferred.rs Cargo.lock
git commit -m "test(runtime-tokio): SMA-828 use the deferral testkit helpers"
```

### Task 4: Update the mdBook and verify the branch

**Files:**
- Modify: `docs/book/src/reference/crates.md:3,10,45-51`

**Interfaces:**
- Consumes: the finished branch from Tasks 1-3.

- [ ] **Step 1: Update `crates.md`**

Line 3: "three further internal" → "four further internal". Line 10: add to the parenthesis that `-runtime-tokio` (and `core`) carry a path-only dev-dep on the internal `paigasus-helikon-deferral-testkit` for their tests, stripped from the published manifest. Line 45: "three further members" → "four further members". Add a bullet after line 49: `paigasus-helikon-deferral-testkit` (under `tests/deferral-testkit/`, `version = "0.0.0"`, `publish = false`) — the test helpers (the tool-pairing oracle, a request-recording model, the legacy-layout session rewrite) that the core and runtime-tokio integration tests share. Line 51: "All four are intentional non-published members, not stubs: three test harnesses and one shared test-helper library."
Run: `mdbook build docs/book && npx markdownlint-cli2`
Expected: both clean.

- [ ] **Step 2: Compare test lists and counts with the baseline**

Run Task 1 Step 1 again with `new-` in place of `base-`, then `diff $S/base-list-$C.txt $S/new-list-$C.txt` and `diff $S/base-count-$C.txt $S/new-count-$C.txt` for both crates.
Expected: no output from any of the four diffs.

- [ ] **Step 3: Diff the moved bodies against `main`**

Extract each function body from `git show main:<path>` and from `tests/deferral-testkit/src/lib.rs`, then diff:
- `assert_tool_calls_paired`: against `main:crates/paigasus-helikon-core/tests/common/mod.rs` and `main:crates/paigasus-helikon-runtime-tokio/tests/deferred.rs`.
- `with_late_transfer_results`: against `main:crates/paigasus-helikon-core/tests/deferred_resume.rs` and `main:crates/paigasus-helikon-runtime-tokio/tests/deferred.rs`.
- `text_of`: against `main:crates/paigasus-helikon-core/tests/deferred_resume.rs`.
Expected: the only differing lines are the `pub` keyword and `CANCELLED` → `LEGACY_CANCEL_TEXT`. For `RequestLog::invoke`, read it and confirm the order: push the request, then pop.
Then run `git diff -U0 main -- crates/paigasus-helikon-core/tests crates/paigasus-helikon-runtime-tokio/tests | grep '^+' | grep -v '^+++'`.
Expected: only `use` lines, `RequestLog` renames and the two `LEGACY_CANCEL_TEXT` lines.

- [ ] **Step 4: Mutation check (not committed)**

Add `panic!("SMA-828 mutation");` as the first line of `assert_tool_calls_paired` in the testkit. Run `cargo test -p paigasus-helikon-core --test handoff_transfer_result` and `cargo test -p paigasus-helikon-runtime-tokio --test deferred`.
Expected: both FAIL with `SMA-828 mutation`. Then remove the line and confirm `git diff --stat tests/deferral-testkit` is empty.

- [ ] **Step 5: No copy notes remain**

Run:
```bash
git grep -nE "Keep the two copies|Copy of .assert_tool_calls_paired|A copy is in .crates/|\(A copy of the" -- crates tests
git grep -n "runtime-tokio/tests/deferred.rs" -- crates/paigasus-helikon-core
git grep -nE "core/tests/(common/mod|deferred_resume)\.rs" -- crates/paigasus-helikon-runtime-tokio
```
Expected: no output (exit status 1) from all three.

- [ ] **Step 6: Commit the book, then check packaging**

```bash
git add docs/book/src/reference/crates.md
git commit -m "docs(docs): SMA-828 list the deferral testkit as a workspace member"
cargo package -p paigasus-helikon-core --no-verify
cargo package -p paigasus-helikon-runtime-tokio --no-verify
```
Then `tar -xzOf target/package/paigasus-helikon-core-<ver>.crate paigasus-helikon-core-<ver>/Cargo.toml | grep -c deferral-testkit` and the same for runtime-tokio (read `<ver>` from each `Cargo.toml`).
Expected: `0` for both.

- [ ] **Step 7: The full CI gates**

Run:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
```
Expected: all succeed. (The worktree path avoids the macOS NATIVE_ROOTS bedrock red herring only under the scratchpad; if exactly the bedrock tests fail with a native-roots error, report it and run `cargo test -p paigasus-helikon-providers-bedrock` from a scratchpad worktree to confirm that `main` fails the same way.)
