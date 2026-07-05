# SMA-431 — sessions-sqlite `concurrent_writers` Windows flake: root cause and fix design

**Status:** draft for review
**Ticket:** [SMA-431](https://linear.app/smaschek/issue/SMA-431/flaky-sessions-sqlite-concurrent-writers-test-on-windows-ci-sqlite)
**Crate:** `paigasus-helikon-sessions-sqlite` (plus its testkit-driven conformance run)
**Branch:** `feature/sma-431-flaky-sessions-sqlite-concurrent_writers-test-on-windows-ci`

## 1. Problem

`concurrent_appends_produce_contiguous_sequence`
(`crates/paigasus-helikon-sessions-sqlite/tests/concurrent_writers.rs`) fails
intermittently on Windows CI with:

```
append: Backend(Database(SqliteError { code: 5, message: "database is locked" }))
```

Observed once on `main` (run `27703633580`, job `test (windows-latest, 1.85)`,
commit `583c76f`, 2026-06-17) on a PR that never touched sessions-sqlite. Only
signal-only matrix rows are affected; required checks stay green. The cost is a
red `main` and eroded trust in the matrix signal.

## 2. Root cause (evidence-backed)

### 2.1 What the ticket assumed vs. what the code already does

The ticket's two favored fix directions — `busy_timeout` and WAL — have been in
place **since the crate was born** (SMA-318, PR #35, commit `e0ba6b2`):

- `SqliteSession::append` opens every write transaction with
  `pool.begin_with("BEGIN IMMEDIATE")`, so writers take SQLite's RESERVED lock
  up-front and serialize correctly (no read→write upgrade, so the
  busy handler **is** honored — the `SQLITE_BUSY_SNAPSHOT` immediate-failure
  path does not apply).
- The test pool sets `journal_mode=WAL` and `busy_timeout=30s`.

So the flake happened **despite** the ticket's candidate mitigations, and the
root cause is elsewhere.

### 2.2 The failing run's timeline is a smoking gun

From the `test (windows-latest, 1.85)` log of run `27703633580`:

| Time (UTC) | Event |
|---|---|
| 16:28:32.9 | `concurrent_writers.rs` test binary starts |
| 16:29:06.5 | **Three** writer tasks panic with `SQLITE_BUSY` within 1 ms of each other |

That is ~33.6 s in — the workload was still incomplete past the 30-second
`busy_timeout`, and the failures cluster exactly where busy-handler exhaustion
predicts. For contrast, the most recent green `main` Windows run
(`28513492935`, 2026-07-01) finishes the same test in **1.34 s**.

### 2.3 Mechanism

The test runs 16 tasks × 10 appends = 160 write transactions, all serialized
through SQLite's single-writer lock. sqlx 0.9 leaves `PRAGMA synchronous` at
its default **FULL**, so every WAL commit performs a full fsync. On a healthy
runner a transaction takes ~8 ms (160 × 8 ms ≈ 1.3 s, matching the green run).
On a degraded Windows runner (Defender real-time scanning of the
freshly-compiled test binary and DB file, slow disk, 2-core VM) per-commit
fsync latency inflates ~25×, pushing the total serialized backlog past 30 s.

`busy_timeout` bounds **each writer's total wait for the write lock**. Under
sustained contention, the last writers in line must wait for the entire
remaining backlog. SQLite's busy handler polls with no fairness/FIFO
guarantee, so several tail writers can all exhaust their 30 s budget and
surface `SQLITE_BUSY` (code 5) — which `append` maps to
`SessionError::Backend` and the test unwraps into a panic.

### 2.4 Classification (acceptance criterion #1)

**Test robustness gap, not a backend concurrency bug.** The backend's
serialization is correct: `BEGIN IMMEDIATE` prevents sequence races, the
`(session_id, sequence)` primary key backstops uniqueness, and no data is lost
or duplicated — writers time out cleanly rather than corrupting anything. The
gap is that the test's worst-case workload duration can exceed its own
lock-wait ceiling on slow runners.

There is a **secondary, product-level documentation gap**: any real deployment
with sustained multi-writer contention faces the same failure mode (append
fails with `SessionError::Backend` wrapping `SQLITE_BUSY` once a writer waits
longer than `busy_timeout`), and neither the crate docs, the README, nor the
mdBook currently state the sizing rule or the failure mode.

### 2.5 Exposure is wider than the named test

The shared testkit (`paigasus-helikon-sessions-testkit::run_concurrent_writers`,
invoked via `run_all`) runs an **identical 16×10 workload** against the SQLite
conformance pool in `tests/conformance.rs`, which uses the same
`busy_timeout=30s` and also leaves `synchronous=FULL`. The same flake can
therefore hit `sqlite_passes_conformance`. Both SQLite test pools must be
fixed together.

## 3. Approaches considered

### A. Re-size the test configuration + document contention behavior (chosen)

Attack both factors of the inequality `backlog duration > busy_timeout`:

1. **Cut per-commit cost ~10×+**: set `synchronous=NORMAL` on both SQLite test
   pools. WAL+NORMAL is SQLite's canonical throughput configuration; the
   durability trade-off (a power loss may drop the most recent commits, never
   corrupt) is irrelevant for tests.
2. **Raise the ceiling 4×**: `busy_timeout` 30 s → 120 s in both test pools. A
   busy timeout is a *cap on waiting*, not a sleep — raising it costs nothing
   when the runner is healthy (test stays ~1.3 s) and buys ~100× combined
   headroom against degraded runners.
3. **Document the behavior** (crate docs + README + mdBook) and pin the error
   path with a deterministic regression test (§4.3).

Pros: fixes the actual root cause; zero product-behavior change; makes the
documented contract honest. Cons: none identified — the contention pattern the
test exists to exercise (16 concurrent writers racing `BEGIN IMMEDIATE`) is
preserved unchanged.

### B. Backend retry-on-`SQLITE_BUSY` in `append` (rejected)

`busy_timeout` **is** SQLite's retry mechanism, tunable by the pool owner. An
app-level retry loop on top duplicates it with worse observability (latency
hidden inside the backend, unbounded tail), and it patches a correctness
property that is not broken.

### C. In-process single-writer serialization (rejected)

Serializing writes through one connection (or an async mutex) would require
shared per-pool state across cloned `SqliteSession` instances — an
architectural change to a shipped, correct backend — and does nothing for
cross-process writers, which SQLite's lock already handles. It also
contradicts the crate's documented design ("appends serialize through SQLite's
database-level write lock").

### D. Test-side retry on BUSY (rejected)

Strictly worse than raising `busy_timeout`: same effect, more code, and it
would mask a genuine regression if the backend ever stopped honoring the busy
handler (e.g. an accidental return to a deferred transaction).

## 4. Design of the chosen fix

### 4.1 Test pool changes (the flake fix)

`crates/paigasus-helikon-sessions-sqlite/tests/concurrent_writers.rs` and
`crates/paigasus-helikon-sessions-sqlite/tests/conformance.rs`:

```rust
let opts = SqliteConnectOptions::new()
    .filename(&path)
    .create_if_missing(true)
    .journal_mode(SqliteJournalMode::Wal)
    .synchronous(SqliteSynchronous::Normal)   // NEW: fsync per checkpoint, not per commit
    .busy_timeout(Duration::from_secs(120));  // WAS: 30s
```

Update the `concurrent_writers.rs` header comment: it currently claims the
30-second timeout "absorbs slow CI runners", which run `27703633580`
empirically disproved. Replace with the sizing rule and a pointer to SMA-431
(green baseline 1.34 s vs. degraded >33 s; tail writers must be able to wait
out the whole backlog).

Workload constants (16 tasks × 10 events) stay unchanged in both the test and
the testkit — the contention pattern is the point of the test. No testkit code
changes: pool configuration is the backend-specific caller's job, which is
exactly the boundary the testkit's `make` closure already draws.

### 4.2 Documentation changes (acceptance criterion #3)

Three places currently repeat the recommended-pool snippet and must stay in
sync (per the repo's README/book currency rules):

1. **`src/lib.rs` module docs** — extend the "Recommended pool configuration"
   example with `.synchronous(SqliteSynchronous::Normal)` and rewrite the
   `busy_timeout` paragraph to state:
   - the failure mode: under sustained contention, an `append` whose wait for
     the write lock exceeds `busy_timeout` fails with `SessionError::Backend`
     wrapping `SQLITE_BUSY` ("database is locked"); no data is lost or
     corrupted by such a failure;
   - the sizing rule: a writer's worst-case wait approximates the total
     duration of the write backlog ahead of it — size `busy_timeout` above
     `(concurrent writers) × (worst-case per-transaction latency) × (appends
     per writer)`, not above a single transaction's latency;
   - the `synchronous=NORMAL` trade-off: recommended with WAL for
     multi-writer workloads; durability on power loss drops to
     "recent commits may be lost, corruption impossible" (keep FULL where
     that is unacceptable);
   - fix the stale sentence claiming 30 s "is the value exercised by this
     crate's concurrent_writers test" (the test will use 120 s; the example's
     30 s remains a sane production starting point).
2. **`crates/paigasus-helikon-sessions-sqlite/README.md`** — mirror the
   updated snippet and add one sentence on the sizing rule / failure mode
   (this file is the crates.io landing page).
3. **`docs/book/src/concepts/sessions.md`** — mirror the updated snippet and
   the contention paragraph. `mdbook build docs/book` must stay clean
   (linkcheck `warning-policy = "error"`).

No Rust API or behavior changes anywhere in `src/`. The lib.rs doc example
stays `no_run` (compile-checked only).

### 4.3 Deterministic regression test for the documented failure mode

New integration test (proposed: `tests/busy_timeout.rs`) that pins the
documented contract without any timing race:

1. Open pool A (WAL, file-backed) and hold a raw `BEGIN IMMEDIATE`
   transaction open on it (acquire a connection, begin, don't commit).
2. Open a `SqliteSession` on pool B against the same file with
   `busy_timeout` ≈ 100 ms.
3. `append` one event → must return `Err(SessionError::Backend(..))` whose
   display/source chain contains "database is locked" (SQLITE_BUSY, code 5).
4. Drop/rollback A's transaction, retry the append → succeeds; `events(None)`
   returns exactly the one event.

Step 4 doubles as documentation that BUSY timeouts are clean failures: the
session remains fully usable and nothing was persisted by the failed attempt.
This is deterministic (the lock is genuinely held, not racing) and fast
(~100 ms). It also guards the `BEGIN IMMEDIATE` choice itself: if `append`
ever regressed to a deferred `BEGIN`, step 3 would still pass but existing
concurrency tests plus this test's comment anchor the intent.

### 4.4 What is deliberately out of scope

- Backend code changes (`src/lib.rs` logic) — nothing is broken.
- Testkit changes — workload stays as specified by SMA-318/SMA-330.
- Postgres/Redis session backends (`sessions-it`) — no SQLITE_BUSY analogue;
  their conformance runs are unaffected by this failure mode.
- CI matrix/required-checks changes — the flake is fixed at the source, not
  papered over in workflow config.

## 5. Verification plan (acceptance criterion #2)

1. **Local stress loop** (macOS, worktree): run the two affected tests ~50×
   back-to-back (`cargo test -p paigasus-helikon-sessions-sqlite --test
   concurrent_writers --test conformance` in a shell loop); expect 0 failures.
   macOS cannot reproduce Windows fsync degradation, so this validates
   "no new flakiness", not the fix itself.
2. **Mechanism proof**: the new `busy_timeout.rs` test deterministically
   produces and then clears the exact `SQLITE_BUSY` error observed on CI —
   demonstrating the diagnosed mechanism is real and the error path is clean.
3. **Exact CI gates locally**: `cargo fmt --all -- --check`; `cargo clippy
   --workspace --all-features --all-targets -- -D warnings`; `cargo test
   --workspace --all-features`; `RUSTDOCFLAGS="-D warnings" cargo doc
   --workspace --all-features --no-deps`; doc-coverage script; `mdbook build
   docs/book`.
4. **CI matrix evidence**: on the PR, all six `test` matrix rows green; re-run
   the Windows jobs at least twice (`gh run rerun --job`) to accumulate
   repeated-run evidence per the AC. (A degraded runner cannot be summoned on
   demand; the margin math in §4.1 — ~100× vs. the observed ~25× degradation —
   is the analytical backstop.)

## 6. Release & workflow notes

- **PR title (squash commit):** `fix(sessions-sqlite): SMA-431 harden
  concurrent-writer tests against slow runners and document contention
  behavior`. Rationale for `fix` over `test`: the deliverable includes a
  user-facing documentation correction (stale/incomplete contention contract)
  published to crates.io/docs.rs, and `fix` lets release-plz cut the patch
  release that actually ships it. `sessions-sqlite` is an already-released
  crate → normal release-plz flow, no manual version ritual; the facade
  cascade is automatic because release-plz performs the bump itself.
- Branch commits: `test(sessions-sqlite): …` for test changes,
  `docs(sessions-sqlite): …` / `docs(docs): …` for crate-doc/book changes —
  all types/scopes verified against `.versionrc`.
- After merge: watch the auto-opened `chore: release` PR's CI per repo
  convention.
- Post-merge housekeeping (not part of the PR): the auto-memory note that
  "a red main ci from this alone is a flake" should be updated once the fix
  has soaked.

## 7. Resolved questions (answered from ticket + code, for Gate 1 visibility)

| Question | Answer | Basis |
|---|---|---|
| Test-only or backend bug? | Test robustness gap; backend correct | §2 evidence |
| Why did busy_timeout/WAL not prevent it? | They were already active; the timeout was under-sized for degraded-runner backlog | §2.2 timing |
| Backend retry? | No — duplicates `busy_timeout` | §3.B |
| Scope of the fix? | Both SQLite test pools + docs ×3 + one new deterministic test | §2.5, §4 |
| Reproduce on Windows first? | Not feasible locally; CI log timing + deterministic mechanism test + margin math instead | §5 |

**Judgment calls explicitly flagged for Sven at Gate 1:**
(a) recommending `synchronous=NORMAL` in the public docs (vs. documenting it
as an option only); (b) `fix(...)` PR title so the doc correction publishes,
vs. `test(...)` which would defer publication to the next unrelated release.
