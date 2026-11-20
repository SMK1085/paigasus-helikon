# SMA-816: `ToolDeferred` conformance test — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the public sub-test `run_tool_deferred` to `paigasus-helikon-sessions-testkit`, call it last from `run_all`, and prove with negative tests that it fails for a backend that corrupts a deferral.

**Architecture:** One new async function in `crates/paigasus-helikon-sessions-testkit/src/lib.rs` appends a fixed 8-event log with three deferrals in two batches. Then it checks the round trip, the projection, `open_deferrals` and two watermarks. Two new test files in the testkit's own `tests/` hold the `CompactingSession` run (D4) and six broken wrapper sessions (D6). The four backends get the new test through their existing `run_all` call. Their files do not change.

**Tech Stack:** Rust 1.94 workspace, tokio, serde_json, jiff, async-trait, futures-core, mdBook.

**Spec:** `docs/superpowers/specs/2026-11-20-sma-816-tooldeferred-conformance-design.md` (approved at Gate 1, 2026-11-20). The spec is the binding authority. Read it before each task.

## Global Constraints

- Work only in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-816-tooldeferred-conformance`, branch `feature/sma-816-add-a-sessioneventtooldeferred-conformance`. Use absolute paths under this root for every Read, Write and Edit.
- Do not run a git command that moves HEAD or switches branches: no `git checkout`, `git switch`, `git reset`, `git rebase`, `git stash`, `git restore --source`. Do not push.
- Stage with explicit paths only. Never `git add -A` or `git add .` (`.env` and `.claude` are not ignored).
- Never use `--no-verify`. Commits are signed through 1Password. If a commit fails with "failed to fill whole buffer", stop and report. Do not bypass signing.
- Run every cargo command in the foreground and wait for it to finish. Do not background a cargo run. Do not end your turn while a command runs.
- Run `cargo fmt --all` and `cargo clippy --workspace --all-features --all-targets -- -D warnings` before each commit that touches Rust.
- Do not change `paigasus-helikon-core`, the facade, or any backend crate (`sessions-sqlite`, `sessions-postgres`, `sessions-redis`), including their `tests/`.
- Do not change any version number. Do not edit any `CHANGELOG.md`. release-plz does both (D5).
- The core requirement stays `paigasus-helikon-core = { workspace = true }` (workspace pin `0.6.0`).
- New normal dependency of the testkit: `serde_json = { workspace = true }`. New dev-dependencies: `async-trait = { workspace = true }`, `futures-core = { workspace = true }`. No other dependency changes.
- Commit message format: `<type>(<scope>): SMA-816 <lowercase message>`, then a blank line, then `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Scopes used: `sessions`. Types used: `feat`, `test`, `docs`.
- Every `pub` item has a `///` doc (`missing_docs` + `-D warnings`). Test files start with `#![allow(missing_docs)]`, as `tests/memory.rs` does.

## Review Focus

- **A real backend round-trips `args` E exactly**: 2^53+1, `-42`, `1250.75`, the 4-byte character and the tab/quote/backslash string through Postgres `JSONB`, Redis and SQLite. Pinned by Task 4, Step 2 (Docker run, no `SKIP` line).
- **The panicking fake model is reachable**, so the compacting test proves something. Pinned by Task 2 test `compacting_control_compacts_without_deferral` (`#[should_panic]`).
- **The test wrappers are neutral apart from their one corruption**, so a negative test cannot pass by accident. Pinned by Task 2 test `passthrough_wrapper_passes` (the shared wrapper with no corruption passes `run_tool_deferred`).
- **A per-batch-sequence backend fails at the watermark inside the batch, not at the batch boundary.** Pinned by Task 2 test `same_sequence_per_batch_fails`, whose `expected` string names `SequenceId(5)`, not `SequenceId(4)`.
- **The published tarball builds against registry core 0.6.0 with `serde_json` as a normal dependency.** Pinned by Task 4, Step 3.

## Fixed values (used by Tasks 1 and 2)

The implementer of Task 1 writes these values exactly. Task 2 relies on them.

