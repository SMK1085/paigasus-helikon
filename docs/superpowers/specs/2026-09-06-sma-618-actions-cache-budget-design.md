# SMA-618 — Actions cache budget: stop the thrash

Design document. Revision 2 (post spec-challenge).
Linear: [SMA-618](https://linear.app/smaschek/issue/SMA-618/actions-cache-thrashes-at-37percent-over-the-10-gb-limit-pr-scoped)

Delivered as **two PRs** — see [Rollout](#rollout). SMA-618 closes on PR 2.

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

Two independent causes. The Linear issue names only the first.

### Cause 1 — PR jobs write caches

Every `Swatinem/rust-cache` site uses the default `save-if: true`, so each PR job
saves an entry scoped to `refs/pull/N/merge`. Those entries are read a handful of
times and die with the PR, but while they live they evict `main`'s. In the
inventory above, PRs #240 and #241 hold **9.54 GB of the 13.69 GB** — the entire
overage.

### Cause 2 — `main`'s own footprint exceeds the limit by itself

Not in the Linear issue, and it is what makes the issue's proposed fix
insufficient alone. A single push to `main` writes **15 cache entries**:

| Workflow | Cached jobs | Count |
| -- | -- | -- |
| `ci.yml` | `clippy`, `test` (6 legs), `build-no-default-features`, `docs`, `doc-coverage`, `sessions-it` | 11 |
| `msrv.yml` | `verify` | 1 |
| `audit.yml` | `audit` | 1 |
| `deny.yml` | `deny` | 1 |
| `integration.yml` | `temporal-it` **only** | 1 |

`agentcore-image` has **no** cache step — verified: `integration.yml` contains
exactly one `Swatinem/rust-cache` occurrence, at line 131, inside `temporal-it`.
`agentcore-image` builds inside Docker with BuildKit cache mounts, which
`docs/runbooks/ci-architecture.md` records as having no cross-run persistence.

The seven entries whose sizes have been observed already sum to **10.65 GB**
(3.10 + 2.61 + 2.20 + 1.07 + 0.92 + 0.55 + 0.20). The other eight are non-zero.

Directly observable consequence: on run `33992753929` — a push to `main` at
`4eb98545` — **9 of the 11 cached `ci.yml` jobs logged `No cache found.`**,
including `test (ubuntu-latest, stable)`, which then took 15 minutes. The two
that restored did so `full match: false`, i.e. prefix-only via the fallback
restore key. `main` is running nearly as cold as the PRs.

### What this means for the issue's plan

The issue's revised acceptance criterion — "`main` holds an entry for every
cached job" — is arithmetically unreachable at observed sizes, whoever writes.
This design therefore pairs `save-if` with size reduction and restates the
criterion (see [Acceptance](#acceptance)).

## Constraints

From the Linear issue and `CLAUDE.md`. All are preserved.

- **`--skip trybuild_ui` on the `1.94` legs must stay.** It exists for an
  unrelated reason: `.stderr` snapshots pin rustc diagnostic text byte-for-byte
  and that text drifts across releases.
- **No job may be renamed.** The required-status-check contexts in
  `.github/rulesets/main-protection-checks.json` are bare job names; a rename
  silently un-gates `main`.
- **`test (windows-latest, stable)` stays required.** It is the only gate
  exercising the Windows timeout path in `paigasus-helikon-tools` (`cmd /C`
  spawning, `TerminateProcess` kill). Nothing here touches that path.
- **Actions stay SHA-pinned** with the version in a comment. `Swatinem/rust-cache`
  is not bumped; only its inputs change.
- **Do not add `target/tests/trybuild` to `cache-directories`** — but *not* for
  the reason the Linear issue gives. See the correction below.

### Correction to the issue: the trybuild directory is already cached

The issue says adding `target/tests/trybuild` to `cache-directories` "would grow
the very cache that is already being evicted". That is wrong. It would be a
**no-op**, because `rust-cache` already caches it deliberately.

Verified in `src/cleanup.ts` at the pinned SHA `6323deb…`, lines 43–57:

```ts
// workspaces under `target/tests`. Notably, `target/tests/target` and
// `target/tests/trybuild`.
if (path.basename(profileDir) === "tests") {
  cleanTargetDir(path.join(profileDir, "trybuild"), packages, checkTimestamp);
  await rmExcept(profileDir, new Set(["target", "trybuild"]), checkTimestamp);
```

`rmExcept(…, {"target", "trybuild"})` **preserves** trybuild's nested dependency
graph. That is what the measured 2.03 GB delta between the Windows stable
(3.10 GB, runs trybuild) and Windows 1.94 (1.07 GB, skips it) entries *is*.

The instruction stands — do not add it — but as "no-op", not "backwards". The
distinction matters because it makes trybuild's cache footprint an explicitly
managed quantity rather than an accident, which is what change E acts on.

## How `rust-cache` builds a key

Read from source at the pinned SHA `6323deb102c322ba6fcbdcafc7e3dddab59af2b6`,
not from the README.

**1. Key shape** (`src/config.ts`):

```text
<prefix-key>-<shared-key | key + GITHUB_JOB>-<OS>-<arch>-<env-hash>-<lock-hash>
```

`shared-key` **replaces** the job-id component rather than augmenting it. The
observed key `v0-rust-test-Linux-x64-6ff13d87-b7037afc` decomposes as `v0-rust` /
job `test` / `Linux` / `x64` / env-hash / lock-hash.

**The rustc version is not a separate field — it is hashed *into* the env-hash**,
together with the environment variables below. This matters: it means the
env-hash (and therefore the prefix restore key) is already toolchain-safe, so
`stable`, `1.94` and the nightly pin can never collide even under an identical
`shared-key`.

**2. `save-if` gates only the save.** The action's `post` step is `dist/save.js`;
`save-if: false` suppresses it and leaves restore untouched. A PR run still
restores, and GitHub's cache scoping lets a `refs/pull/N/merge` ref read entries
created on the default branch. PR jobs keep their warm start.

**3. The env-hash covers these prefixes:** `CARGO`, `CC`, `CFLAGS`, `CXX`,
`CMAKE`, `RUST`. So `CARGO_PROFILE_DEV_DEBUG` (change B) lands *inside the cache
key*, and any two jobs meant to share a key must declare it identically.

That drift is already present, not hypothetical: **`msrv.yml` and `bench.yml`
have no workflow-level `env:` block at all**, so they lack the
`CARGO_TERM_COLOR: always` that `ci.yml`, `audit.yml`, `deny.yml`, `sbom.yml` and
`integration.yml` set. Their env-hash already differs from every `ci.yml` job's.
Cross-workflow sharing requires normalizing that first.

Checked and confirmed *not* to interfere:

- `NIGHTLY_TOOLCHAIN`, `DOC_COVERAGE_THRESHOLD`, `HELIKON_REQUIRE_SANDBOX` —
  wrong prefix.
- `RUSTDOCFLAGS` in `docs` — set at *step* level on the `cargo doc` step, so it
  is absent from the environment when the cache step runs.
- `PROTOC` / `PROTOC_INCLUDE`, exported to `$GITHUB_ENV` by
  `.github/actions/setup-protoc/install.sh` (lines 107–110) — wrong prefix.
- `RUSTUP_TOOLCHAIN`, if `dtolnay/rust-toolchain` exports it: prefix `RUST`, so
  it *is* hashed, but its value is `stable` at every intended sharer. The
  implementation task confirms this from a job log before relying on sharing.

**4. The cached *path list* is folded into the cache version.** `@actions/cache`
derives a cache version from the path list, so an entry saved with `target/` in
its paths is unreachable from a job whose paths omit it — **even with a
byte-identical key**. Consequence: `cache-targets`, `cache-directories`,
`workspaces` and `cache-workspace-crates` must be identical across every site
sharing a `shared-key`, not just the `CARGO_*` env. This is why the design needs
*two* shared keys rather than one (change C).

## Design

### A. Restrict saving to the default branch — *PR 1*

At every `Swatinem/rust-cache` site:

```yaml
  with:
    save-if: ${{ github.ref == 'refs/heads/main' }}
```

Literal `'refs/heads/main'` rather than
`github.event.repository.default_branch`, for consistency with the workflow
triggers and `main-protection-checks.json`, and because a literal is greppable.

| Workflow | Triggers | Saves on |
| -- | -- | -- |
| `ci.yml`, `msrv.yml` | push `main`, PR | push to `main` |
| `audit.yml`, `deny.yml`, `integration.yml` | push `main`, PR, cron, dispatch | push to `main`, cron, dispatch (all resolve `github.ref` to `refs/heads/main`) |
| `bench.yml` | dispatch only | reader — see D |
| `sbom.yml` | push tag | reader — see D |

Fork PRs never save under this expression, which is the desired behaviour and is
noted so nobody "fixes" it later.

### B. Cut debug info in CI — *PR 2*

At the workflow `env:` level of every workflow using `rust-cache`:

```yaml
env:
  CARGO_TERM_COLOR: always
  CARGO_PROFILE_DEV_DEBUG: line-tables-only
```

The `test` profile inherits from `dev`, so this covers `cargo test` as well as
`cargo build` / `clippy` / `doc`. `line-tables-only` rather than `0`: it keeps
file and line numbers in panic backtraces, so a Windows-only or macOS-only CI
failure stays localizable from the log, while dropping variable and type
information that nothing in CI reads.

**Verified locally on 2026-09-06**, not assumed. Cargo validates this variable
and accepts the value:

```console
$ CARGO_PROFILE_DEV_DEBUG=bogus-value cargo check -p paigasus-helikon-macros
Caused by:
  invalid value: string "bogus-value", expected a boolean, 0, 1, 2, "none",
  "limited", "full", "line-tables-only", or "line-directives-only"

$ CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo check -p paigasus-helikon-macros
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.80s
```

The control test matters: it proves cargo *rejects* bad values, so acceptance of
`line-tables-only` is meaningful rather than silent ignoring. Fallback if a
future cargo narrows this: `CARGO_PROFILE_DEV_DEBUG=1`, also accepted, also
emits line tables.

Further safety, verified rather than assumed:

- The workspace declares **no `[profile.*]` sections at all**, so nothing is
  being overridden.
- Nothing in CI sets `RUST_BACKTRACE`; no test asserts on backtrace content.
- trybuild `.stderr` snapshots pin rustc *diagnostics*, which do not depend on
  debug info.

`msrv.yml` and `bench.yml` gain a workflow-level `env:` block, which they
currently lack — a prerequisite for C.

One-time cost: this variable is in the cache key, so the first `main` run after
PR 2 invalidates every entry and runs cold. Expected, not a regression.

### C. Two shared keys, one writer each — *PR 2*

The naive form of this change — one `shared-key` for all six ubuntu-stable
jobs — is wrong, and the reason is worth stating because it is not obvious.

Cargo fingerprints an artifact against its exact resolved feature set and rustc
version, and the workspace uses `resolver = "2"`. So a target directory built by
`cargo test --workspace --all-features` is reusable only by a job building the
same union under the same toolchain:

| Job | Builds | Target reuse from an `--all-features` cache |
| -- | -- | -- |
| `clippy` | `--workspace --all-features --all-targets` | **Yes.** `RUSTC_WORKSPACE_WRAPPER` refingerprints only workspace members; dependency rlibs are reused. |
| `docs` | `--workspace --all-features --no-deps` | **Yes.** |
| `build-no-default-features` | `--no-default-features` on 3 packages | **No.** A completely different feature unification. |
| `sessions-it` | `-p …-sessions-postgres -p …-sessions-redis`, default features | **No.** Under resolver v2 the shared deps (sqlx, tokio, redis) resolve to a strict subset and will not match. |
| `verify` (msrv) | `cargo msrv verify` across many toolchains | **No.** Every artifact is refingerprinted by rustc version. |
| `bench` | `cargo bench` (release profile) | **No.** Different profile. |
| `temporal-it` | `-p …-runtime-temporal --test temporal_live` | **Partial at best**, same resolver-v2 reason. |

Making the bottom five readers of a ~1.3 GB target-bearing cache would have them
download a payload they cannot use. `build-no-default-features` runs in ~90
seconds today; the restore alone could exceed that. It and `sessions-it` are
**required** contexts.

So: **two keys**, split by what each group can actually use.

```yaml
# Group 1 — target + registry.
    shared-key: helikon

# Group 2 — registry only.
    shared-key: helikon-registry
    cache-targets: false
```

They must be separate keys, not the same key with different `cache-targets`,
because the path list is in the cache version (fact 4 above) — an identical key
with a differing path list is a guaranteed miss, silently.

| Key (after `v1-rust-`) | Payload | Writer | Readers (`save-if: false`) |
| -- | -- | -- | -- |
| `helikon-Linux-x64-<stable>` | target + registry | `test (ubuntu-latest, stable)` | `clippy`, `docs` |
| `helikon-<other OS/toolchain>` | target + registry | the 5 other `test` legs | — |
| `helikon-registry-Linux-x64-<stable>` | registry only | `build-no-default-features` | `sessions-it`, `verify`, `bench`, `temporal-it`, `sbom` |
| `doc-coverage-Linux-x64-<nightly>` | target + registry | `doc-coverage` | — (job-based key; no `shared-key`) |
| `audit-…`, `deny-…` | registry + advisory DB | themselves | — |

Notes on the table:

- **The `helikon` writer is `test`, not `clippy`.** `cargo clippy` is a
  `cargo check` variant: it emits dependency `.rmeta` without `.rlib`, so a
  clippy-populated cache cannot satisfy `cargo test`. The reverse works.
- **A single `shared-key: helikon` on the `test` step is correct for all six
  legs** — OS, arch and the rustc-bearing env-hash partition them automatically,
  so the step keeps A's `main` expression and each leg writes its own key.
- **The `helikon-registry` writer gets `cargo fetch --locked` as its first step.**
  Without it, `build-no-default-features` would publish a registry containing
  only the no-default-features subset — the same thin-cache trap as below, one
  level down. `cargo fetch --locked` populates from `Cargo.lock`, which carries
  the full dependency set regardless of features.
- **`doc-coverage` gets no `shared-key`.** It is the sole occupant of its key,
  so a shared key buys nothing and only adds surface.

#### The one-writer invariant

Readers get literal `save-if: false`, **not** A's `main` expression. This is
load-bearing.

`build-no-default-features` completes in ~90 seconds and builds the smallest
dependency set. If it shared the write path for `helikon` with
`test (ubuntu-latest, stable)` (~15 minutes, full rlibs plus dev-dependencies),
it would win the save race and publish a thin cache. Every later run would
restore that thin cache, rebuild what it lacks, then be unable to save because
the key already exists — a permanent, silent degradation with no red gate.

**Writers additionally get `cache-on-failure: "true"`.** Without it the action's
`post-if: success()` means a red or cancelled `main` run leaves every reader of
that key cold until the next green `main` — and because `ci.yml` sets
`cancel-in-progress: false` for pushes, a pending `main` run can be cancelled
before it ever writes (the SMA-479 mechanism recorded in `audit.yml`). A
`cargo test` failure occurs *after* compilation, so the target directory is
complete and valid; there is no downside. `integration.yml` line 133 already
does exactly this for `temporal-it`.

### D. Remove waste — *PR 1*

- **`audit.yml`, `deny.yml`: `cache-targets: false`.** Neither `cargo-audit` nor
  `cargo-deny` compiles the workspace. Their `target` cache is waste; the
  existing `cache-directories` advisory-DB entries are the point of those steps
  and are preserved. Note honestly that this does **not** shrink them to nothing:
  `cache-targets: false` still caches `~/.cargo`, so each keeps a registry
  (`~/.cargo/registry` measures 1.3 GB uncompressed on the dev host). They cannot
  join `helikon-registry` because their `cache-directories` differ, which changes
  the path list and therefore the cache version. One-time cost: their existing
  entries become unreachable and both go cold once.
- **`sbom.yml`: reader, not removal.** It is tag-triggered only, so under A it
  could never save, and no `main`-written `sbom`-keyed entry exists to restore —
  its cache step is currently inert. Rather than delete it, make it a
  `helikon-registry` reader: the release path then gets `main`'s warm registry at
  **zero** budget cost. That is the one place a cold registry costs wall-clock on
  a human-blocking operation.
- **`bench.yml`: reader** of `helikon-registry`. Dispatch-only and rare; it gets a
  warm registry and consumes no budget.
- **`temporal-it`: reader** of `helikon-registry`. Its dedicated entry costs ~3 GB
  and `integration.yml` lines 58–61 already record that "rust-cache usually misses
  here: the job is path-filtered, so pushes to `main` rarely populate the cache
  other branches read from". Converting a documented usual-miss into a usual-hit
  while freeing ~3 GB is strictly better than either keeping it or (as an earlier
  draft proposed) deleting its cache outright.

### E. Skip `trybuild_ui` on Windows — *PR 2*

Replace the matrix `test_args` with an expression, per the Linear issue:

```yaml
      - run: >-
          cargo test --workspace --all-features
          ${{ (matrix.toolchain == 'stable' && matrix.os != 'windows-latest') && '' || '-- --skip trybuild_ui' }}
```

and delete the two `include:` entries carrying `test_args`. The expression form
avoids depending on GitHub's later-`include`-entry-wins merge rule.

Justification, both halves measured:

- **Coverage cost is nil.** The `.stderr` snapshots pass byte-identically on all
  three OSes, so Windows adds no distinct trybuild signal. ubuntu and macOS keep
  running the 21 UI cases.
- **Budget win is ~2 GB.** At the same commit on `main`, the two Windows entries
  differ only by this flag: 3.10 GB with trybuild, 1.07 GB without.
- **Wall-clock win is ~13 minutes.** The measured cold trybuild build on
  windows-stable is 819–826 s.

This is promoted from the issue's "last resort" to a first-class change because
the [sensitivity analysis](#budget) shows the budget does not reliably fit
without it.

**Rejected alternative: `rm -rf target/tests` before the save.** It achieves the
same ~2 GB with zero coverage loss, and was raised in review as strictly better.
It is not: the 819 s figure above *is* the cold trybuild dependency build, so
deleting that directory from the cache pays ~13 minutes of Windows wall-clock on
every run — the opposite of this ticket's goal. Recorded here so it is not
re-proposed.

### F. Drift guard — *PR 1 (env), extended in PR 2 (keys)*

`scripts/check-cargo-profile-env-sync.sh`, modeled on
`scripts/check-advisory-ignore-sync.sh` — same "header comment explains why this
exists" style, same honest statement of limitations. Bash plus `yq`, preflighted
the way `scripts/apply-repo-config.sh` preflights `jq`. The implementation task
confirms `yq` is present on the `ubuntu-latest` image and adds an install step to
the `fmt` job if it is not.

Three assertions, not one:

1. **Env uniformity.** Every workflow whose sites share a `shared-key` declares
   an identical set of workflow-level `env:` entries matching
   `^(CARGO|CC|CFLAGS|CXX|CMAKE|RUST)`. Scoped to the sharing set, so a future
   `ci.yml`-only variable is not blocked by a guard protecting nothing.
2. **No hidden env.** No *job-level* `env:` with those prefixes in any job
   containing a `rust-cache` step, and no `env:` on a `rust-cache` step itself.
   Job-level env is present when the cache step runs; step-level env on *other*
   steps (like `RUSTDOCFLAGS` on `docs`' `cargo doc` step) is not, and is
   deliberately allowed.
3. **One writer, one path list per key.** For each distinct `shared-key` value,
   at most one site whose `save-if` is not literally `false`, and all sites with
   that value declare identical `cache-targets` / `cache-directories` /
   `workspaces` / `cache-workspace-crates`.

Assertion 3 is the one that guards the invariant the design calls load-bearing.
Without it, a future PR adding an ubuntu-stable job by copying the `test` block —
which carries `shared-key: helikon` *and* a truthy `save-if` — silently creates a
second writer and reproduces the exact degradation described above.

Runs as a step in `fmt`: the cheapest job and the only one with no cache to
perturb. Gets a self-test in the style of `.github/actions/setup-protoc/selftest.sh`.

Incidental finding to fix while here: `docs/runbooks/ci-architecture.md` line 31
claims `actionlint` and `shellcheck` run in CI. They do not — neither appears in
any workflow. Correct the runbook.

### G. Budget monitor — *PR 1*

The repository sat at 37% over for months with nothing red. After this change the
same silence returns, and there are now *more* ways to drift over. Add a step to
`audit.yml`'s existing daily `main` cron that reads `actions/caches` and emits
`::warning::` above 8.5 GB.

This is what makes the 10 GB budget an enforceable standing constraint in
`CLAUDE.md` rather than an aspiration. It needs the `actions: read` permission.

### `prefix-key: v1`

Bump `prefix-key` from the default `v0-rust` to `v1-rust` in PR 2. Change B
invalidates every key anyway, so this is free, and it makes the transition
legible: everything `v0-*` is provably stale, so the post-merge purge becomes
`grep '^v0-'` rather than a blind delete-everything.

## Budget

Baseline figures are compressed bytes — the unit GitHub's `size_in_bytes` and the
10 GB limit use. Rows marked † are inferred, not observed.

Per-entry composition: registry ≈ 0.5 GB compressed (the dev host's
`~/.cargo/registry` is 1.3 GB uncompressed) and is **not** reduced by change B.
Only the target portion is.

| Entry | Now | Notes |
| -- | -- | -- |
| `helikon` Linux stable | 2.61 | absorbs `clippy` (~2.6†), `docs` (0.55) |
| `helikon` Linux 1.94 | ~2.0† | |
| `helikon` macOS stable / 1.94 | 2.20 / ~1.7† | |
| `helikon` Windows stable | 3.10 → **1.07** | change E |
| `helikon` Windows 1.94 | 1.07 | |
| `doc-coverage` | 0.92 | |
| `helikon-registry` | ~0.5† | absorbs `build-no-default-features`, `sessions-it`, `verify` (0.20), `bench`, `temporal-it` (~3†), `sbom` |
| `audit` + `deny` | ~1.0† | registry + advisory DB each |

**Sensitivity to change B**, applying the saving only to the target portion:

| Compressed debug saving | Projected total |
| -- | -- |
| 50% | ~9.0 GB |
| 40% | ~9.8 GB |
| 30% | ~10.7 GB — **over** |
| 0% | ~13.1 GB — over |

**The design fits at a compressed saving of roughly 40% or better and is marginal
below that.** The 40–55% figure commonly cited for debug info is an
*uncompressed* target-directory statistic; DWARF compresses exceptionally well,
so the compressed saving will be lower. This is the single largest uncertainty in
the design and it is deliberately not hidden in a point estimate.

Two consequences for the plan:

1. **Measure before writing PR 2's YAML.** Task 1 of PR 2's plan is: build the
   workspace with and without `CARGO_PROFILE_DEV_DEBUG=line-tables-only`, compress
   each cleaned `target/` the way `@actions/cache` does (zstd), record both
   numbers and the `~/.cargo/registry` compressed floor in this document. If the
   saving is under 40%, apply the fallback below *in the same PR* rather than
   shipping a design known not to fit.
2. **Remaining fallback rung**, if measurement still lands over 10 GB: stop
   caching the three non-required `1.94` legs (~3.1 GB at 30% saving). They are
   signal-only. Cost: those legs run cold — acceptable for ubuntu and macOS;
   windows-1.94 is the one to watch, though change E has already removed trybuild
   from it.

Note the dev host is arm64 macOS, so a local measurement is direct evidence for
macOS only. Linux and Windows-MSVC (which keeps debug info in PDBs) may differ,
and Windows is both the largest entry and the one where B is the only lever left
after E. The measurement is a floor on confidence, not a guarantee — which is
precisely why the rollout is staged.

## Rollout

Two PRs. The review made the decisive point: `save-if` means the PR run of this
change exercises only the *reader* path, so the write path, the one-writer
invariant and the whole budget are first exercised post-merge on `main`, and
`ci.yml` has no `workflow_dispatch` for a controlled dry run.

**PR 1 — `save-if` + waste + guard + monitor.** Changes A, D, F (assertions 1–2),
G, plus the purge. No cache-key invalidation. Independently recovers the 9.54 GB
of PR-scoped entries that constitute the entire current overage. Effect is
cleanly attributable.

**PR 2 — size reduction + consolidation.** Changes B, C, E, F (assertion 3),
`prefix-key: v1`. Key-invalidating; informed by PR 1's measurement. SMA-618
closes here.

E is in PR 2 rather than PR 1 despite not touching any key: it changes what a
required gate executes, which is a different risk class from PR 1's
cache-plumbing changes and deserves its own review attention.

### Rollback

- **PR 1** reverts cleanly. Reverting A restores PR-scoped saving; nothing is
  invalidated. Reverting D's `cache-targets: false` orphans the registry-only
  `audit`/`deny` entries once more — one cold run each, no correctness impact.
- **PR 2 costs a second cold cycle in either direction.** Reverting B changes the
  env-hash back, invalidating every entry a second time. Budget for one fully
  cold `main` run on revert; do not revert PR 2 as a reflex to a single slow run.
- **Partial rollback order**, most to least likely to be the culprit: E (a
  behaviour change on a required gate) → C (key consolidation) → B (env). F and G
  are inert with respect to caching and need never be reverted for a cache
  problem.

## Acceptance

Measurement, not reasoning — per the issue. The list endpoint, never
`actions/cache/usage`: the issue established that `active_caches_count`
disagrees with the list (4 vs 6 rows) while byte totals agree.

```bash
gh api repos/SMK1085/paigasus-helikon/actions/caches --paginate \
  --jq '.actions_caches[] | "\(.size_in_bytes) \(.ref) \(.key)"'
```

### PR 1

1. Merge.
2. **Purge the pre-existing entries — before any measurement.** Ordering is
   load-bearing: leaving ~9.5 GB of stale entries alongside fresh ones means
   GitHub evicts the new ones as fast as they are written, and step 4 measures a
   thrashing system — a false negative caused by the cleanup, not the design.

   ```bash
   gh api --paginate repos/SMK1085/paigasus-helikon/actions/caches \
     --jq '.actions_caches[].id' \
   | xargs -I{} gh api --method DELETE \
       repos/SMK1085/paigasus-helikon/actions/caches/{}
   ```

   Needs `actions: write`. **This is not one-time:** every PR still based on
   pre-merge `main` runs the *old* workflow definitions on its next push and
   keeps writing `refs/pull/N/merge` entries until rebased. Re-purge, or wait for
   all open PRs to rebase.
3. Let one push-to-`main` run complete across `ci.yml`, `msrv.yml`, `audit.yml`,
   `deny.yml`, `integration.yml`.
4. Assert: **no entry exists under any `refs/pull/*` ref** created after the
   merge, and `main` holds 15 entries. Record the total and every individual
   size — this is the measured baseline PR 2's budget is re-derived from, and it
   replaces every `†` in the table above.

### PR 2

1. Merge; purge again (change B invalidates everything, so all `v0-*` entries are
   provably stale: `grep '^v0-'`).
2. Let a **first** `main` run complete — expected fully cold.
3. Let a **second** `main` run complete. This is the one that demonstrates
   restore.
4. Assert:
   - total across all entries is **under 10 GB**;
   - `main` holds **one entry per distinct key**: 6 `helikon` entries (one per
     OS × toolchain), `helikon-registry`, `doc-coverage`, `audit`, `deny` — 10
     entries. Note `temporal-it` no longer has an entry of its own, and
     `agentcore-image` never did;
   - `test (ubuntu-latest, stable)` logs
     `Restored from cache key "v1-rust-helikon-Linux-x64-…"`, and `clippy` and
     `docs` log a restore from that **same** key;
   - `sessions-it`, `verify`, `bench` and `temporal-it` log a restore from
     `v1-rust-helikon-registry-Linux-x64-…`;
   - **wall-clock sanity check** on `build-no-default-features` and `sessions-it`
     — both are required gates that trade an exact-match cache for a
     registry-only one, so their runtimes must be recorded and compared against
     the PR 1 baseline. Restoring is not the same as benefiting; without this
     check the design is constructed so that a regression on these two cannot be
     observed.

Non-goal: a wall-clock target for any *other* leg. The issue's own history shows
single-run timings are a noisy sample of a thrashing system. The two checks above
are exceptions because they guard a known, deliberate trade.

## Out of scope

- Bumping `Swatinem/rust-cache` — only its inputs change.
- Adding `target/tests/trybuild` to `cache-directories` — a no-op, per the
  correction above.
- Changing `--skip trybuild_ui` on the `1.94` legs.
- A scheduled workflow reaping `refs/pull/*/merge` caches. Under A, PRs stop
  creating entries, so it would have nothing ongoing to do; the existing ones are
  cleared by the purge step.
- Renaming any job, or editing `.github/rulesets/main-protection-checks.json`.
- **`release-plz.yml`** — it runs `cargo publish --verify` on every push to
  `main`, compiling the whole workspace with **no cache at all**. Deliberately
  untouched: adding one would consume budget on the release path this design is
  trying to free. Stated so the next reader knows it was considered, not missed.
- **`docs.yml`** (the required `book-build` gate) and **`pr-title.yml`** — no
  Rust compilation, no `rust-cache`, nothing to do.

## Documentation

- **`CLAUDE.md`, CI section** — the 10 GB budget as a standing constraint and the
  daily monitor that enforces it; `save-if`-on-`main`-only; the two shared keys,
  the one-writer invariant, and why `test` rather than `clippy` is the writer;
  the requirement that `CARGO_*` env *and* the cached path list stay uniform
  across a shared key; `check-cargo-profile-env-sync.sh` in the local-reproduction
  list; and that Windows no longer runs `trybuild_ui`.
- **`docs/runbooks/ci-architecture.md`** — full rationale, the key-shape
  reference, the purge and measurement procedures, the rollback order, and the
  fallback rung. Also fix the stale `actionlint`/`shellcheck` claim on line 31.
- **`CONTRIBUTING.md`** — the new script in the contributor gate list.
- **mdBook and crate READMEs: deliberately not touched.** Pure-internal CI change
  — no public API, crate roster, quickstart or feature-flag change. A conscious
  call per `CLAUDE.md`, not a silent skip.

## Files touched

**PR 1**

| File | Change |
| -- | -- |
| `.github/workflows/ci.yml` | `save-if` at 6 sites; `check-cargo-profile-env-sync.sh` step in `fmt` |
| `.github/workflows/msrv.yml` | `save-if` |
| `.github/workflows/audit.yml` | `save-if`; `cache-targets: false`; budget-monitor step on the cron; `actions: read` |
| `.github/workflows/deny.yml` | `save-if`; `cache-targets: false` |
| `.github/workflows/integration.yml` | `save-if` |
| `.github/workflows/bench.yml`, `sbom.yml` | `save-if` |
| `scripts/check-cargo-profile-env-sync.sh` | new — assertions 1–2 |
| `CLAUDE.md`, `CONTRIBUTING.md`, `docs/runbooks/ci-architecture.md` | as above |

**PR 2**

| File | Change |
| -- | -- |
| `.github/workflows/ci.yml` | `CARGO_PROFILE_DEV_DEBUG` env; `prefix-key: v1`; `shared-key: helikon` on `test`, `clippy`, `docs`; `shared-key: helikon-registry` + `cache-targets: false` on `build-no-default-features`, `sessions-it`; literal `save-if: false` on `clippy`, `docs`, `sessions-it`; `cache-on-failure` on `test` and `doc-coverage`; `cargo fetch --locked` in `build-no-default-features`; matrix `test_args` → expression (change E) |
| `.github/workflows/msrv.yml`, `bench.yml` | new `env:` block; `shared-key: helikon-registry`; `cache-targets: false`; `save-if: false` |
| `.github/workflows/sbom.yml` | `env:` addition; `shared-key: helikon-registry`; `cache-targets: false`; `save-if: false` |
| `.github/workflows/integration.yml` | `env:` addition; `shared-key: helikon-registry`; `cache-targets: false`; `save-if: false` on `temporal-it` |
| `.github/workflows/audit.yml`, `deny.yml` | `env:` addition; `prefix-key: v1` |
| `scripts/check-cargo-profile-env-sync.sh` | assertion 3 |
| `CLAUDE.md`, `docs/runbooks/ci-architecture.md` | as above |
