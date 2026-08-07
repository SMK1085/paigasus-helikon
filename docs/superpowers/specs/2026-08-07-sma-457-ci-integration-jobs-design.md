# SMA-457 — `temporal-it` + `agentcore-image` CI jobs design

**Status:** awaiting GATE 1 approval (Feature Factory Stage 2 pending)
**Ticket:** [SMA-457](https://linear.app/smaschek/issue/SMA-457/ci-add-temporal-it-and-agentcore-docker-build-jobs)
**Related:** [SMA-332](https://linear.app/smaschek/issue/SMA-332/paigasus-helikon-runtime-temporal-paigasus-helikon-runtime-agentcore) (shipped both suites as local-only by GATE 1 decision), SMA-330 (the `sessions-it` job this follows)

## 1. Context and goal

SMA-332 shipped two validation suites that CI does not exercise:

- **Temporal live integration suite** — `crates/paigasus-helikon-runtime-temporal/tests/temporal_live.rs`,
  six tests including `crash_resume_mid_tool_call`, the SMA-332 acceptance criterion. It is
  deliberately *not* `#[ignore]`d: it compiles on every PR (so it cannot bit-rot) and
  **loud-skips** when `TEMPORAL_TEST_SERVER` is unset. In CI today, every one of those six
  tests skips.
- **AgentCore image gates** — `scripts/agentcore-image-check.sh` builds both container
  images and asserts four acceptance criteria (two image sizes < 30 MB, two
  `docker run`→first-`/ping`-200 cold starts < 50 ms). It runs only by hand, per
  `docs/runbooks/agentcore-image-check.md`.

"Compiles but never executes" is a weaker guarantee than it looks. The Temporal suite's
value is entirely in its runtime behaviour — durable replay, retry timing, cancellation
semantics — none of which a compile check touches. The image gates are worth even less
unexecuted: a dependency bump that doubles the binary size or adds a blocking startup
path produces a green PR today.

**Goal:** wire both into `.github/workflows/ci.yml` as **signal-only** (non-required)
jobs, so regressions surface on the PR that causes them. Promotion of `temporal-it` to a
required check is explicitly out of scope and deferred until the job has a track record.

### Non-goals

- Making either job a required status check (see §7).
- Restructuring the Dockerfile for cross-run layer caching (e.g. `cargo-chef`). Out of
  scope; see §8 for why the cheaper within-run win is the right stopping point.
- Changing any test assertion, gate threshold *default*, or the suites' local behaviour.
  A developer running either suite locally after this change sees byte-identical
  behaviour to before it.

## 2. Verified facts this design rests on

Established by direct check against the live environment on 2026-08-07, not assumed:

| Fact | How verified | Consequence |
| --- | --- | --- |
| `SMK1085/paigasus-helikon` is **public** | `gh repo view --json visibility` → `PUBLIC` | GitHub's arm64 hosted runners (`ubuntu-24.04-arm`) are free for this repo — native arm64 builds, no qemu |
| `temporalio/cli` publishes versioned linux tarballs + `checksums.txt`; latest is **v1.8.2** | `gh api repos/temporalio/cli/releases/tags/v1.8.2` | The CLI can be installed from a pinned, checksum-verified first-party artifact |
| The `temporal_cli_1.8.2_linux_amd64.tar.gz` SHA-256 is `d8421bda989e6514b4bdb4d63a9012a8a05a806892e881a5aad8510496349a94` | fetched `checksums.txt` from the release | The literal can be pinned in the workflow rather than fetched at run time |
| `temporalio/setup-temporal` exists, is active (pushed 2026-08-03), has tags `v0.1.0`/`v0` but **no GitHub releases** | `gh api repos/temporalio/setup-temporal{,/tags,/releases/latest}` (last → 404) | CLAUDE.md's `releases/latest` SHA-pinning recipe does not apply cleanly; combined with 12 stars, this third-party action is rejected in favour of the pinned tarball (§4.1) |
| BuildKit **cache mounts work without a `# syntax=` directive** on the dev host's Docker 29.6.2 | built a throwaway `RUN --mount=type=cache,...` image — succeeded | No unpinned `docker/dockerfile:1` frontend image needs to enter the build (§5.2) |
| The workspace has **no git dependencies** | `grep -c 'source = "git+' Cargo.lock` → 0 | A cache mount on `$CARGO_HOME/registry` alone suffices; no `$CARGO_HOME/git` mount needed |
| `ci` and `workflows` are both allowed commit/PR-title scopes | `.versionrc` `scopeRegex`, `pr-title.yml` `scopes:` | Commits and PR title can use either without tripping `commits` or `pr-title` |

## 3. Shape of the change

Both jobs go in **`ci.yml`**, not a new workflow file. They share `ci.yml`'s triggers
(PR + push to `main`), its `concurrency` group, and its `permissions: contents: read`.
The ticket names `sessions-it` as the pattern, and `sessions-it` lives in `ci.yml`; the
supply-chain workflows are separate only because they have genuinely independent triggers
(a daily cron) and failure semantics, which these two do not.

Both follow the `sessions-it` skeleton exactly: checkout → `dorny/paths-filter` → a no-op
echo step when the filter misses → the real steps, each guarded by
`if: steps.filter.outputs.<name> == 'true'`.

**Why `if:`-guarded steps rather than a `job.if` on the filter output.** A skipped *job*
reports no status at all. Today that is merely untidy, but the moment `temporal-it` is
promoted to required (the explicit plan), a job that reports nothing blocks every PR that
does not touch Temporal — the exact failure mode recorded in
`feedback_verify_required_checks_reported`. Guarding at the step level means the job always
reports, green, having run nothing. `sessions-it` already does this, and it is required.

## 4. Job A — `temporal-it`

```yaml
temporal-it:
  runs-on: ubuntu-latest
  timeout-minutes: 30
```

`timeout-minutes` is load-bearing, not hygiene: a dev server that starts but never becomes
healthy would otherwise hold a runner for GitHub's 6-hour default.

**Path filter** (`temporal`):

```
crates/paigasus-helikon-runtime-temporal/**
crates/paigasus-helikon-core/src/**
Cargo.toml
Cargo.lock
.github/workflows/ci.yml
```

Core is included because the Temporal runner implements core's `Runner`/`Agent` contracts
and the suite asserts on core's `RunError` variants; `Cargo.lock` catches SDK bumps;
`ci.yml` self-reference means edits to the job re-run it.

**Steps, in order:**

1. `actions/checkout` (`persist-credentials: false`, matching every other job).
2. `dorny/paths-filter` → `steps.filter.outputs.temporal`.
3. No-op echo when the filter misses.
4. `dtolnay/rust-toolchain` @ `stable`.
5. **`arduino/setup-protoc`** — the SMA-332 lesson the ticket calls out. `temporalio-protos`
   compiles `.proto` files at build time via `prost-build` and needs a system `protoc`;
   without this the job fails at compile, not at test.
6. `Swatinem/rust-cache`.
7. Install the Temporal CLI (§4.1).
8. Start the dev server (§4.2).
9. Run the suite (§4.3).
10. `if: failure()` — dump the dev-server log (§4.4).

All of steps 4–10 carry the same `if: steps.filter.outputs.temporal == 'true'` guard. Step 10
combines it with the failure condition —
`if: failure() && steps.filter.outputs.temporal == 'true'` — because a bare `failure()`
would also fire for a failure in an unrelated earlier step, and because GitHub replaces the
implicit `success()` when any `if:` is present.

### 4.1 CLI install — pinned tarball

```bash
TEMPORAL_CLI_VERSION=1.8.2
TEMPORAL_CLI_SHA256=d8421bda989e6514b4bdb4d63a9012a8a05a806892e881a5aad8510496349a94
```

`curl -sSfL` the release tarball, verify with `sha256sum -c` against that **literal**, then
extract `temporal` onto `PATH`.

The checksum is pinned in the workflow rather than fetched from the release's own
`checksums.txt`, because a checksum fetched from the same host as the artifact only proves
the download was not corrupted in transit — it proves nothing about the artifact's
identity. A literal pinned in a reviewed, version-controlled file is a real assertion:
changing it requires a commit.

Rejected alternatives: `temporalio/setup-temporal` (a 12-star third-party action in the
trust path of a job that runs PR-authored code, with no releases to pin against per
CLAUDE.md's recipe); a digest-pinned `temporalio/temporal` container (structurally closer
to `sessions-it`, but adds container networking as a failure mode for a server that runs
perfectly well as a host binary, and the ticket explicitly prescribes the host install).

**Maintenance cost, stated plainly:** version and checksum are hand-bumped. Dependabot
tracks neither. This is the accepted price of keeping a third-party action out of the trust
path; the same trade-off already applies to `sessions-it`'s digest-pinned Postgres and
Redis images.

### 4.2 Dev server startup

```bash
temporal server start-dev --headless --ip 127.0.0.1 --port 7233 \
  > "${RUNNER_TEMP}/temporal-dev-server.log" 2>&1 &
```

`--headless` skips the Web UI (nothing consumes it). Output is redirected to a file
precisely so §4.4 has something to show. `start-dev` uses an in-memory store by default —
no database setup, and no state leaking between the six tests beyond what they already
isolate via per-test uuid task queues.

Readiness is then polled with a bounded loop (~30 × 2 s) on
`temporal operator cluster health --address 127.0.0.1:7233`, failing loudly with the server
log if the budget is exhausted. A fixed `sleep` is not used: it is simultaneously too long
on a fast runner and too short on a loaded one.

### 4.3 Running the suite — single attempt, no retry

```bash
TEMPORAL_TEST_SERVER=127.0.0.1:7233 \
  cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1
```

`--test-threads=1` matches the suite's own documented invocation. (Each test mints a uuid
task queue, so serialization is belt-and-braces against a shared dev server rather than a
correctness requirement — but the documented invocation is what was validated, and CI
should not quietly run something else.)

**Deliberately no retry loop, diverging from `sessions-it`.** `sessions-it` retries three
times because it is a *required* check where a flake blocks a merge. `temporal-it` starts
signal-only for the opposite reason: the entire purpose of this phase is to find out how
often `crash_resume_mid_tool_call` — which orchestrates a real worker abort against
wall-clock activity timeouts — actually flakes on shared hardware. A retry loop would erase
exactly the evidence needed to decide whether promotion is safe. A retry can be added at
promotion time, as an informed decision rather than a copied default.

### 4.4 Failure diagnostics

An `if: failure()` step `cat`s `${RUNNER_TEMP}/temporal-dev-server.log`.

This is three lines, no artifact upload, and no retry. Its justification: the first
question any red run raises is "did the server even come up?", and a signal-only job that
cannot answer it gets muted rather than fixed. Without the log, a startup failure and a
genuine crash-resume regression are indistinguishable from the job output.

## 5. Job B — `agentcore-image`

```yaml
agentcore-image:
  runs-on: ubuntu-24.04-arm
  timeout-minutes: 60
```

**Why arm64 is mandatory, not a preference.** The Dockerfile hardcodes
`--platform linux/arm64` because AWS Bedrock AgentCore's runtime targets are arm64
microVMs, and the runbook explicitly says not to switch it to amd64 for convenience.
Building that on an x86 runner means qemu emulation of a musl release build that compiles
aws-lc-rs's C and assembly — plausibly an hour or more, twice. GitHub's `ubuntu-24.04-arm`
runners are free for public repos (§2), so the native path is available and is the only
one that makes this job viable.

**Path filter** (`agentcore`), deliberately narrow:

```
crates/paigasus-helikon-runtime-agentcore/**
scripts/agentcore-image-check.sh
Cargo.lock
.github/workflows/ci.yml
```

Notably **absent**: `core`, `runtime-axum`, `runtime-tokio`, `mcp`,
`providers-anthropic`. Those are all compiled into the images, so a change there *can*
move the numbers — but the measured margins are wide (agent image at ~11% of the size
budget, cold start at ~20% of the latency budget), and `Cargo.lock` still catches the
dependency bumps that realistically move image size. The trade is explicit: a
first-party source change to a dependency crate can move the numbers without this job
running. Given the margins, that is an acceptable gap for a job this expensive, and the
job still runs before any release that touches `Cargo.lock`.

**The job body is one step**, plus checkout and the filter:

```yaml
- run: bash scripts/agentcore-image-check.sh
  env:
    AGENTCORE_COLD_START_LIMIT_MS: 250
```

No Rust toolchain, no `setup-protoc`, no `rust-cache` on the host — every compile happens
inside the Dockerfile's builder stage. (The SMA-332 protoc lesson applies to jobs that
compile the *workspace*; this one does not.)

