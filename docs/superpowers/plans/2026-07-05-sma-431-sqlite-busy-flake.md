# SMA-431 SQLite Busy-Flake Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the sessions-sqlite concurrent-writer tests reliably green on degraded Windows CI runners, and document the SQLite contention contract (failure mode + `busy_timeout` sizing rule) in the three places that repeat the recommended pool snippet.

**Architecture:** No backend code changes — `SqliteSession::append`'s `BEGIN IMMEDIATE` serialization is correct. The fix re-sizes the two SQLite test pools (`busy_timeout` 30s→120s, `synchronous=NORMAL`), adds one new deterministic integration test pinning the SQLITE_BUSY error path, and updates docs in `src/lib.rs`, the crate README, and the mdBook sessions page.

**Tech Stack:** Rust workspace, sqlx 0.9 (SQLite), tokio, mdBook. Approved spec: `docs/superpowers/specs/2026-07-05-sma-431-sqlite-busy-flake-design.md`.

## Global Constraints

- Work in the worktree root `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-431-sqlite-flake/` — all paths below are relative to it. Never `cd` out of it; never run `git checkout`/`git switch`; stay on branch `feature/sma-431-flaky-sessions-sqlite-concurrent_writers-test-on-windows-ci`.
- Run `cargo fmt --all` before every commit (the pre-commit hook is a no-op; pre-push runs fmt/clippy/convco).
- Commit messages must satisfy convco: `<type>(<scope>): SMA-431 <lowercase subject>`. Allowed scopes used here: `sessions-sqlite`, `docs`, `plan`.
- Test pools get `busy_timeout = 120s` + `synchronous = NORMAL`. Documentation examples keep `busy_timeout = 30s` (production starting point) + `synchronous = NORMAL`. Do not mix these up.
- No changes to `crates/paigasus-helikon-sessions-testkit/`, no changes to `src/lib.rs` *code* (docs only), no changes to workload constants (16 tasks × 10 events).
- Every code snippet in this plan is the exact intended content — do not improvise alternatives.

---

### Task 1: Deterministic `busy_timeout` regression test

Pins the documented failure mode: an `append` that cannot get SQLite's write lock within `busy_timeout` fails cleanly with `SessionError::Backend` ("database is locked"), nothing is persisted, and the session stays usable afterward. This is a characterization test of EXISTING behavior — it must pass once written; there is no implementation step.

**Files:**
- Create: `crates/paigasus-helikon-sessions-sqlite/tests/busy_timeout.rs`