**Timestamps:** event `i` (0-based) has `Timestamp::from_second(1_700_000_000 + i).unwrap()`.

**Deferral ids (lowercase UUID v4):**

- `d1` = `"6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b"`
- `d2` = `"0b8e7d6c-5a4f-4e3d-9c2b-1a0f9e8d7c6b"`
- `d3` = `"d3a1b2c4-e5f6-4a7b-8c9d-0e1f2a3b4c5d"`

**`args` E (effective arguments of `d1`):**

```rust
serde_json::json!({
    "memo": "Miete für März 💶",
    "note": "ref \"A-7\"\\rent\tmonthly",
    "amount": 1250.75,
    "ref_id": 9_007_199_254_740_993_u64,
    "adjust": -42,
    "recurring": true,
    "end_date": null,
    "tags": ["rent", "monthly"],
    "payee": { "name": "Hausverwaltung Nord", "iban_suffix": "0042" }
})
```

**`args` M (model arguments of `c1`):** E with `"amount": 1300.5` and without the `"memo"` key.

**The log** (batch 1 = events 0-4, batch 2 = events 5-7):

| # | Event | Fields |
|---|-------|--------|
| 0 | `UserMessage` | content `[Text "pay my rent"]` |
| 1 | `ToolCalled` | `call_id "c1"`, `name "transfer_funds"`, `args` M |
| 2 | `ToolDeferred` | `d1`, `call_id "c1"`, `name "transfer_funds"`, `args` E, `prompt "Approve the transfer of 1250.75 EUR?\nPayee: Hausverwaltung Nord "` (two lines, one trailing space), `handoff_path ["triage", "payments"]` |
| 3 | `ToolCalled` | `call_id "c2"`, `name "close_account"`, `args {"account": "acct-9"}` |
| 4 | `ToolDeferred` | `d2`, `call_id "c2"`, `name "close_account"`, `args {"account": "acct-9"}`, `prompt "Close the account acct-9?"`, `handoff_path []` |
| 5 | `ToolCalled` | `call_id "c3"`, `name "lookup_balance"`, `args {}` |
| 6 | `ToolDeferred` | `d3`, `call_id "c3"`, `name "lookup_balance"`, `args {}`, `prompt "Show the balance?"`, `handoff_path ["triage"]` |
| 7 | `ToolReturned` | `call_id "c3"`, content `[Text "balance: 40.00"]` |

Build all events as variant literals (not the `SessionEvent::*` constructors), so each `ts` is fixed.

**Assertion messages.** All messages start with `run_tool_deferred: `. A `#[should_panic(expected = ...)]` string matches a substring of the panic text, so `assert_eq!(a, b, "<msg>")` works.

| Check (spec D2) | Exact message |
|---|---|
| `append` batch 1 `.expect` | `run_tool_deferred: append batch 1 (events 0-4)` |
| `append` batch 2 `.expect` | `run_tool_deferred: append batch 2 (events 5-7)` |
| `events(None)` `.expect` | `run_tool_deferred: events(None)` |
| `snapshot()` `.expect` | `run_tool_deferred: snapshot()` |
| `events(Some(SequenceId(n)))` `.expect`, n = 4, 5 | `run_tool_deferred: events(Some(SequenceId(n)))` |
| 1.1 count is 8 | `run_tool_deferred: events(None): event count` |
| 1.1 kinds in D1 order | `run_tool_deferred: events(None): event kinds` |
| 1.2 one field of one deferral | `run_tool_deferred: ToolDeferred {label}: {field}` |
| 1.2 the event is not a `ToolDeferred` | `run_tool_deferred: ToolDeferred {label}: not a ToolDeferred event` |
| 1.3 JSON backstop | `run_tool_deferred: events(None): JSON of all events` |
| 2.1 item count is 5 | `run_tool_deferred: snapshot().messages: item count` |
| 2.2 JSON equals `project(&appended)` | `run_tool_deferred: snapshot().messages: JSON` |
| 3.1 open deferrals equal `[d1, d2]` | `run_tool_deferred: open_deferrals: [d1, d2] in log order` |
| 3.2 `d1.agent() == Some("payments")` | `run_tool_deferred: open_deferrals: d1.agent()` |
| 3.2 `d2.agent() == None` | `run_tool_deferred: open_deferrals: d2.agent()` |
| 3.3 no open deferral has the `d3` id | `run_tool_deferred: open_deferrals: d3 is closed` |
| 4 kinds after watermark n | `run_tool_deferred: events(Some(SequenceId(n))): event kinds` |