### 5.1 `scripts/agentcore-image-check.sh` — env-overridable limits

```bash
SIZE_LIMIT_BYTES="${AGENTCORE_SIZE_LIMIT_BYTES:-$((30 * 1024 * 1024))}"
COLD_START_LIMIT_MS="${AGENTCORE_COLD_START_LIMIT_MS:-50}"
```

Defaults are today's values, so a local run and the runbook are unchanged.

The summary table and every failure message currently hardcode `< 30 MB` and `< 50 ms` as
literal display strings. Those become computed from the variables. This is not cosmetic:
a table that prints a gate the script is not enforcing is actively misleading, and the
whole point of CI running this script is that someone reads its output.

**CI overrides only the cold-start limit, to 250 ms.** The size gate is
environment-independent — 30 MB is 30 MB on any host — and stays at its default. Cold
start is not: the 9–11 ms measured on Docker Desktop is a property of that machine, and a
shared runner under contention is a different measurement instrument. 250 ms is roughly
5–10× the expected native-Linux figure: loose enough that runner noise does not redden the
job, tight enough that a real regression (a blocking initialisation, a synchronous network
call at startup) still trips it by an order of magnitude.

The alternative — running the script unmodified — was rejected because a signal-only job
that reddens for reasons unrelated to the code trains everyone to ignore it, which is
strictly worse than not having it.

