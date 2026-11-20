# SMA-826 Shared Core Test Helpers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `assert_tool_calls_paired`, `DeferB` and `RequestLog` one source in `crates/paigasus-helikon-core/tests/common/mod.rs`, and point the runtime-tokio oracle copy at it.

**Architecture:** A pure move of test helpers into the shared test module that 25 core test binaries already include with `#[path = "common/mod.rs"] mod common;`. No test changes what it checks. No production code changes.

**Tech Stack:** Rust 1.94, cargo integration tests, tokio.

**Spec:** `docs/superpowers/specs/2026-11-20-core-shared-test-helpers-design.md`

## Global Constraints

- Work only in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-826`. Use worktree-absolute paths. Never run `git checkout`, `git switch`, `git reset`, `git stash` or any command that moves HEAD or another branch.
- Code commits use `test(core): SMA-826 <lowercase subject>` (Task 3 uses `test(runtime-tokio)`, because it changes only that crate).
- Shell state does not persist between commands. Each command block sets `cd` and `S` again where it needs them. End each commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Stage explicit paths only, never `git add -A`.
- The oracle code stays the same character for character. Only `pub` and comments may change.
- No change under `src/`, no `Cargo.toml` change, no `exclude` key.
- Run every cargo command in the foreground and wait for its result. Do not end your turn while a command runs.
- Before each commit, run `cargo fmt --all` and `cargo clippy --workspace --all-features --all-targets -- -D warnings`. Both must be clean.

## Review Focus

- A test name disappears or changes in a test binary (a test was deleted by mistake with a helper). Pinned by the `--list` comparison in Tasks 1 and 2.
- The shared oracle loses a rule when the copies merge (for example the round rule). Pinned by the diff step in Task 1 and by the existing `#[should_panic]` self-test, which stays in `deferred_resume.rs`.
- An unused import survives and fails only the clippy `-D warnings` gate. Pinned by the clippy step in each task.
- A private method on `RequestLog` gives E0624 in a caller. Pinned by the compile in Task 1.
- The runtime-tokio copy drifts from the core source. No test can pin this (a shared crate is out of scope); Task 3 makes both doc comments point at each other.

---

### Task 1: Move the helpers into `common` and use them in two files

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/common/mod.rs` (imports at lines 15-19; append helpers at the end)
- Modify: `crates/paigasus-helikon-core/tests/deferred_resume.rs` (remove `DeferB` :23-42, `RequestLog` :44-76, oracle :573-619; imports :6-18)
- Modify: `crates/paigasus-helikon-core/tests/handoff_transfer_result.rs` (remove oracle :182-220 approx., `DeferB` :600-619, `RequestLog` :621-653; imports)

**Interfaces:**
- Produces, in `common`:
  - `pub fn assert_tool_calls_paired(messages: &[Item])`
  - `pub struct DeferB;` with `impl PermissionPolicy<()> for DeferB` (defers tool `"b"` with prompt `"pay?"`, allows the rest)
  - `pub struct RequestLog { inner: Arc<MockModel>, requests: Mutex<Vec<ModelRequest>> }` (fields private), `pub fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self>`, `pub fn requests(&self) -> Vec<ModelRequest>`, `impl Model for RequestLog` (records then delegates to `inner`)

- [ ] **Step 1: Record the baseline test lists**

```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-826 || exit 1
S=/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/59427013-806e-43e7-bca8-7ccef030506b/scratchpad
for t in deferred_resume handoff_transfer_result subagent_propagation; do
  cargo test -p paigasus-helikon-core --test $t -- --list 2>/dev/null | grep ': test$' | sort > $S/before_$t.txt
done
wc -l $S/before_*.txt
```

Expected: three non-empty files.

- [ ] **Step 2: Add the helpers to `common/mod.rs`**

Copy `DeferB` and `RequestLog` from `deferred_resume.rs` and make them `pub` as in Interfaces. Copy the oracle from `deferred_resume.rs` (with its inline comments) and make it `pub`. Use this doc comment:

```rust
/// Each `ToolCall` has a `ToolResult` with the same call id later, with no
/// `UserMessage` or `AssistantMessage` between them. Each `ToolResult` closes
/// one earlier open `ToolCall` with its call id. A call id can repeat only
/// after its earlier call has a result. A call after a result starts a new
/// round, so no call may wait for a result then.
///
/// A copy is in `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs`.
/// Keep the two copies the same.
```

Add `PermissionDecision` and `PermissionPolicy` to the `paigasus_helikon_core::{...}` import.

- [ ] **Step 3: Remove the local copies and import from `common`**

In both test files, delete the local `DeferB`, `RequestLog` and `assert_tool_calls_paired`, and extend the `use common::{...}` line with `assert_tool_calls_paired, DeferB, RequestLog`. Keep the `#[should_panic]` test `pairing_oracle_rejects_a_call_after_a_result_while_a_call_waits` in `deferred_resume.rs`. Remove the imports that become unused:
- `deferred_resume.rs`: `Model`, `ModelCapabilities`, `ModelError`.
- `handoff_transfer_result.rs`: `Mutex`, `PermissionDecision`, `PermissionPolicy`.