`{label}` is `d1`, `d2` or `d3` for the `events(None)` read, and `d3 after SequenceId(4)` or `d3 after SequenceId(5)` for the watermark reads. `{field}` is one of `deferral_id`, `call_id`, `name`, `args`, `prompt`, `handoff_path`, `ts`, checked in that order.

---

### Task 1: The `run_tool_deferred` sub-test

**Files:**

- Modify: `crates/paigasus-helikon-sessions-testkit/Cargo.toml` (`[dependencies]`: add `serde_json = { workspace = true }`)
- Modify: `Cargo.lock` (cargo adds `serde_json` to the testkit entry; commit what cargo writes)
- Modify: `crates/paigasus-helikon-sessions-testkit/src/lib.rs`
- Modify: `crates/paigasus-helikon-sessions-testkit/tests/memory.rs`

**Interfaces:**

- Consumes: core `0.6.0`: `SessionEvent` (`Serialize`, `#[non_exhaustive]` enum, `kind() -> &'static str`), `project(&[SessionEvent]) -> ConversationSnapshot`, `ConversationSnapshot.messages: Vec<Item>` (`Item: Serialize`), `open_deferrals(&[SessionEvent]) -> Vec<PendingToolCall>`, `PendingToolCall::new(deferral_id, call_id, tool, args: serde_json::Value, prompt, handoff_path: Vec<String>)` (`PartialEq`, `#[non_exhaustive]`), `PendingToolCall::agent() -> Option<&str>`, `PendingToolCall::deferral_id() -> &str`, `SequenceId(pub u64)` (0-based, exclusive). All are re-exported at the core crate root.
- Produces: `pub async fn run_tool_deferred<F, Fut>(make: F) where F: Fn() -> Fut, Fut: Future<Output = Arc<dyn Session>>`. It calls `make()` once. It needs no `tokio::spawn`. `run_all` calls it last, as `run_tool_deferred(&make).await`.

- [ ] **Step 1: Write the failing test**

Add to `tests/memory.rs`:

```rust
#[tokio::test]
async fn memory_session_passes_tool_deferred() {
    run_tool_deferred(|| async { Arc::new(MemorySession::new()) as Arc<dyn Session> }).await;
}
```

Import `run_tool_deferred` next to `run_all`.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-sessions-testkit --test memory`
Expected: compile error, `run_tool_deferred` not found in `paigasus_helikon_sessions_testkit`.

- [ ] **Step 3: Implement `run_tool_deferred` in `src/lib.rs`**

Add `serde_json` to `Cargo.toml`. Implement the D2 checks in the order 1, 2, 3, 4, with the fixed values and the exact messages above. Private helpers (names are free, these are suggestions): `fn fixed_ts(i: i64) -> Timestamp`, `fn effective_args() -> serde_json::Value`, `fn model_args() -> serde_json::Value`, `fn deferral_log() -> Vec<SessionEvent>`, `fn assert_deferral(got: &SessionEvent, want: &SessionEvent, label: &str)`.

Rules the code must follow:

- `append` gets `&log[0..5]`, then `&log[5..8]`.
- 1.2 checks the events at positions 2, 4 and 6 of `events(None)` against `log[2]`, `log[4]`, `log[6]`. Use `let SessionEvent::ToolDeferred { deferral_id, call_id, name, args, prompt, handoff_path, ts } = got else { panic!(...) }` with the "not a ToolDeferred event" message. Compare `handoff_path` as `Vec<String>` (order matters).
- 1.3 compares `serde_json::to_value(&got).unwrap()` with `serde_json::to_value(&log).unwrap()`.
- 2.2 compares `serde_json::to_value(&snap.messages)` with `serde_json::to_value(&project(&log).messages)`. Use `log` (the testkit input), not the backend output.
- 3.1 compares `open_deferrals(&got)` with `vec![PendingToolCall::new(d1 ...), PendingToolCall::new(d2 ...)]`, built from the fixed values.
- 4 expects kinds `["tool_called", "tool_deferred", "tool_returned"]` after `SequenceId(4)`, with `d3` at index 1, and kinds `["tool_deferred", "tool_returned"]` after `SequenceId(5)`, with `d3` at index 0. Check the `d3` fields with `assert_deferral` and the labels from the message table.
- Any `match` on `SessionEvent` or `Item` has a wildcard arm (`#[non_exhaustive]`).