### 5.2 Dockerfile — BuildKit cache mounts

```dockerfile
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/workspace/target,sharing=locked \
    cargo build --release --locked -p paigasus-helikon-runtime-agentcore \
    --example "${EXAMPLE}" ${FEATURES:+--features "${FEATURES}"} \
    && strip "target/release/examples/${EXAMPLE}" \
    && cp "target/release/examples/${EXAMPLE}" /agentcore-example
```

`/usr/local/cargo` is `rust:1.94-alpine`'s `CARGO_HOME`; this is asserted at
implementation time rather than assumed. Output images are bit-identical — cache mounts
change build inputs' availability, not the compiler's output.

**The load-bearing invariant, which gets a comment in the Dockerfile:** the `cp` out of
`target/` must stay inside this same `RUN`. A cache mount is not part of the image
filesystem, so splitting the `cp` into a later `RUN` (or a `COPY --from`) would silently
produce an image with no binary. That is a quiet failure mode a future editor will not
anticipate, which is why it is documented in the file rather than only here.

**What this buys, honestly:** the two builds share a BuildKit daemon within one job, so
the second image reuses the first's compiled dependencies — expect roughly 40–50% off the
second build. It does **not** help the first build, and it does not persist across runs:
every job gets a fresh runner with an empty cache. Cross-run caching would need
`cargo-chef` or `buildx --cache-to type=gha`, both of which are defeated by `COPY . .`
invalidating on every commit anyway. This is the cheap half of the win, taken; the
expensive half is a non-goal.