**Interfaces:**
- Consumes: `SqliteSession::{migrate, open_without_migrate, session_id}` and `paigasus_helikon_core::{Session, SessionError, SessionEvent, ContentPart}` — all existing public API.
- Produces: nothing other tasks depend on (Task 2's stress loop runs this test file among others).

- [ ] **Step 1: Write the test**

Create `crates/paigasus-helikon-sessions-sqlite/tests/busy_timeout.rs` with exactly:

```rust
//! Pins the contention failure mode documented in the crate docs (SMA-431):
//! an `append` that cannot acquire SQLite's database-level write lock within
//! `busy_timeout` fails with `SessionError::Backend` wrapping `SQLITE_BUSY`
//! ("database is locked"), persists nothing, and leaves the session usable.
//!
//! Deterministic by construction — no timing race:
//! 1. migrations run BEFORE any lock is held (so the only write that can
//!    collide with the held lock is the append under test);
//! 2. the blocking `BEGIN IMMEDIATE` transaction is provably held when the
//!    short-timeout append runs;
//! 3. it is released with an explicit awaited `rollback()` (sqlx's `Drop`
//!    enqueues ROLLBACK asynchronously — never rely on it for ordering)
//!    before the retry.

use std::time::Duration;

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, Session, SessionError, SessionEvent};
use paigasus_helikon_sessions_sqlite::SqliteSession;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

fn msg(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        content: vec![ContentPart::Text { text: text.into() }],
        ts: Timestamp::from_second(1_700_000_000).unwrap(),
    }
}

#[tokio::test]
async fn busy_timeout_exhaustion_fails_cleanly_and_session_recovers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("busy.db");

    // Pool A: default busy_timeout; used to migrate first, then to hold the
    // write lock.
    let opts_a = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool_a = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts_a)
        .await
        .expect("pool a");
    SqliteSession::migrate(&pool_a).await.expect("migrate");

    // Pool B: aggressively short busy_timeout, session under test. Built via
    // `open_without_migrate` — `open` would re-touch `_sqlx_migrations` and
    // is not the path under test.
    let opts_b = SqliteConnectOptions::new()
        .filename(&path)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_millis(100));
    let pool_b = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts_b)
        .await
        .expect("pool b");
    let session = SqliteSession::open_without_migrate(pool_b, "busy-session");

    // Hold SQLite's write lock: BEGIN IMMEDIATE takes it up-front and keeps
    // it until commit/rollback.
    let tx_a = pool_a
        .begin_with("BEGIN IMMEDIATE")
        .await
        .expect("begin immediate on pool a");

    // The append must exhaust its 100ms busy_timeout and fail cleanly.
    let err = session
        .append(&[msg("blocked")])
        .await
        .expect_err("append must time out while the write lock is held");
    assert!(
        matches!(&err, SessionError::Backend(_)),
        "expected SessionError::Backend, got: {err:?}"
    );
    assert!(
        err.to_string().contains("database is locked"),
        "expected SQLITE_BUSY (\"database is locked\"), got: {err}"
    );

    // Release the lock deterministically, then the same session must work.
    tx_a.rollback().await.expect("rollback");
    session
        .append(&[msg("after-recovery")])
        .await
        .expect("append succeeds once the lock is released");

    // The failed append persisted nothing; exactly one event survives.
    let events = session.events(None).await.expect("events");
    assert_eq!(events.len(), 1, "failed append must not persist anything");
    match &events[0] {
        SessionEvent::UserMessage { content, .. } => match &content[0] {
            ContentPart::Text { text } => assert_eq!(text, "after-recovery"),
            other => panic!("unexpected content part: {other:?}"),
        },
        other => panic!("unexpected event: {other:?}"),
    }
}
```

- [ ] **Step 2: Run the new test — expect PASS**

```bash
cargo test -p paigasus-helikon-sessions-sqlite --test busy_timeout
```

Expected: `test busy_timeout_exhaustion_fails_cleanly_and_session_recovers ... ok` — `1 passed; 0 failed`.

If it fails, STOP and report the failure verbatim — do not adjust assertions to make it pass. (A failure here means the spec's model of the error path is wrong, which invalidates later doc wording.)

- [ ] **Step 3: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-sessions-sqlite/tests/busy_timeout.rs
git commit -m "test(sessions-sqlite): SMA-431 pin busy-timeout failure mode with deterministic regression test

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Re-size the two SQLite test pools

The flake fix itself: `busy_timeout` 30s→120s (guaranteed mitigation for the proven busy-handler exhaustion) and `synchronous=NORMAL` (removes per-commit fsync, the hypothesized degraded-runner cost driver) on both concurrent-writer pools. Also replaces the header comment whose "30 seconds absorbs slow CI runners" claim CI run 27703633580 disproved.

**Files:**
- Modify: `crates/paigasus-helikon-sessions-sqlite/tests/concurrent_writers.rs:1-37`
- Modify: `crates/paigasus-helikon-sessions-sqlite/tests/conformance.rs:1-25`

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: the stress-loop verification baseline used in Task 5.

- [ ] **Step 1: Update `concurrent_writers.rs`**

Replace the module doc comment (current lines 1-10, from `//! Covers acceptance` through `//! seconds.`) with:

```rust
//! Covers acceptance criterion #2 from SMA-318 (concurrency): N tasks
//! appending to the same `session_id` produce a contiguous sequence with
//! no gaps or duplicates.
//!
//! **Why not loom:** loom models pure-Rust concurrency primitives and
//! can't reason about SQLite's lock state machine. Using a real
//! `tokio::test` with a file-backed pool exercises the actual write-lock
//! path.
//!
//! **Pool sizing (SMA-431):** `busy_timeout` caps each writer's wait for
//! SQLite's single write lock, and the worst-placed writer waits out the
//! whole backlog — all 160 transactions here. A healthy Windows runner
//! finishes that backlog in ~1.3s, but a degraded one (CI run 27703633580)
//! was still incomplete at 33.6s and blew through the previous 30s timeout.
//! 120s plus `synchronous=NORMAL` (no per-commit fsync) gives a ≥3.5×
//! guaranteed floor over the worst observed backlog, ~40× if fsync
//! dominates. See docs/superpowers/specs/2026-07-05-sma-431-sqlite-busy-flake-design.md.
```

Replace the import line

```rust
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
```

with

```rust
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
```

Replace the options block

```rust
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(30));
```

with

```rust
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(120));
```

Everything else in the file (constants, test body, assertions) stays byte-identical.

- [ ] **Step 2: Update `conformance.rs`**

Same two mechanical changes. Replace

```rust
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
```

with

```rust
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
```

and replace

```rust
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(30));
```

with

```rust
    // Pool sizing rationale: see concurrent_writers.rs — the testkit's
    // run_concurrent_writers (via run_all) replays the same 16×10 workload
    // here (SMA-431).
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(120));
```

- [ ] **Step 3: Run the crate's tests — expect all green**

```bash
cargo test -p paigasus-helikon-sessions-sqlite
```

Expected: all test binaries pass (`concurrent_writers`, `conformance`, `busy_timeout`, `multi_session`, `persistence`, `roundtrip`, doc-tests) — `0 failed` everywhere.

- [ ] **Step 4: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-sessions-sqlite/tests/concurrent_writers.rs crates/paigasus-helikon-sessions-sqlite/tests/conformance.rs
git commit -m "test(sessions-sqlite): SMA-431 harden concurrent-writer pools against slow runners

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Rewrite the `src/lib.rs` contention documentation

Doc-comment-only change (no code). Replaces the "Recommended pool configuration" section: adds `synchronous=NORMAL` to the example (with the required import — the example is `no_run` but still compiled), documents the failure mode and the sizing rule, and removes the stale sentence claiming 30s "is the value exercised by this crate's concurrent_writers test".

**Files:**
- Modify: `crates/paigasus-helikon-sessions-sqlite/src/lib.rs:9-40` (module docs only)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: the canonical doc wording that Task 4 mirrors (README + book).

- [ ] **Step 1: Replace the module-doc section**

In `crates/paigasus-helikon-sessions-sqlite/src/lib.rs`, replace lines 9-28 (from `//! ## Recommended pool configuration` through `//! upward if you expect heavy multi-writer contention.`) with:

```rust
//! ## Recommended pool configuration
//!
//! ```no_run
//! use sqlx::sqlite::{
//!     SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
//! };
//! use std::time::Duration;
//!
//! # async fn build() -> Result<sqlx::SqlitePool, sqlx::Error> {
//! let opts = SqliteConnectOptions::new()
//!     .filename("sessions.db")
//!     .create_if_missing(true)
//!     .journal_mode(SqliteJournalMode::Wal)
//!     .synchronous(SqliteSynchronous::Normal)
//!     .busy_timeout(Duration::from_secs(30));
//! SqlitePoolOptions::new().connect_with(opts).await
//! # }
//! ```
//!
//! `synchronous = NORMAL` is the recommended pairing with WAL for
//! multi-writer workloads: commits stop fsyncing individually (the WAL is
//! synced at checkpoints instead), which multiplies write throughput under
//! contention. The trade-off is durability, not integrity — after a power
//! loss the most recent commits may be missing, but the database cannot
//! corrupt. Keep sqlx's default `FULL` if losing any acknowledged append is
//! unacceptable.
//!
//! ## Concurrent appends under contention
//!
//! Writers serialize on SQLite's single database-level write lock.
//! `busy_timeout` caps how long one `append` waits for that lock, and under
//! sustained contention the worst-placed writer waits out the *entire
//! backlog* ahead of it — so size the timeout against
//! `(concurrent writers) × (appends per writer) × (worst-case transaction
//! latency)`, not against a single transaction. An `append` that exhausts
//! the timeout fails with [`SessionError::Backend`] wrapping `SQLITE_BUSY`
//! ("database is locked"). The failure is clean: nothing from the failed
//! call is persisted, no stored data is lost or corrupted, and the session
//! remains usable (pinned by this crate's `busy_timeout` integration test).
//! 30 seconds is a sane starting point; tune upward for heavy multi-writer
//! contention.
```

- [ ] **Step 2: Add the `SessionError::Backend` link target**

At the bottom of the module docs (current lines 38-40), the reference links read:

```rust
//! [`Session`]: paigasus_helikon_core::Session
//! [`project`]: paigasus_helikon_core::project
//! [`Compacted`]: paigasus_helikon_core::SessionEvent::Compacted
```

Add one line so the block becomes:

```rust
//! [`Session`]: paigasus_helikon_core::Session
//! [`SessionError::Backend`]: paigasus_helikon_core::SessionError::Backend
//! [`project`]: paigasus_helikon_core::project
//! [`Compacted`]: paigasus_helikon_core::SessionEvent::Compacted
```

- [ ] **Step 3: Verify the doctest compiles and docs build warning-free**

```bash
cargo test -p paigasus-helikon-sessions-sqlite --doc
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-sessions-sqlite --no-deps
```

Expected: doc-test `src/lib.rs - (line …) - compile ... ok`; `cargo doc` finishes with zero warnings (a broken intra-doc link or missing import fails here).

- [ ] **Step 4: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-sessions-sqlite/src/lib.rs
git commit -m "docs(sessions-sqlite): SMA-431 document contention failure mode and busy_timeout sizing

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Mirror the guidance in the crate README and the mdBook sessions page

Keeps the three snippet sites in sync per the repo's README/book currency rules. Neither file's code fences are compiled — mirror content, no imports needed beyond what's shown.

**Files:**
- Modify: `crates/paigasus-helikon-sessions-sqlite/README.md:17-31`
- Modify: `docs/book/src/concepts/sessions.md:99-122`

**Interfaces:**
- Consumes: canonical wording from Task 3 (already restated verbatim below — no need to read Task 3's diff).
- Produces: nothing further.

- [ ] **Step 1: Update the crate README**

In `crates/paigasus-helikon-sessions-sqlite/README.md`, replace the example block (lines 15-29) with:

````markdown
```rust
use paigasus_helikon_sessions_sqlite::SqliteSession;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use std::time::Duration;

let opts = SqliteConnectOptions::new()
    .filename("sessions.db")
    .create_if_missing(true)
    .journal_mode(SqliteJournalMode::Wal)
    .synchronous(SqliteSynchronous::Normal)
    .busy_timeout(Duration::from_secs(30));
let pool = SqlitePoolOptions::new().connect_with(opts).await?;

// Opens (or implicitly creates) the session and runs migrations.
let session = SqliteSession::open(pool, "user-123").await?;
```
````

and replace the paragraph at line 31 (`Wrap the session in `Arc` …`) with:

```markdown
Wrap the session in `Arc` and pass it into `RunContext::new(...)` (whose session parameter is `Arc<dyn Session>`) in place of `Arc::new(MemorySession::new())` to persist transcripts across runs. WAL journal mode plus `synchronous = NORMAL` are recommended for concurrent writers. `busy_timeout` caps how long one append waits for SQLite's single write lock — under sustained contention the worst-placed writer waits out the entire backlog ahead of it, so size the timeout against total backlog duration, not a single transaction; an append that exhausts it fails cleanly with `SessionError::Backend` ("database is locked"), persisting nothing. See the crate docs for the full sizing rule and the `open_without_migrate` fast path.
```

- [ ] **Step 2: Update the mdBook sessions page**

In `docs/book/src/concepts/sessions.md`, replace the snippet's import + options lines (99-106):

```rust
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteJournalMode};
use std::time::Duration;

let opts = SqliteConnectOptions::new()
    .filename("sessions.db")
    .create_if_missing(true)
    .journal_mode(SqliteJournalMode::Wal)
    .busy_timeout(Duration::from_secs(30));
```

with

```rust
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use std::time::Duration;

let opts = SqliteConnectOptions::new()
    .filename("sessions.db")
    .create_if_missing(true)
    .journal_mode(SqliteJournalMode::Wal)
    .synchronous(SqliteSynchronous::Normal)
    .busy_timeout(Duration::from_secs(30));
```

and replace the closing paragraph (lines 120-122):

```markdown
Appends serialize through SQLite's database-level write lock (`BEGIN IMMEDIATE`
plus a `(session_id, sequence)` primary key), so the backend is safe for
concurrent writers.
```

with

```markdown
Appends serialize through SQLite's database-level write lock (`BEGIN IMMEDIATE`
plus a `(session_id, sequence)` primary key), so the backend is safe for
concurrent writers. `synchronous = NORMAL` is the recommended pairing with WAL
for multi-writer workloads (durability-for-throughput trade: a power loss may
drop the newest commits, never corrupt). `busy_timeout` caps how long one
append waits for the write lock — under sustained contention the worst-placed
writer waits out the entire backlog ahead of it, so size the timeout against
`writers × appends × worst-case transaction latency`, not a single
transaction. An append that exhausts it fails cleanly with
`SessionError::Backend` ("database is locked"), persisting nothing; the
session stays usable.
```

- [ ] **Step 3: Verify the book builds clean**

```bash
mdbook build docs/book
```

Expected: exits 0 with no warnings (linkcheck `warning-policy = "error"` turns any broken link into a failure).

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-sessions-sqlite/README.md docs/book/src/concepts/sessions.md
git commit -m "docs(sessions-sqlite): SMA-431 mirror contention guidance in README and book

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: Full CI-gate verification + stress loop

Reproduces every CI gate locally (CLAUDE.md list) plus the spec's §5.1 stress loop. Fix-forward anything red (with `cargo fmt --all` + amend or follow-up commit as appropriate); report results verbatim.

**Files:**
- No new files; fixes only if a gate fails.

**Interfaces:**
- Consumes: all previous tasks committed.
- Produces: the verification evidence for the PR description.

- [ ] **Step 1: Formatting, lints, workspace tests**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
```