The `///` doc of `run_tool_deferred` says, in STE: the test appends a log with three deferred tool calls in two batches; it checks that every field comes back unchanged, that `snapshot()` equals the projection, that `open_deferrals` finds `d1` and `d2`, and that a watermark inside a batch reads correctly. It also says that it calls the factory once.

Add `run_tool_deferred(&make).await;` as the last line of `run_all`.

Update the crate rustdoc at the top of `src/lib.rs` (spec "Docs"):

- "runs four tests" becomes "runs five tests", and "`run_all` runs all four tests" becomes "`run_all` runs all five tests".
- Add a list line: `` - `run_tool_deferred`: a log with deferred tool calls comes back exactly, `snapshot()` adds no item for a deferral, and `open_deferrals` finds the open deferrals. ``
- In "# Stability", change the example pin from `=0.1.0` to `=0.1.1`. Add this sentence after the pin sentence: "A patch release can also move the core requirement: 0.1.0 uses core 0.5, and 0.1.1 uses core 0.6. So an exact pin must match your core version."

- [ ] **Step 4: Run the testkit tests to verify they pass**

Run: `cargo test -p paigasus-helikon-sessions-testkit`
Expected: `memory_session_passes_conformance` and `memory_session_passes_tool_deferred` pass, and the crate doctest passes (it calls `run_all`).

Run: `cargo test -p paigasus-helikon-sessions-sqlite --test conformance`
Expected: `sqlite_passes_conformance` passes.

- [ ] **Step 5: Format, lint, docs**

Run: `cargo fmt --all && cargo clippy --workspace --all-features --all-targets -- -D warnings && RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-sessions-testkit --no-deps`
Expected: no diff from fmt, no warnings, no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-sessions-testkit/Cargo.toml Cargo.lock \
  crates/paigasus-helikon-sessions-testkit/src/lib.rs \
  crates/paigasus-helikon-sessions-testkit/tests/memory.rs
git commit -m "feat(sessions): SMA-816 add the run_tool_deferred conformance sub-test" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

Then `git show --stat HEAD` must list exactly these four files.

---

### Task 2: Compacting wrapper run and negative tests

**Files:**

- Modify: `crates/paigasus-helikon-sessions-testkit/Cargo.toml` (add `[dev-dependencies]` `async-trait = { workspace = true }`, `futures-core = { workspace = true }`)
- Modify: `Cargo.lock` (if cargo changes it)
- Create: `crates/paigasus-helikon-sessions-testkit/tests/compacting.rs`
- Create: `crates/paigasus-helikon-sessions-testkit/tests/broken_backends.rs`

**Interfaces:**

- Consumes: `run_tool_deferred` from Task 1, and its messages and fixed values (see "Fixed values" above). Core: `CompactingSession::builder(inner: S, model: Arc<dyn Model>) -> CompactingSessionBuilder<S>`, `.threshold(1)`, `.build() -> Result<CompactingSession<S>, CompactingSessionError>`; `CompactingSession<S: Session>: Session`. `Model` trait (`#[async_trait]`): `async fn invoke(&self, request: ModelRequest, cancel: CancellationToken) -> Result<futures_core::stream::BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError>` and `fn capabilities(&self) -> ModelCapabilities`. Core does not re-export `BoxStream`, so use `futures_core::stream::BoxStream`.
- Produces: tests only. Nothing public.

