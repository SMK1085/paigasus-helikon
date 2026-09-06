# SMA-618 — Actions cache budget: stop the thrash

Design document. Status: approved for planning.
Linear: [SMA-618](https://linear.app/smaschek/issue/SMA-618/actions-cache-thrashes-at-37percent-over-the-10-gb-limit-pr-scoped)

## Problem

GitHub's Actions cache limit is 10 GB per repository and is not raisable. This
repository runs chronically over it, so GitHub evicts LRU continuously and some
CI leg starts cold on essentially every run.

Measured on 2026-09-06 via `gh api repos/SMK1085/paigasus-helikon/actions/caches`:

| Size | Ref | Key |
| -- | -- | -- |
| 3.10 GB | `refs/pull/241/merge` | `v0-rust-test-Windows_NT-x64-2113753f-6e49faf4` |
| 3.10 GB | `refs/heads/main` | `v0-rust-test-Windows_NT-x64-2113753f-6e49faf4` |
| 2.76 GB | `refs/pull/240/merge` | `v0-rust-test-Windows_NT-x64-2113753f-6f72c38b` |
| 2.61 GB | `refs/pull/241/merge` | `v0-rust-test-Linux-x64-6ff13d87-c41dae9a` |
| 1.07 GB | `refs/pull/241/merge` | `v0-rust-test-Windows_NT-x64-4c70ae13-6e49faf4` |
| 1.07 GB | `refs/heads/main` | `v0-rust-test-Windows_NT-x64-4c70ae13-6e49faf4` |

**13.69 GB against a 10 GB limit — 37% over.**

There are two independent causes. The Linear issue names only the first.

### Cause 1 — PR jobs write caches

Every `Swatinem/rust-cache` site in the repository uses the default
`save-if: true`, so each PR job saves its own entry scoped to
`refs/pull/N/merge`. Those entries are read a handful of times, then die with
the PR — but while they live they evict `main`'s, which every job restores from.

In the inventory above, PRs #240 and #241 hold **9.54 GB of the 13.69 GB**:
the entire overage.

### Cause 2 — `main`'s own footprint exceeds the limit by itself

This is not in the Linear issue and it is what makes the issue's proposed fix
insufficient on its own.

A single push to `main` writes **~16 cache entries**:

| Workflow | Cached jobs | Count |
| -- | -- | -- |
| `ci.yml` | `clippy`, `test` (6 matrix legs), `build-no-default-features`, `docs`, `doc-coverage`, `sessions-it` | 11 |
| `msrv.yml` | `verify` | 1 |
| `audit.yml` | `audit` | 1 |
| `deny.yml` | `deny` | 1 |
| `integration.yml` | `temporal-it`, `agentcore-image` | 2 |

The seven entries whose sizes have actually been observed already sum to
**10.65 GB** (3.10 + 2.61 + 2.20 + 1.07 + 0.92 + 0.55 + 0.20). The other nine
are unmeasured but non-zero. A realistic total is 20–25 GB.

The consequence is directly observable. On run `33992753929` — a push to `main`
at `4eb98545` — **9 of the 11 cached `ci.yml` jobs logged `No cache found.`**,
including `test (ubuntu-latest, stable)`, which then took 15 minutes. The two
that did restore (`test (macos-latest, stable)`, `test (windows-latest, stable)`)
both restored `full match: false`, i.e. a prefix-only restore via the fallback
restore key.

`main` is running nearly as cold as the PRs are.

### What this means for the issue's plan

The issue's revised acceptance criterion is:

> after `save-if` lands and one `main` run completes, total usage sits
> comfortably under 10 GB and `main` holds an entry for every cached job

The first half is achievable by `save-if` alone. The second half is not:
16 entries at observed sizes cannot fit in 10 GB no matter who writes them.
This design therefore pairs `save-if` with size reduction, and revises the
criterion (see [Acceptance](#acceptance)).

## Constraints

Carried from the Linear issue and `CLAUDE.md`; all are preserved by this design.

- **Do not add `target/tests/trybuild` to `cache-directories`.** It is the
  intuitive fix and it is backwards — that directory is the ~2.03 GB making the
  Windows-stable entry the largest in the repo.
- **`--skip trybuild_ui` on the `1.94` legs must stay.** It exists for an
  unrelated reason: `.stderr` snapshots pin rustc diagnostic text byte-for-byte
  and that text drifts across releases.
- **No job may be renamed.** The required-status-check contexts in
  `.github/rulesets/main-protection-checks.json` are bare job names; a rename
  silently un-gates `main`.
- **`test (windows-latest, stable)` stays required.** It is the only gate
  exercising the Windows timeout path in `paigasus-helikon-tools` (`cmd /C`
  spawning, `TerminateProcess` kill). Nothing here touches that.
- **Actions stay SHA-pinned** with the human-readable version in a comment.
  `Swatinem/rust-cache` is not being bumped; only its inputs change.

## How `rust-cache` actually builds a key

Read from the source at the pinned SHA `6323deb102c322ba6fcbdcafc7e3dddab59af2b6`
(`src/config.ts`), not from the README. Three facts below are load-bearing for
this design and each is easy to get wrong.

**1. Key shape.**

```text
<prefix-key>-<shared-key | key + GITHUB_JOB>-<OS>-<arch>-<env-hash>-<lock-hash>
```

`shared-key` **replaces** the job-id component rather than augmenting it. The
observed key `v0-rust-test-Linux-x64-6ff13d87-b7037afc` decomposes as
`v0-rust` / job `test` / `Linux` / `x64` / env-hash / lock-hash.

**2. `save-if` gates only the save.** The action's `post` step is
`dist/save.js`; `save-if: false` suppresses it and leaves restore untouched. A
PR run still restores, and GitHub's cache scoping lets a `refs/pull/N/merge` ref
read entries created on the default branch. So PR jobs keep their warm start.

**3. The env hash covers `CARGO*`.** `src/config.ts` hashes the rustc version
plus every environment variable whose name starts with one of:

```text
CARGO  CC  CFLAGS  CXX  CMAKE  RUST
```

Two consequences:

- `CARGO_PROFILE_DEV_DEBUG` (introduced below) lands **inside the cache key**.
  Any two jobs meant to share a key must declare it identically, or their keys
  silently diverge and the consolidation quietly undoes itself.
- This drift is not hypothetical, it is already present: **`msrv.yml` and
  `bench.yml` have no workflow-level `env:` block at all**, so they lack the
  `CARGO_TERM_COLOR: always` that `ci.yml`, `audit.yml`, `deny.yml`, `sbom.yml`
  and `integration.yml` all set. Their env hash already differs from every
  `ci.yml` job's. Cross-workflow key sharing requires normalizing that first.

Variables that do *not* enter the hash, and therefore do not block sharing:
`NIGHTLY_TOOLCHAIN` and `DOC_COVERAGE_THRESHOLD` (wrong prefix), and
`HELIKON_REQUIRE_SANDBOX` (wrong prefix). `RUSTDOCFLAGS` in the `docs` job is
set at *step* level on the `cargo doc` step, so it is not in the environment
when the cache step runs.

## Design

Four changes. A, B and C attack the two causes; D removes waste found along the
way.

### A. Restrict saving to the default branch

At every `Swatinem/rust-cache` site:

```yaml
- uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6
  with:
    save-if: ${{ github.ref == 'refs/heads/main' }}
```

The literal `'refs/heads/main'` is used rather than
`github.event.repository.default_branch` for consistency with the rest of the
repository, which hardcodes `main` in workflow triggers and in
`main-protection-checks.json`, and because a literal is greppable.

Per-workflow behaviour under this expression:

| Workflow | Triggers | Saves on |
| -- | -- | -- |
| `ci.yml` | push `main`, PR | push to `main` |
| `msrv.yml` | push `main`, PR | push to `main` |
| `audit.yml` | push `main`, PR, cron, dispatch | push to `main`, cron, dispatch (all resolve `github.ref` to `refs/heads/main`) |
| `deny.yml` | same as `audit.yml` | same |
| `integration.yml` | push `main`, PR, cron, dispatch | same |
| `bench.yml` | dispatch only | never — see D |
| `sbom.yml` | push tag `paigasus-helikon-v*` | never — see D |

### B. Cut debug info in CI

At the workflow `env:` level of every workflow that uses `rust-cache`:

```yaml
env:
  CARGO_TERM_COLOR: always
  CARGO_PROFILE_DEV_DEBUG: line-tables-only
```

The `test` profile inherits from `dev`, so this covers `cargo test` as well as
`cargo build`/`cargo clippy`/`cargo doc`. Debug info is typically 40–55% of a
`target` directory, so this shrinks **every** cache at once — the highest-leverage
single change here, and the only one that helps macOS and Windows, where key
consolidation cannot (see C).

`line-tables-only` rather than `0`: it keeps file and line numbers in panic
backtraces, so a Windows-only or macOS-only CI failure is still localizable from
the log. It drops variable and type information, which nothing in CI reads.

Safety, verified rather than assumed:

- The workspace declares **no `[profile.*]` sections at all** (root `Cargo.toml`
  and every member), so nothing is being overridden.
- Nothing in CI sets `RUST_BACKTRACE`; no test asserts on backtrace content.
- trybuild `.stderr` snapshots pin rustc *diagnostics*, which do not depend on
  debug info. The `backtrace` matches in those files are rustc's boilerplate
  "run with `RUST_BACKTRACE=1`" note.

`msrv.yml` and `bench.yml` gain a workflow-level `env:` block, which they
currently lack. This also brings their `CARGO_TERM_COLOR` into line — a
prerequisite for C.

One-time cost: `CARGO_PROFILE_DEV_DEBUG` is in the cache key, so the first run
after merge invalidates every existing entry and runs cold. This is expected and
is not a regression.

### C. One shared key, one writer per key

Add a single uniform `shared-key` to the `ci.yml`, `msrv.yml` and `bench.yml`
sites:

```yaml
    shared-key: helikon
```

Because OS, architecture and rustc version are appended *after* the shared key,
one uniform value is sufficient — the six `test` matrix legs remain six distinct
entries automatically, and `doc-coverage` (nightly toolchain) cannot collide with
the stable jobs. The consolidation lands exactly where it should:

**The six ubuntu + stable jobs collapse from six cache entries to one:**
`clippy`, `test (ubuntu-latest, stable)`, `build-no-default-features`, `docs`,
`sessions-it`, and `verify` (from `msrv.yml`).

macOS and Windows have exactly one job per toolchain, so `shared-key` is a no-op
there. This is worth stating because it looks as though it should help and does
not; on those platforms only B reduces size.

#### The one-writer invariant

Readers get `save-if: false` — a literal, **not** the `main` expression from A.
This is load-bearing.

`build-no-default-features` completes in roughly 90 seconds and builds the
smallest dependency set of the six. If it shared the write path with
`test (ubuntu-latest, stable)` (~15 minutes, full rlibs plus dev-dependencies),
it would win the save race and publish a thin cache under the shared key. Every
later run would restore that thin cache, rebuild what it lacks, and then be
unable to save because the key already exists — a permanent, silent degradation
with no red gate.

Exactly one writer per distinct key:

| Key (after `v0-rust-helikon-`) | Writer | Readers (`save-if: false`) |
| -- | -- | -- |
| `Linux-x64-<stable>` | `test (ubuntu-latest, stable)` | `clippy`, `build-no-default-features`, `docs`, `sessions-it`, `verify`, `bench` |
| `Linux-x64-<nightly>` | `doc-coverage` | — |
| `Darwin-arm64-<stable>` / `<1.94>` | `test` legs | — |
| `Windows_NT-x64-<stable>` / `<1.94>` | `test` legs | — |

Two details behind that table:

- **The writer is `test`, not `clippy`.** `cargo clippy` is a `cargo check`
  variant: it emits dependency `.rmeta` without `.rlib`, so a clippy-populated
  cache cannot satisfy `cargo test`. The reverse direction works — a full build
  leaves both.
- **The `test` step carries one `save-if` expression for all six legs.** Legs
  other than ubuntu-stable write their own distinct keys and must keep saving,
  so the step uses A's `main` expression; only the five ubuntu-stable *readers*
  outside the matrix are pinned to `false`.

`integration.yml` deliberately gets **no** `shared-key`, so `temporal-it` and
`agentcore-image` keep job-based keys and cannot collide with the ubuntu-stable
entry. They still get A and B.

Accepted edge case: the action's `post-if` is `success() || CACHE_ON_FAILURE`,
so if `test (ubuntu-latest, stable)` fails on `main` the shared entry is not
refreshed until the next green `main`. `main` is expected green; this is
recorded, not mitigated.

### D. Remove waste

- **`audit.yml`, `deny.yml`: `cache-targets: false`.** Neither `cargo-audit` nor
  `cargo-deny` compiles the workspace — they read `Cargo.lock` and an advisory
  database. Their `target` cache is pure waste. The existing
  `cache-directories: "~/.cargo/advisory-db"` / `"~/.cargo/advisory-dbs"` entries
  are the point of those cache steps and are preserved; `cache-targets: false`
  keeps the cargo registry and those directories, and drops only the target dir.
- **`sbom.yml`: remove the `rust-cache` step entirely.** It is triggered only by
  `push: tags: paigasus-helikon-v*`. Under A it could never save; and because
  `main` never runs `sbom`, no `sbom`-keyed entry exists for it to restore. It is
  100% dead weight after A. Removing it is clearer than leaving an inert step.
- **`bench.yml`: reader.** Dispatch-only and rare. It becomes a reader of the
  ubuntu-stable key (`shared-key: helikon`, `save-if: false`) so it gets a warm
  `~/.cargo/registry` — useful even though `cargo bench` uses the release profile
  and cannot reuse the dev artifacts — while consuming no budget.

### E. Drift guard

`scripts/check-cargo-profile-env-sync.sh`, modeled on the existing
`scripts/check-advisory-ignore-sync.sh` (same "header comment explains why this
exists" style).

It asserts that every workflow file containing `Swatinem/rust-cache` declares a
byte-identical set of workflow-level `CARGO_*` `env:` entries. Rationale, which
belongs in the script header: those variables are inside the cache key, so drift
between two workflows silently splits a shared key into two entries — the exact
failure this ticket exists to fix — and nothing goes red when it happens.

It runs as a step in the `fmt` job: the cheapest job in `ci.yml` and the only one
with no cache of its own to perturb. It is also added to the local-reproduction
list in `CLAUDE.md` and `CONTRIBUTING.md`.

Because `sbom.yml` loses its `rust-cache` step under D, it falls out of the
script's scope automatically.

## Projected budget

| Entry | Now | Projected |
| -- | -- | -- |
| ubuntu-stable (6 jobs → 1) | ~10.1 | ~1.3 |
| `test` ubuntu-1.94 | ~2.0 | ~1.0 |
| `test` macos-stable | 2.20 | ~1.1 |
| `test` macos-1.94 | ~1.7 | ~0.9 |
| `test` windows-stable | 3.10 | ~1.6 |
| `test` windows-1.94 | 1.07 | ~0.6 |
| `doc-coverage` (nightly) | 0.92 | ~0.5 |
| `audit` + `deny` (advisory DBs only) | target dirs | ~0.04 |
| `integration` (2 jobs) | ~4 | ~2.0 |
| `sbom`, `bench` | writes | 0 |
| **Total** | **~25 GB** | **~9.0 GB** |

**The ~50% figure for B is an estimate, not a measurement**, and it is the number
that decides whether this closes. Values marked `~` in the "Now" column are
inferred rather than observed.

If measurement lands above 10 GB, apply in order:

1. Drop `rust-cache` from `integration.yml` (`temporal-it`, `agentcore-image`).
   Both are signal-only, non-required, and largely nightly. Worth ~2 GB.
2. The Linear issue's step 3 — skip `trybuild_ui` on Windows. Worth ~1 GB off
   the largest remaining entry. Last, because it trades coverage.

Neither is in scope for this change.

## Acceptance

Measurement, not reasoning — per the issue.

1. Merge; let one push-to-`main` run of `ci.yml`, `msrv.yml`, `audit.yml`,
   `deny.yml` and `integration.yml` complete. Expect this run to be cold: B
   invalidates every existing key.
2. Let a **second** `main` run complete. This is the one that demonstrates
   restore.
3. Read the inventory with the **list** endpoint:

   ```bash
   gh api repos/SMK1085/paigasus-helikon/actions/caches --paginate \
     --jq '.actions_caches[] | "\(.size_in_bytes) \(.ref) \(.key)"'
   ```

   Use the list endpoint, not `actions/cache/usage`: the issue established that
   `active_caches_count` disagrees with the list (4 vs 6 rows) while the byte
   totals agree, so the count field is unreliable.
4. Assert:
   - total across all entries is **under 10 GB**;
   - `main` holds **one entry per distinct key** — 6 `test` legs (of which
     ubuntu-stable is the shared entry), `doc-coverage`, `audit`, `deny`,
     `temporal-it`, `agentcore-image`;
   - no entry exists under any `refs/pull/*` ref created *after* the merge;
   - the second run's `test (ubuntu-latest, stable)` logs
     `Restored from cache key "v0-rust-helikon-Linux-x64-…"`, and `clippy`,
     `docs`, `build-no-default-features` and `verify` log a restore from that
     **same** key — the direct evidence that consolidation works.

This revises the issue's criterion. "`main` holds an entry for every cached job"
is no longer the right test, because six jobs now deliberately share one entry;
"one entry per distinct key" is the equivalent statement under this design.

Non-goals for acceptance: a specific wall-clock target for any leg. Wall time is
a consequence, and the issue's own history shows single-run timings are a noisy
sample of a thrashing system.

## Out of scope

- Bumping `Swatinem/rust-cache` — only its inputs change.
- Adding `target/tests/trybuild` to `cache-directories` — explicitly forbidden
  above.
- Changing `--skip trybuild_ui` on any leg.
- A scheduled workflow that reaps `refs/pull/*/merge` caches for closed PRs.
  Under A, PRs stop creating entries at all, so it would have nothing ongoing to
  do. The ~9.5 GB of entries that already exist are cleared once, by hand, with
  `gh api --method DELETE`; that is an operational step in the PR description,
  not code.
- Renaming any job, or editing `.github/rulesets/main-protection-checks.json`.

## Documentation

- **`CLAUDE.md`, CI section** — the 10 GB budget as a standing constraint; the
  `save-if`-on-`main`-only rule; the one-writer-per-shared-key invariant and why
  `test` rather than `clippy` is the writer; the requirement that `CARGO_*`
  workflow env stays uniform because it is inside the cache key; and
  `check-cargo-profile-env-sync.sh` in the local-reproduction command list.
- **`docs/runbooks/ci-architecture.md`** — full rationale, the key-shape
  reference, the measurement procedure from [Acceptance](#acceptance), and the
  ordered fallback list.
- **`CONTRIBUTING.md`** — the new script in the contributor gate list.
- **mdBook and crate READMEs: deliberately not touched.** This is a pure-internal
  CI change — no public API, no crate roster change, no quickstart or feature-flag
  change. Recorded here as a conscious call, per `CLAUDE.md`, not a silent skip.

## Files touched

| File | Change |
| -- | -- |
| `.github/workflows/ci.yml` | `save-if` + `shared-key` at 6 sites; `save-if: false` on 4 readers; `CARGO_PROFILE_DEV_DEBUG` env; sync-check step in `fmt` |
| `.github/workflows/msrv.yml` | new `env:` block; `shared-key`; `save-if: false` (reader) |
| `.github/workflows/audit.yml` | `save-if`; `cache-targets: false`; `CARGO_PROFILE_DEV_DEBUG` env |
| `.github/workflows/deny.yml` | `save-if`; `cache-targets: false`; `CARGO_PROFILE_DEV_DEBUG` env |
| `.github/workflows/integration.yml` | `save-if`; `CARGO_PROFILE_DEV_DEBUG` env; no `shared-key` |
| `.github/workflows/bench.yml` | new `env:` block; `shared-key`; `save-if: false` (reader) |
| `.github/workflows/sbom.yml` | remove the `rust-cache` step |
| `scripts/check-cargo-profile-env-sync.sh` | new |
| `CLAUDE.md` | CI section |
| `CONTRIBUTING.md` | gate list |
| `docs/runbooks/ci-architecture.md` | rationale, key reference, measurement procedure |