Expected: all three exit 0. (Run the EXACT workspace-level test gate — per-crate green does not imply workspace green.)

- [ ] **Step 2: Docs gates**

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
mdbook build docs/book
```

Expected: all exit 0; doc coverage ≥ 80%.

- [ ] **Step 3: Stress loop (spec §5.1)**

```bash
for i in $(seq 1 50); do
  cargo test -p paigasus-helikon-sessions-sqlite -q >/dev/null 2>&1 || { echo "FAIL at iteration $i"; break; }
done; echo "stress loop done"
```

Expected: prints only `stress loop done` (no `FAIL at iteration N`). If any iteration fails, STOP and report — do not retry-until-green.

- [ ] **Step 4: Confirm working tree is clean and branch is 7 commits ahead of main**

```bash
git status --short
git log --oneline main..HEAD
```

Expected: empty status; commits = 2 spec commits + 1 plan commit + Task 1-4 commits.

---

## Self-Review (completed)

1. **Spec coverage:** §4.1 → Task 2; §4.2 → Tasks 3 + 4; §4.3 → Task 1; §5.1-5.3 → Task 5 (§5.4 CI-matrix evidence happens on the PR, Stage 5/6). ✓
2. **Placeholder scan:** none — every step carries exact content/commands. ✓
3. **Type consistency:** test uses only existing public API (`SessionError::Backend` verified at `crates/paigasus-helikon-core/src/session.rs:561-576`; `Pool::begin_with` is the same API `src/lib.rs:118` already uses; `SqliteSynchronous` verified present in sqlx-sqlite 0.9.0). ✓