`tests/compacting.rs`:

- `struct PanicModel;` with `#[async_trait] impl Model`. `invoke` calls `panic!("PanicModel::invoke: CompactingSession started a compaction")`. `capabilities` returns `ModelCapabilities::default()`.
- `fn compacting() -> Arc<dyn Session>`: `CompactingSession::builder(MemorySession::new(), Arc::new(PanicModel)).threshold(1).build().expect("threshold 1 is valid")`, as `Arc<dyn Session>`.
- `#[tokio::test] async fn compacting_session_passes_tool_deferred()`: `run_tool_deferred(|| async { compacting() }).await`. Do not use `run_all` (spec D4).
- `#[tokio::test] #[should_panic(expected = "PanicModel::invoke")] async fn compacting_control_compacts_without_deferral()`: append one `SessionEvent::user_message(vec![ContentPart::Text { text: "pay my rent".into() }])` to `compacting()`. This proves that threshold 1 reaches the model when no deferral is open.

`tests/broken_backends.rs`:

- One wrapper `struct Broken { inner: MemorySession, fault: Fault, batch_starts: std::sync::Mutex<Vec<u64>> }` with `#[derive(Clone, Copy)] enum Fault { None, ReverseHandoffPath, ArgsThroughF64, CalledArgsIntoDeferral, RenameSnapshotToolCall, SwapEvents2And3, SameSequencePerBatch }`, and `#[async_trait] impl Session for Broken`. A per-fault struct is also acceptable, if each test still corrupts exactly one thing.
- `fn broken(fault: Fault) -> Arc<dyn Session>`.
- `append` delegates to `inner`. It also pushes the start index of the batch (the event count before the append) to `batch_starts`.
- `events(since)` reads `inner.events(None)` and applies the fault to the full log. For `SameSequencePerBatch`, the sequence of each event is the start index of its batch, and the read returns the events whose sequence is greater than `since`. For every other fault, it then applies `since` the normal way (skip the first `since + 1` events).
- `snapshot()` returns `inner.snapshot()`. For `RenameSnapshotToolCall`, it changes the `name` of the first `Item::ToolCall` in `messages` from `transfer_funds` to `transfer_fund` (match `snap.messages.iter_mut()` with `Item::ToolCall { name, .. }` and a wildcard arm; `Item` is a `#[non_exhaustive]` enum and `ConversationSnapshot` a `#[non_exhaustive]` struct, but its `pub` field `messages` is mutable).

The faults, on read:

| Fault | What it does |
|---|---|
| `ReverseHandoffPath` | reverses `handoff_path` of every `ToolDeferred` |
| `ArgsThroughF64` | replaces every JSON number in the `args` of every `ToolCalled` and `ToolDeferred` by `Value::from(n.as_f64().unwrap())`, recursively (2^53+1 becomes `9007199254740992.0`) |
| `CalledArgsIntoDeferral` | sets the `args` of each `ToolDeferred` to the `args` of the `ToolCalled` with the same `call_id` |
| `RenameSnapshotToolCall` | changes `snapshot()` only (above) |
| `SwapEvents2And3` | swaps positions 2 and 3 of the full log |
| `SameSequencePerBatch` | gives all events of one `append` the same sequence (above) |

Tests (each is `#[tokio::test]`, and each calls `run_tool_deferred(|| async { broken(Fault::X) })`):

| Test | `#[should_panic(expected = ...)]` |
|---|---|
| `passthrough_wrapper_passes` | none (it must pass) — `Fault::None` |
| `reversed_handoff_path_fails` | `"run_tool_deferred: ToolDeferred d1: handoff_path"` |
| `args_through_f64_fails` | `"run_tool_deferred: ToolDeferred d1: args"` |
| `called_args_in_deferral_fails` | `"run_tool_deferred: ToolDeferred d1: args"` |
| `renamed_snapshot_tool_call_fails` | `"run_tool_deferred: snapshot().messages: JSON"` |
| `swapped_events_fail` | `"run_tool_deferred: events(None): event kinds"` |
| `same_sequence_per_batch_fails` | `"run_tool_deferred: events(Some(SequenceId(5))): event kinds"` |

