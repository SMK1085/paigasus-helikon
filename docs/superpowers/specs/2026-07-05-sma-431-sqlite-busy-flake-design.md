# SMA-431 — sessions-sqlite `concurrent_writers` Windows flake: root cause and fix design

**Status:** draft for review (revised after adversarial challenge)
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

### 1.1 Acceptance criteria (verbatim from SMA-431)

1. Root cause identified (test-only vs. backend).
2. `concurrent_appends_produce_contiguous_sequence` passes reliably on
   Windows — green across repeated CI runs / a local stress loop, with no
   `database is locked` errors.
3. If a backend fix lands: documented behavior for concurrent appends under
   contention.

(No backend *code* fix lands — §2.4 classifies this as a test gap — but the
spirit of AC #3 is honored by documenting the contention contract, which is
today undocumented; see §4.2.)

## 2. Root cause (what is proven, what is hypothesized)

### 2.1 What the ticket assumed vs. what the code already does

The ticket's two favored fix directions — `busy_timeout` and WAL — have been in
place **since the crate was born** (SMA-318, PR #35, commit `e0ba6b2`):

- `SqliteSession::append` opens every write transaction with
  `pool.begin_with("BEGIN IMMEDIATE")`, so writers take SQLite's RESERVED lock
  up-front and serialize correctly (no read→write upgrade, so the
  busy handler **is** honored — the `SQLITE_BUSY_SNAPSHOT` immediate-failure
  path does not apply).
- The **test pools** set `journal_mode=WAL` and `busy_timeout=30s`.

Note the boundary: WAL and `busy_timeout` are *caller-supplied pool settings*,
not backend defaults. The backend imposes nothing; a user building a pool from
raw sqlx defaults gets `busy_timeout=5s` and `synchronous=FULL` and is
therefore **more** exposed to this failure mode than the test is. That is
precisely why the documentation half of this fix matters (§4.2).

So the flake happened **despite** the ticket's candidate mitigations, and the
root cause is elsewhere.

### 2.2 The failing run's timeline

From the `test (windows-latest, 1.85)` log of run `27703633580`:

| Time (UTC) | Event |
|---|---|
| 16:28:32.9 | `concurrent_writers.rs` test binary starts |
| 16:29:06.5 | **Three** writer tasks panic with `SQLITE_BUSY` within 1 ms of each other |

That is ~33.6 s in — the workload was still incomplete past the 30-second
`busy_timeout`, and the failures cluster exactly where busy-handler exhaustion
predicts (several tail writers exhausting their budget almost simultaneously).
For contrast, the most recent green `main` Windows run (`28513492935`,
2026-07-01) finishes the same test in **1.34 s**.

### 2.3 Mechanism — proven vs. hypothesized

**Proven (from the timeline + code):** busy-handler exhaustion. The test runs
16 tasks × 10 appends = 160 write transactions, all serialized through
SQLite's single-writer lock. `busy_timeout` bounds **each writer's total wait
for the write lock**; under sustained contention the last writers in line must
wait out the entire remaining backlog. SQLite's busy handler polls with no
fairness/FIFO guarantee, so several tail writers can exhaust their 30 s budget
together and surface `SQLITE_BUSY` (code 5) — which `append` maps to
`SessionError::Backend` and the test unwraps into a panic. On the failing
runner the whole workload was ≥ 25× slower than the green baseline
(incomplete at 33.6 s vs. 1.34 s green), so 30 s was simply under-sized for
that runner's backlog.

**Hypothesized (plausible, not isolated):** *why* the runner was ~25× slower.
sqlx 0.9 leaves `PRAGMA synchronous` at its default **FULL**, so every WAL
commit performs a full fsync; on a degraded Windows runner (Defender real-time
scanning of the freshly-compiled binary and DB/`-wal`/`-shm` files, slow disk,
2-core VM) fsync latency is the most likely dominant cost — but Defender
file-lock interference or scheduler starvation could contribute, and no I/O
profile of the degraded run exists to separate them. The fix treats the
`busy_timeout` raise as the **guaranteed** mitigation (it addresses the proven
mechanism regardless of cost driver) and `synchronous=NORMAL` as a
**hypothesis-driven optimization** that shrinks the backlog if — as is likely —
fsync dominates.

### 2.4 Classification (AC #1)

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
mdBook currently state the sizing rule or the failure mode — while the
recommended-configuration snippet they all repeat implies 30 s is enough.

### 2.5 Exposure is wider than the named test

The shared testkit (`paigasus-helikon-sessions-testkit::run_concurrent_writers`,
invoked via `run_all`) runs an **identical 16×10 workload** against the SQLite
conformance pool in `tests/conformance.rs`, which uses the same
`busy_timeout=30s` and also leaves `synchronous=FULL`. The same flake can
therefore hit `sqlite_passes_conformance`. Both SQLite test pools must be
fixed together. (The other test files — `roundtrip.rs`, `multi_session.rs`,
`persistence.rs` — are single-connection or in-memory with no concurrent
writers, and are out of scope.)

## 3. Approaches considered

### A. Re-size the test configuration + document contention behavior (chosen)

Attack both factors of the inequality `backlog duration > busy_timeout`:

1. **Raise the ceiling 4× (guaranteed fix for the proven mechanism)**:
   `busy_timeout` 30 s → 120 s in both test pools. A busy timeout is a *cap on
   waiting*, not a sleep — raising it costs nothing when the runner is healthy
   (test stays ~1.3 s).
2. **Cut per-commit cost (hypothesis-driven optimization)**: set
   `synchronous=NORMAL` on both SQLite test pools. WAL+NORMAL is SQLite's
   canonical throughput configuration; the durability trade-off (a power loss
   may drop the most recent commits, never corrupt) is irrelevant for tests.
   If fsync dominates the degraded-runner cost (§2.3), this shrinks the
   backlog several-fold on exactly the runners that matter.
3. **Document the behavior** (crate docs + README + mdBook) and pin the error
   path with a deterministic regression test (§4.3).

Honest framing of what this achieves: it drives recurrence probability to
near-zero under any degradation comparable to what has been observed, but it
cannot *prove* elimination — the failing run never completed, so the true
worst-case backlog on a pathological runner is unmeasured (§4.1 derives the
margin). Residual risk: a runner degraded ≫ 25× could still exceed 120 s; if
that ever happens the loud failure is itself useful signal that CI runner
health, not this crate, is the problem.

Pros: addresses the proven mechanism directly; zero product-behavior change;
makes the documented contract honest; keeps the stress test's assertion
strict (any BUSY = failure = early warning that headroom is eroding). Cons:
retains a wall-clock dependency, mitigated by margin (§4.1).

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

### D. Test-side BUSY tolerance (retry loop in the stress test) (rejected, revised rationale)

The challenge review correctly noted that the original rejection ("would mask
a regression in busy-handler honoring") is nullified by §4.3, which pins that
contract deterministically. D is genuinely the only option that fully
decouples the stress test from runner speed. It is still rejected, on two
surviving grounds:

1. **It weakens the stress test's assertion.** Today the test asserts "16
   concurrent writers all succeed within a generous wait budget" — a
   throughput-and-liveness property. With retry-on-BUSY it asserts only "data
   eventually survives", and a future change that tanked concurrent append
   throughput (e.g. accidental lock-holding across an await) would pass
   silently instead of flaking loudly.
2. **The decoupling is illusory.** Any *bounded* retry (by attempts ×
   timeout) is still a wall-clock budget, just an obscured one; an unbounded
   retry converts pathological runners into multi-minute hangs bounded only
   by the CI job timeout. There is no magic-number-free variant — so prefer
   the explicit, derivable magic number of approach A.

Workload reduction (shrinking 16×10) was also considered and rejected: the
constants live in the shared testkit and in this test as the SMA-318
concurrency acceptance workload; shrinking them reduces what the test proves
for every backend (Postgres/Redis conformance included) to save budget only
SQLite needs.

## 4. Design of the chosen fix

### 4.1 Test pool changes (the flake fix)

`crates/paigasus-helikon-sessions-sqlite/tests/concurrent_writers.rs` and
`crates/paigasus-helikon-sessions-sqlite/tests/conformance.rs`:

```rust
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};

let opts = SqliteConnectOptions::new()
    .filename(&path)
    .create_if_missing(true)
    .journal_mode(SqliteJournalMode::Wal)
    .synchronous(SqliteSynchronous::Normal)   // NEW: fsync per checkpoint, not per commit
    .busy_timeout(Duration::from_secs(120));  // WAS: 30s
```

**Why 120 s and not 300 s (or 30 s):** the failing run's backlog was
incomplete at 33.6 s, so the observed-worst-case need is "somewhat above
34 s". 120 s covers ~3.5× the observed-incomplete backlog *even if
`synchronous=NORMAL` buys nothing*, and roughly an order of magnitude more if
the fsync hypothesis holds (§2.3). Going to 300 s would buy more tail but
directly worsens time-to-signal on a *real* regression: on a genuine
never-released lock, every waiting writer fails only after the full timeout,
so the test's worst-case wall time on a true deadlock is ≈ `busy_timeout`
plus queue drain. 120 s keeps that bounded at ~2 minutes. (A
`tokio::time::timeout` wrapper around the test body was considered for
fail-fast on hangs and rejected as redundant — `busy_timeout` itself already
bounds every writer's wait, and the failure is loud.)

**Margin arithmetic, reconciled:** timeout raise = 4× (guaranteed); NORMAL ≈
up to ~10× *if* fsync dominates (hypothesis); combined ≈ up to ~40× against a
~25×-degraded runner, and a guaranteed-floor of ~3.5× over the observed
backlog with zero benefit from NORMAL.

Update the `concurrent_writers.rs` header comment: it currently claims the
30-second timeout "absorbs slow CI runners", which run `27703633580`
empirically disproved. Replace with the sizing rule and a pointer to SMA-431
(green baseline 1.34 s vs. degraded >33 s; tail writers must be able to wait
out the whole backlog).

Workload constants (16 tasks × 10 events) stay unchanged in both the test and
the testkit (see §3.D for why shrinking them was rejected). No testkit code
changes: pool configuration is the backend-specific caller's job, which is
exactly the boundary the testkit's `make` closure already draws.

### 4.2 Documentation changes (AC #3)

Three places currently repeat the recommended-pool snippet and must stay in
sync (per the repo's README/book currency rules). The challenge review
verified this enumeration is exact — no fourth site exists:

1. **`src/lib.rs` module docs** — extend the "Recommended pool configuration"
   example with `.synchronous(SqliteSynchronous::Normal)` **and add
   `SqliteSynchronous` to the example's `use` line** (the example is `no_run`
   but still compiled — a missing import fails the `docs`/`test` gates).
   Rewrite the `busy_timeout` paragraph to state:
   - the failure mode: under sustained contention, an `append` whose wait for
     the write lock exceeds `busy_timeout` fails with `SessionError::Backend`
     wrapping `SQLITE_BUSY` ("database is locked"); no data is lost or
     corrupted by such a failure, and the session remains usable;
   - the sizing rule: a writer's worst-case wait approximates the total
     duration of the write backlog ahead of it — size `busy_timeout` above
     `(concurrent writers) × (worst-case per-transaction latency) × (appends
     per writer)`, not above a single transaction's latency;
   - the `synchronous=NORMAL` trade-off: recommended *for multi-writer
     workloads* with WAL; durability on power loss drops to "recent commits
     may be lost, corruption impossible" — keep the default FULL where that
     is unacceptable (final recommendation posture is Gate-1 judgment call
     (a), §7);
   - fix the stale sentence claiming 30 s "is the value exercised by this
     crate's concurrent_writers test" (the test will use 120 s; the example's
     30 s remains a sane production starting point).
2. **`crates/paigasus-helikon-sessions-sqlite/README.md`** — mirror the
   updated snippet and add one sentence on the sizing rule / failure mode
   (this file is the crates.io landing page; its fences are not compiled).
3. **`docs/book/src/concepts/sessions.md`** — mirror the updated snippet and
   the contention paragraph. `mdbook build docs/book` must stay clean
   (linkcheck `warning-policy = "error"`).

No Rust API or behavior changes anywhere in `src/`.

### 4.3 Deterministic regression test for the documented failure mode

New integration test (proposed: `tests/busy_timeout.rs`) that pins the
documented contract without any timing race. Ordering revised per the
challenge review so the **only** write that can collide with the held lock is
the append under test:

1. **Migrate first, before any lock is held**: build a file-backed pool,
   run `SqliteSession::migrate`, so the schema exists.
2. Open pool A on the same file and hold a raw `BEGIN IMMEDIATE` transaction
   open (acquire a connection, begin, do not commit).
3. Build a `SqliteSession` via **`open_without_migrate`** (never `open`, which
   would re-touch `_sqlx_migrations` and is not the path under test) on pool
   B against the same file with `busy_timeout` ≈ 100 ms.
4. `append` one event → must return `Err(SessionError::Backend(..))` whose
   display/source chain contains "database is locked" (SQLITE_BUSY, code 5).
5. **Explicitly `rollback().await` A's transaction** — never rely on `Drop`,
   whose ROLLBACK is enqueued asynchronously in sqlx and may not have
   released the lock by the time the retry runs.
6. Retry the append → succeeds; `events(None)` returns exactly the one event.

Step 6 doubles as documentation that BUSY timeouts are clean failures: the
session remains fully usable and nothing was persisted by the failed attempt.
The test is deterministic on every platform including Windows: the lock is
genuinely held (not racing) when step 4 runs, and it is provably released
(awaited rollback) before step 6, so the 100 ms budget only ever has to
expire against a lock that cannot go away early — no false-pass window — and
never gates a lock that is still being released.

### 4.4 What is deliberately out of scope

- Backend code changes (`src/lib.rs` logic) — nothing is broken.
- Testkit changes — workload stays as specified by SMA-318/SMA-330.
- Postgres/Redis session backends (`sessions-it`) — no SQLITE_BUSY analogue;
  their conformance runs are unaffected by this failure mode.
- CI matrix/required-checks changes — the flake is fixed at the source, not
  papered over in workflow config.

## 5. Verification plan (AC #2)

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
   demand; the margin derivation in §4.1 is the analytical backstop, with its
   guaranteed-floor and hypothesis components separated.)

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
| Proven root cause vs. hypothesis? | Busy-handler exhaustion proven; fsync-dominance is the labeled hypothesis behind NORMAL | §2.3 |
| Backend retry? | No — duplicates `busy_timeout` | §3.B |
| Test-side retry? | No — weakens the stress assertion; bounded retry is wall-clock coupling in disguise | §3.D |
| Why 120 s? | ≥3.5× guaranteed floor over observed backlog, ~40× with NORMAL; keeps deadlock signal ≤ ~2 min | §4.1 |
| Scope of the fix? | Both SQLite test pools + docs ×3 + one new deterministic test | §2.5, §4 |
| Reproduce on Windows first? | Not feasible locally; CI log timing + deterministic mechanism test + margin math instead | §5 |

**Judgment calls explicitly flagged for Sven at Gate 1:**
(a) how strongly to recommend `synchronous=NORMAL` in the public docs — as
the recommended multi-writer setting (spec's current position) vs. a
documented option with FULL kept as the headline recommendation;
(b) `fix(...)` PR title so the doc correction publishes, vs. `test(...)`
which would defer publication to the next unrelated release.