## 6. Documentation updated in the same PR

| File | Change |
| --- | --- |
| `CLAUDE.md` | CI section: "eight jobs" → ten. Describe both jobs, their signal-only status and promotion path, the arm64-runner rationale, and why CI's cold-start budget is deliberately looser than the runbook's. |
| `CONTRIBUTING.md` | Note the two non-required integration jobs. The required-contexts table is unchanged (§7). |
| `docs/runbooks/agentcore-image-check.md` | Document the two env overrides, that CI now runs the script, and that its looser cold-start budget is a deliberate instrument-difference rather than a relaxed AC. |
| `crates/paigasus-helikon-runtime-temporal/README.md` | Its testing section says the live suite is run locally; note that CI now runs it too. |

**mdBook: deliberately not touched.** CLAUDE.md requires this be a conscious call rather
than a silent skip. This change alters no public API, no quickstart or example flow, no
crate roster entry, and no documented concept — it is CI-internal. `docs/book/src/concepts/runtimes.md`
quotes the measured image sizes and cold starts, and those numbers do not change.

## 7. Deliberate non-changes

- **`.github/rulesets/main-protection-checks.json` is untouched.** Both jobs are
  signal-only per the ticket. Promoting `temporal-it` means adding `temporal-it` to that
  file *and* to CONTRIBUTING.md's required-contexts table — a separate, evidence-based
  decision once the job has a flake record.