- [ ] **Step 1: Add the dev-dependencies and write both test files**

- [ ] **Step 2: Run the new tests**

Run: `cargo test -p paigasus-helikon-sessions-testkit --test compacting --test broken_backends`
Expected: 9 tests pass (2 in `compacting`, 7 in `broken_backends`). If a `should_panic` test fails with "panic did not contain expected string", read the panic text: the wrapper or the message in `src/lib.rs` is wrong. Fix the wrapper. Change `src/lib.rs` only if its message differs from the table in "Fixed values".

- [ ] **Step 3: Format and lint**

Run: `cargo fmt --all && cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: no diff, no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-sessions-testkit/Cargo.toml \
  crates/paigasus-helikon-sessions-testkit/tests/compacting.rs \
  crates/paigasus-helikon-sessions-testkit/tests/broken_backends.rs
git add Cargo.lock   # only if `git status` shows it modified
git commit -m "test(sessions): SMA-816 prove run_tool_deferred catches broken backends" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: README and mdBook

**Files:**

- Modify: `crates/paigasus-helikon-sessions-testkit/README.md`
- Modify: `docs/book/src/concepts/sessions.md:353-374` (section "Backend conformance")

**Interfaces:**

- Consumes: the name `run_tool_deferred` and the stability text from Task 1, Step 3.
- Produces: docs only.

- [ ] **Step 1: Edit the README**

- Intro: "The suite runs four tests against a `Session` implementation: append and read, exclusive watermark, projection, and concurrent writers." becomes five tests, with "deferred tool calls" added last.
- "Run a subset": add `- run_tool_deferred` (in backticks) as the last list item.
- "Stability": the same change as the rustdoc in Task 1, Step 3 (`=0.1.1`, and the core-requirement sentence, word for word).

- [ ] **Step 2: Edit the book page**

- Add a table row after `run_concurrent_writers`: `` | `run_tool_deferred` | A log with deferred tool calls: every field comes back unchanged, `snapshot()` adds no item for a deferral, `open_deferrals` finds the open deferrals, and a watermark inside an `append` batch reads correctly | ``
- After the sentence that ends "...before it ships.", add: "The testkit also runs `run_tool_deferred` against `CompactingSession<MemorySession>` with `threshold(1)`. This proves that the wrapper stores deferrals exactly and does not compact over an open deferral."

- [ ] **Step 3: Verify**

Run: `npx markdownlint-cli2 && mdbook build docs/book`
Expected: markdownlint reports 0 errors. mdbook builds with no linkcheck error.

Run: `grep -n "four" crates/paigasus-helikon-sessions-testkit/README.md crates/paigasus-helikon-sessions-testkit/src/lib.rs`
Expected: no line that counts the sub-tests.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-sessions-testkit/README.md docs/book/src/concepts/sessions.md
git commit -m "docs(sessions): SMA-816 document the run_tool_deferred sub-test" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Verification

**Files:** none changed. If a step finds a defect, fix it in the file of the task that owns it and commit with that task's commit type.

**Interfaces:** consumes Tasks 1 to 3.

- [ ] **Step 1: Start Postgres and Redis**

Run `docker info --format '{{.ServerVersion}}'`. Expected: a version number. If Docker is not running, stop and report.

```bash
docker run -d --name sma816-pg -e POSTGRES_PASSWORD=postgres -p 55432:5432 postgres:17@sha256:5c855ad7b85e68e48a62f34662853f38b57c1c1d80f3a927ab58034fd6d31c5e
docker run -d --name sma816-rds -p 56379:6379 redis:7@sha256:b2b95679e3b46fb51864949ed25ea976fc3a6bcc00a40a1bc00d568cb2822e50
for i in $(seq 1 30); do docker exec sma816-pg pg_isready -U postgres && break; sleep 2; done
for i in $(seq 1 30); do docker exec sma816-rds redis-cli ping && break; sleep 2; done
```

These are the image digests of `ci.yml` `sessions-it`. The host ports 55432 and 56379 avoid a clash with a local service.

- [ ] **Step 2: Run the backend suites with no skip**

```bash
HELIKON_TEST_POSTGRES_URL=postgres://postgres:postgres@localhost:55432/postgres \
HELIKON_TEST_REDIS_URL=redis://localhost:56379 \
cargo test -p paigasus-helikon-sessions-postgres -p paigasus-helikon-sessions-redis \
  -p paigasus-helikon-sessions-sqlite -p paigasus-helikon-sessions-testkit \
  -- --nocapture 2>&1 | tee target/sma816-sessions.log