Clippy is the final check of this list. Remove what it reports as unused, and nothing that it does not.

- [ ] **Step 4: Verify the oracle is unchanged**

```bash
diff <(git show main:crates/paigasus-helikon-core/tests/deferred_resume.rs | awk '/^fn assert_tool_calls_paired/{p=1} p{print} p&&/^}$/{exit}') \
     <(awk '/^pub fn assert_tool_calls_paired/{p=1} p{print} p&&/^}$/{exit}' crates/paigasus-helikon-core/tests/common/mod.rs)
```

Expected: only the first line differs (`fn` vs `pub fn`).

- [ ] **Step 5: Run the tests and compare the lists**

```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-826 || exit 1
S=/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/59427013-806e-43e7-bca8-7ccef030506b/scratchpad
cargo test -p paigasus-helikon-core --test deferred_resume --test handoff_transfer_result
for t in deferred_resume handoff_transfer_result; do
  cargo test -p paigasus-helikon-core --test $t -- --list 2>/dev/null | grep ': test$' | sort | diff $S/before_$t.txt - && echo "$t SAME"
done
```

Expected: all tests pass; both lines print `SAME`.

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-core/tests/common/mod.rs crates/paigasus-helikon-core/tests/deferred_resume.rs crates/paigasus-helikon-core/tests/handoff_transfer_result.rs
git commit -m "test(core): SMA-826 move the pairing oracle, DeferB and RequestLog into tests/common"
```

### Task 2: Use the shared helpers in `subagent_propagation.rs`

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/subagent_propagation.rs` (`DeferB` :337-356, `CapturingModel` :358-378, its use near :416 and :449; imports :6-20)

**Interfaces:**
- Consumes: `common::DeferB`, `common::RequestLog::{with_scripts, requests}` from Task 1.

- [ ] **Step 1: Replace the local types**

Delete the local `DeferB` and `CapturingModel`. Import `DeferB` and `RequestLog` from `common`. Replace

```rust
let inner_model = Arc::new(CapturingModel {
    inner: MockModel::with_scripts(call_then_stop("b")),
    requests: Mutex::new(Vec::new()),
});
```

with `let inner_model = RequestLog::with_scripts(call_then_stop("b"));`, and replace `inner_model.requests.lock().unwrap().clone()` with `inner_model.requests()`. Keep `.shared_model(Arc::clone(&inner_model))`. Remove the imports clippy reports as unused (expected: `BoxStream`, `CancellationToken`, `Model`, `ModelCapabilities`, `ModelError`, `ModelRequest`, `PermissionDecision`, `PermissionPolicy`).

- [ ] **Step 2: Run the tests and compare the list**

```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-826 || exit 1
S=/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/59427013-806e-43e7-bca8-7ccef030506b/scratchpad
cargo test -p paigasus-helikon-core --test subagent_propagation
cargo test -p paigasus-helikon-core --test subagent_propagation -- --list 2>/dev/null | grep ': test$' | sort | diff $S/before_subagent_propagation.txt - && echo SAME
grep -rnE '^(pub )?(struct (DeferB|RequestLog|CapturingModel)\b|fn assert_tool_calls_paired\b)' crates/paigasus-helikon-core/tests
```

Expected: tests pass; `SAME`; the grep gives exactly three lines, all in `common/mod.rs`.

- [ ] **Step 3: fmt, clippy, full core tests, commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-core
git add crates/paigasus-helikon-core/tests/subagent_propagation.rs
git commit -m "test(core): SMA-826 use the shared DeferB and RequestLog in subagent_propagation"
```

Expected: `cargo test -p paigasus-helikon-core` passes.

### Task 3: Point the runtime-tokio oracle copy at the core source

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs:419-426` (doc comment only)

- [ ] **Step 1: Replace the doc comment**

```rust
/// Each `ToolCall` has a `ToolResult` with the same call id later, with no
/// `UserMessage` or `AssistantMessage` between them. Each `ToolResult` closes
/// one earlier open `ToolCall` with its call id. A call id can repeat only
/// after its earlier call has a result. A call after a result starts a new
/// round, so no call may wait for a result then.
///
/// Copy of `assert_tool_calls_paired` in
/// `crates/paigasus-helikon-core/tests/common/mod.rs`. Keep the two copies
/// the same.
```

The function itself does not change.

- [ ] **Step 2: Verify**

```bash
diff <(awk '/^pub fn assert_tool_calls_paired/{p=1} p{print} p&&/^}$/{exit}' crates/paigasus-helikon-core/tests/common/mod.rs | sed 's/^pub fn/fn/') \
     <(awk '/^fn assert_tool_calls_paired/{p=1} p{print} p&&/^}$/{exit}' crates/paigasus-helikon-runtime-tokio/tests/deferred.rs) && echo SAME
cargo test -p paigasus-helikon-runtime-tokio --test deferred
```

Expected: `SAME`; tests pass.

- [ ] **Step 3: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-tokio/tests/deferred.rs
git commit -m "test(runtime-tokio): SMA-826 point the pairing oracle copy at core tests/common"
```