- **No `continue-on-error` on either job.** It would make them report green
  unconditionally, which is not a weaker signal but the absence of one. Signal-only is
  achieved by *not* listing the contexts as required, not by suppressing failure.
- **No changes to any test, assertion, or default threshold.** Local behaviour of both
  suites is byte-identical after this change.

## 8. Risks

| Risk | Assessment |
| --- | --- |
| `crash_resume_mid_tool_call` flakes on shared hardware | Known and expected — it aborts a worker against wall-clock activity timeouts. This is the whole reason the job starts signal-only with no retry: to measure the rate rather than mask it. The suite's own backstops (60 s file waits, a 120 s run timeout) are generous. |
| `ubuntu-24.04-arm` capacity or queueing | Job is path-filtered to rarely run; a queue delay costs latency, not correctness. If arm capacity ever became unavailable, the fallback is qemu — slow enough that the job would need rethinking, not a silent switch. |
| Temporal CLI pin goes stale | Accepted, documented (§4.1). Not tracked by Dependabot; same posture as `sessions-it`'s pinned image digests. |
| `agentcore-image` wall clock | Two musl release builds with aws-lc-rs. Mitigated by the narrow filter (rare runs) and the cache mount (~40–50% off the second build), bounded by `timeout-minutes: 60`. |
| Narrow filter misses a dependency-crate regression | Explicit trade in §5. Wide margins on both gates plus `Cargo.lock` coverage make it acceptable. |

## 9. Acceptance criteria

1. `ci.yml` contains `temporal-it` and `agentcore-image`, both path-filtered, both
   reporting a status on every PR (green no-op when their filter misses).
2. On a PR touching the Temporal crate, `temporal-it` installs the checksum-verified CLI,
   starts a dev server, and runs all six `temporal_live` tests to completion — with zero
   `SKIPPED:` lines in its output.
3. On a PR touching the AgentCore crate, `agentcore-image` builds both arm64 images
   natively and reports all four gates, using the 250 ms CI cold-start budget.
4. `bash scripts/agentcore-image-check.sh` with no env set behaves exactly as before this
   change — same gates, same table, same exit codes.
5. Both images built with the cache-mounted Dockerfile still contain `/agentcore-example`
   and pass the size/cold-start gates.
6. Neither job appears in `.github/rulesets/main-protection-checks.json`.
7. The four docs in §6 are updated on the same branch; `mdbook build docs/book` stays
   clean.
8. Every CI gate in CLAUDE.md's "Common commands" list passes locally.