grep -c SKIP target/sma816-sessions.log
```

Expected: every `test result:` line says `0 failed`. `postgres_passes_conformance`, `redis_passes_conformance` and `sqlite_passes_conformance` say `ok`. The `grep -c SKIP` prints `0`. `--nocapture` is necessary: without it, the `SKIP` lines are hidden. Then remove the containers: `docker rm -f sma816-pg sma816-rds`.

- [ ] **Step 3: Package and publish dry run**

Run: `cargo package -p paigasus-helikon-sessions-testkit && cargo publish -p paigasus-helikon-sessions-testkit --dry-run`
Expected: both succeed. The verify build compiles against registry `paigasus-helikon-core v0.6.0` (the output shows `Downloaded` or `Compiling paigasus-helikon-core v0.6.0` from the registry, not a path). The publish ends with "aborting upload due to dry run". The working tree must be clean first; do not pass `--allow-dirty`.

- [ ] **Step 4: Mutation check (spec Verification 5)**

For each row, edit `src/lib.rs` as stated, run the named tests, record the result, and then undo the edit with the Edit tool. After the last row, `git diff --quiet crates/paigasus-helikon-sessions-testkit/src/lib.rs` must exit 0. Do not use `git checkout` or `git restore` for the undo.

| Mutation in `run_tool_deferred` | Run | Expected |
|---|---|---|
| Delete the 1.2 per-field checks | `--test broken_backends` | `reversed_handoff_path_fails`, `args_through_f64_fails`, `called_args_in_deferral_fails` FAIL (the panic now has the 1.3 message) |
| Delete the 1.1 kinds check | `--test broken_backends` | `swapped_events_fail` FAILS |
| Delete the 2.2 JSON check | `--test broken_backends` | `renamed_snapshot_tool_call_fails` FAILS |
| Delete both group 4 kinds checks | `--test broken_backends` | `same_sequence_per_batch_fails` FAILS |
| Group 3: change the expected list to `[d1]` only | `--test memory` | `memory_session_passes_tool_deferred` FAILS |
| 1.3: compare against `serde_json::to_value(&log[..7])` | `--test memory` | `memory_session_passes_tool_deferred` FAILS |

Group 3 and check 1.3 have no D6 negative test that fails when they are deleted. Each of the six D6 corruptions makes an earlier check fail first. So for these two checks, the mutation is a wrong expected value that the positive test must reject.

- [ ] **Step 5: The CI gate list**

Run each command and record the result:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
npx markdownlint-cli2
bash scripts/check-markdownlint-config.sh
bash scripts/check-cargo-profile-env-sync.sh
bash scripts/check-cargo-profile-env-sync-selftest.sh
bash scripts/check-release-gaps-selftest.sh
mdbook build docs/book
```

Expected: all pass, with one known local exception. Local `rustc --version` was `1.99.0` on 2026-11-20. If `cargo test --workspace --all-features` fails only in the trybuild UI tests of `paigasus-helikon-core` and `paigasus-helikon-macros`, record the test names and the local rustc version. That failure comes from a local rustc that differs from CI stable. It is not caused by this branch: confirm that the same tests fail on `main` with the same rustc (run them in a separate scratch worktree of `main`, never by a checkout in this worktree). Any other failure is a defect.

- [ ] **Step 6: Report**

Report: the per-step results, the `SKIP` count, the mutation table results, and the trybuild status with the rustc version. After the PR opens, the controller confirms that the `sessions-it` job ran the suites and did not print "No session-related changes" (spec Verification 4).
