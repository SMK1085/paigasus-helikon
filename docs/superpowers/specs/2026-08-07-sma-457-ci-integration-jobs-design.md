# SMA-457 — `temporal-it` + `agentcore-image` CI jobs design

**Status:** revised after adversarial challenge (Feature Factory Stage 2) — awaiting GATE 1 approval
**Ticket:** [SMA-457](https://linear.app/smaschek/issue/SMA-457/ci-add-temporal-it-and-agentcore-docker-build-jobs)
**Related:** [SMA-332](https://linear.app/smaschek/issue/SMA-332/paigasus-helikon-runtime-temporal-paigasus-helikon-runtime-agentcore) (shipped both suites as local-only by GATE 1 decision), SMA-330 (the `sessions-it` job this borrows from)

## 1. Context and goal

SMA-332 shipped two validation suites that CI does not exercise:

- **Temporal live integration suite** — `crates/paigasus-helikon-runtime-temporal/tests/temporal_live.rs`,
  six tests including `crash_resume_mid_tool_call`, the SMA-332 acceptance criterion. It is
  deliberately *not* `#[ignore]`d: it compiles on every PR (so it cannot bit-rot) and
  **loud-skips** when `TEMPORAL_TEST_SERVER` is unset. In CI today, all six skip.
- **AgentCore image gates** — `scripts/agentcore-image-check.sh` builds both container
  images and asserts four acceptance criteria (two image sizes < 30 MB, two
  `docker run`→first-`/ping`-200 cold starts < 50 ms). It runs only by hand, per
  `docs/runbooks/agentcore-image-check.md`.

"Compiles but never executes" is a weaker guarantee than it looks. The Temporal suite's
value is entirely in runtime behaviour — durable replay, retry timing, cancellation
semantics — none of which a compile check touches. The image gates are worth even less
unexecuted: a dependency bump that doubles the binary size or adds a blocking startup path
produces a green PR today.

**Goal:** run both as **signal-only** (non-required) CI jobs, so regressions surface on the
PR that causes them, and so `temporal-it` accumulates the evidence needed to decide whether
it can be promoted to a required check.

### Non-goals

- Making either job a required status check (see §8).
- Cross-run Docker layer caching (`cargo-chef`, `buildx --cache-to type=gha`). See §6.2.
- Changing any test assertion or gate threshold default. Exactly one additive change lands
  in a test file, and it only fires under an env var CI sets (§5.1).

## 2. Verified facts this design rests on

Established by direct check on 2026-08-07, not assumed:

| Fact | How verified | Consequence |
| --- | --- | --- |
| `SMK1085/paigasus-helikon` is **public** | `gh repo view --json visibility` → `PUBLIC` | GitHub's arm64 hosted runners (`ubuntu-24.04-arm`) are free for this repo — native arm64, no qemu |
| `temporalio/cli` publishes versioned linux tarballs + `checksums.txt`; latest is **v1.8.2** | `gh api repos/temporalio/cli/releases/tags/v1.8.2` | The CLI installs from a pinned, checksum-verified first-party artifact |
| `temporal_cli_1.8.2_linux_amd64.tar.gz` SHA-256 is `d8421bda989e6514b4bdb4d63a9012a8a05a806892e881a5aad8510496349a94` | fetched the release's `checksums.txt` | The literal can be pinned in the workflow rather than fetched at run time |
| BuildKit **cache mounts work with the built-in frontend** (no `# syntax=` directive) on Docker 29.6.2 | built a throwaway `RUN --mount=type=cache,…` image — succeeded | No unpinned `docker/dockerfile:1` frontend image enters the build (§6.2) |
| The workspace has **no git dependencies** | `grep -c 'source = "git+' Cargo.lock` → 0 | A cache mount on `$CARGO_HOME/registry` suffices; no `$CARGO_HOME/git` mount needed |
| A skipped `gate()` test **passes silently** | `temporal_live.rs:45-56` — `eprintln!` then `return`; `cargo test` captures a passing test's output | Drives §5.1, the central correctness fix in this revision |
| The repo already has a green-by-skip guard idiom | `HELIKON_REQUIRE_SANDBOX=1` → `panic!` at `crates/paigasus-helikon-tools/tests/os_sandbox.rs:21` and `os_sandbox_seatbelt.rs:19`, set at `ci.yml:67-69` | §5.1 follows an established in-repo pattern, not a new invention |
| `$GITHUB_STEP_SUMMARY` output has precedent, with a local fallback | `scripts/check-doc-coverage.sh:85` — `>> "${GITHUB_STEP_SUMMARY:-/dev/stdout}"` | §6.1's summary reporting reuses the idiom verbatim |
| `ci` and `workflows` are allowed commit/PR-title scopes; `ci` carries `increment: None` | `.versionrc` `types`/`scopeRegex`, `pr-title.yml` `scopes:` | Commits and the PR title pass the `commits` and `pr-title` gates. Note `increment: None` binds **convco only** — §8 explains why it does *not* stop a release-plz bump |

## 3. Where the jobs live — a new `integration.yml`

**This reverses the first-draft decision to put both jobs in `ci.yml`.** That draft argued
the supply-chain workflows are separate "only because they have genuinely independent
triggers and failure semantics, which these two do not." That reasoning was backwards, and
the challenge caught it: these jobs have **both**.

- **Failure semantics differ.** §5.3 and §9 *expect* `crash_resume_mid_tool_call` to flake
  on shared hardware — measuring that rate is the point of this phase. A failing
  non-required job does not block a merge, but it still makes the enclosing **workflow
  run** conclude `failure`. Putting a deliberately-flaky job in `ci.yml` means `ci` goes
  red on `main` for expected flakes, and within weeks "ci is red on main" means nothing.
  That is precisely the mute-instead-of-fix failure mode §6.1 warns about.
- **Triggers differ.** The flake record needs observations that a path filter cannot
  supply (§5.4), which means a nightly `schedule` and a `workflow_dispatch` — triggers
  `ci.yml` does not have and should not grow.

So: **`.github/workflows/integration.yml`**, holding both jobs.

```yaml
on:
  push: { branches: [main] }
  pull_request:
  schedule: [{ cron: "0 5 * * *" }]   # nightly; temporal-it only (§5.4)
  workflow_dispatch:

concurrency:
  group: integration-${{ github.workflow }}-${{ github.ref }}-${{ github.event_name }}  # event_name is load-bearing (push/schedule/workflow_dispatch all resolve ref to refs/heads/main) — see audit.yml/deny.yml
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

permissions:
  contents: read
```

Both mirror `ci.yml`'s concurrency and permissions posture. The ticket's "pattern:
`sessions-it`" is honoured in job *shape* — path filter, service startup, step guards —
which is what the pattern actually consists of; file location is not part of it.

### 3.1 Run-decision mechanics

`dorny/paths-filter` needs a base to diff against, which `schedule` and `workflow_dispatch`
do not have. Both jobs therefore run the filter step **only** on `push`/`pull_request`, then
collapse everything into one output via a `decide` step:

```yaml
- id: filter
  if: github.event_name == 'push' || github.event_name == 'pull_request'
  uses: dorny/paths-filter@<sha>
  with: { filters: ... }
- id: decide
  env:
    EVENT: ${{ github.event_name }}
    MATCHED: ${{ steps.filter.outputs.temporal }}
  run: |
    if [ "${EVENT}" = "workflow_dispatch" ] || [ "${EVENT}" = "schedule" ]; then
      echo "run=true" >> "$GITHUB_OUTPUT"
    else
      echo "run=${MATCHED:-false}" >> "$GITHUB_OUTPUT"
    fi
```

Every subsequent step is guarded by `steps.decide.outputs.run == 'true'`. When the filter
step is skipped its outputs are empty, hence the `:-false` default. Values reach the script
through `env:` rather than `${{ }}` interpolated into the shell body — the repo's own
convention, and correct regardless of the value's provenance.

**Why step-level guards and not a job-level `if:`.** A skipped *job* reports **no status at
all**. Today that is untidy; the moment `temporal-it` is promoted to required (§8, the
explicit plan), a job reporting nothing blocks every PR that does not touch Temporal — the
exact failure recorded in `feedback_verify_required_checks_reported`. Step-level guards mean
the job always reports, green, having run nothing. `sessions-it` (`ci.yml:197-233`) already
does this, and it is required.

**`agentcore-image` opts out of the nightly** — its `decide` step maps `schedule` to
`false`. Measured cold on the first CI run, both images build and all four gates run in
**~4 minutes**, so this is not a cost decision — re-measuring two numbers that sit far under
their budgets every night simply adds no information. `workflow_dispatch` still reaches it.

## 4. Rejected alternatives

**Piggyback on the existing `test (ubuntu-latest, stable)` matrix entry.** That job already
compiles and runs all six `temporal_live` tests as no-ops, so adding the CLI install, dev
server, and `TEMPORAL_TEST_SERVER` would cost ~10 seconds instead of a second cold build of
the workspace's heaviest dependency tree — which is the dominant cost of this entire design.
**Rejected**, but on one specific ground: `test (ubuntu-latest, stable)` is a *required*
check, and this design's own premise is that the crash-resume test is expected to flake. That
turns a known-flaky live test into a merge blocker on day one, inverting the ticket's
signal-first-then-promote sequencing. The cost is real and is accepted knowingly.

**`temporalio/setup-temporal`.** Rejected — but the first draft's reasoning was wrong twice,
and is corrected here rather than quietly kept. It claimed CLAUDE.md's `releases/latest`
recipe "does not apply", yet `ci.yml` SHA-pins `dtolnay/rust-toolchain` in six places with
the comment *"master (no tagged releases)"* — pinning a release-less action is routine here.
It also claimed the action sits "in the trust path of a job that runs PR-authored code",
which is confused: the job compiles and runs the PR's code, including any `build.rs`, so PR
code is already fully trusted inside it. The **actual** reason to prefer the tarball: it is a
first-party artifact whose identity is asserted by a checksum in a reviewed file, and it adds
no new third-party action to the org's dependency surface. That is sufficient; the bad
arguments are retired so this paragraph is not later cited as precedent for rejecting
legitimate actions.

**A digest-pinned `temporalio/temporal` container.** Structurally closer to `sessions-it`,
but adds container networking as a failure mode for a server that runs fine as a host
binary, and the ticket explicitly prescribes the host install.

## 5. Job A — `temporal-it`

```yaml
temporal-it:
  runs-on: ubuntu-latest
  timeout-minutes: 60
```

**60, not 30.** The first draft justified 30 as protection against a hung dev server, but
the same budget must absorb a from-scratch build of `temporalio-sdk-core`/`-client`/
`-common`/`-workflow` plus prost/tonic and `protoc` codegen — the heaviest dependency tree
in the workspace. `Swatinem/rust-cache` will usually **miss**: the job is path-filtered, so
pushes to `main` (the only caches other branches can read) rarely populate it, and
rust-cache's `cache-on-failure` defaults to `false`, so a red run — which this job is
*designed* to sometimes be — never warms it either. The implementing PR itself runs with a
guaranteed cold cache. The job therefore sets `cache-on-failure: true`, and the tight bounds
go where the risk actually is: a `timeout` around the readiness poll, and `timeout-minutes`
on the suite step.

**Path filter** (`temporal`):

```
crates/paigasus-helikon-runtime-temporal/**
crates/paigasus-helikon-core/src/**
crates/paigasus-helikon-core/Cargo.toml
Cargo.toml
Cargo.lock
.github/workflows/integration.yml
```

Core is included because the Temporal runner implements core's `Runner`/`Agent` contracts
and the suite asserts on core's `RunError` variants; `Cargo.lock` catches SDK bumps.

**Steps:** checkout (`persist-credentials: false`) → filter → decide → `dtolnay/rust-toolchain`
@ stable → **`arduino/setup-protoc` with `repo-token: ${{ secrets.GITHUB_TOKEN }}`** (the
SMA-332 lesson the ticket calls out; `temporalio-protos` runs `prost-build` at build time.
The token is not optional dressing — every one of the six existing uses passes it to avoid
unauthenticated API rate limits) → `Swatinem/rust-cache` with `cache-on-failure: true` →
install CLI (§5.2) → start server (§5.3) → run suite (§5.3) → step summary (§5.4) →
diagnostics on failure (§5.5).

### 5.1 The green-by-skip guard — the one test-file change

`gate()` (`temporal_live.rs:45-56`) prints `SKIPPED:` to stderr and returns `None`; the test
then **returns normally and passes**. `cargo test` captures a passing test's output and never
prints it. So a CI job that never reached the server — a typo'd env var, an `env:` on the
wrong step, a dev server that died between the readiness poll and the suite — is
**indistinguishable from a fully green run**, and the first draft's acceptance criterion
("zero `SKIPPED:` lines in its output") was vacuously true either way.

Shipping that would reproduce the exact weakness this ticket exists to remove, with a green
checkmark on top. The repo already solved this class of bug:

```rust
// crates/paigasus-helikon-tools/tests/os_sandbox.rs:21
if std::env::var("HELIKON_REQUIRE_SANDBOX").as_deref() == Ok("1") {
    panic!("HELIKON_REQUIRE_SANDBOX=1 but Landlock could not be established on this host");
}
```

`gate()` gains the identical shape, keyed on **`HELIKON_REQUIRE_TEMPORAL`**, and the job sets
`HELIKON_REQUIRE_TEMPORAL: 1` at job level. Absent the variable, local behaviour is
unchanged — loud skip, test passes. This is additive and fires only under a variable CI sets.

### 5.2 CLI install — pinned tarball

```bash
TEMPORAL_CLI_VERSION=1.8.2
TEMPORAL_CLI_SHA256=d8421bda989e6514b4bdb4d63a9012a8a05a806892e881a5aad8510496349a94
```

`curl -sSfL` the release tarball, verify with `sha256sum -c` against that **literal**, extract
`temporal` onto `PATH`. The checksum is pinned rather than fetched from the release's own
`checksums.txt`, because a checksum served by the same host as the artifact proves only that
the download was not corrupted in transit — it proves nothing about the artifact's identity.
A literal in a reviewed, version-controlled file is a real assertion: changing it takes a commit.

**Maintenance cost, stated plainly:** version and checksum are hand-bumped; Dependabot tracks
neither. Same posture as `sessions-it`'s digest-pinned Postgres and Redis images.

### 5.3 Dev server and suite

```bash
temporal server start-dev --headless --ip 127.0.0.1 --port 7233 \
  > "${RUNNER_TEMP}/temporal-dev-server.log" 2>&1 &
```

`--headless` skips the Web UI. Redirecting both streams matters for two reasons: a background
process holding the step's stdio pipes open can prevent the step from ever completing, and
§5.5 needs something to print. `start-dev` uses an in-memory store — no database setup.

Readiness is polled with a bounded loop (~30 × 2 s) on
**`temporal operator namespace describe default`**, not `operator cluster health`. The cluster
can report healthy before the `default` namespace finishes registering, and the tests connect
with `ClientOptions::new("default")` (`temporal_live.rs:64`) — a namespace-not-found on the
first test would present as a genuine regression. The probe asserts the thing the tests
actually need. Failure to become ready inside the budget dumps the server log and fails loudly.

```bash
TEMPORAL_TEST_SERVER=127.0.0.1:7233 \
  cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1
```

`--test-threads=1` matches the suite's documented invocation. (Each test mints a uuid task
queue, so serialization is belt-and-braces rather than a correctness requirement — but the
documented invocation is what was validated, and CI should not quietly run something else.)

**`127.0.0.1`, not `localhost`** — a deliberate divergence from the ticket, the README, and
the suite's own doc comment, all of which say `localhost:7233`. On a dual-stack runner
`localhost` can resolve to `::1` first while the dev server is bound to IPv4, producing a
connection failure that looks like a Temporal bug. The literal address removes the resolver
from the picture.

**Deliberately no retry loop, diverging from `sessions-it`.** `sessions-it` retries three
times because it is *required* and a flake blocks a merge. `temporal-it` starts signal-only
for the opposite reason: the entire purpose of this phase is to learn how often
`crash_resume_mid_tool_call` — which aborts a real worker against wall-clock activity
timeouts — actually flakes on shared hardware. A retry loop erases exactly the evidence the
promotion decision needs. A retry can be added at promotion time, as an informed decision
rather than a copied default.

### 5.4 Collecting the flake record

The first draft asserted the phase exists to measure a flake rate, then provided no
measurement mechanism and a path filter that could yield zero observations in a quiet
quarter — leaving §8's "evidence-based decision" unreachable. Fixed by:

- the **nightly `schedule`** (§3), which runs the suite regardless of what changed;
- a step writing outcome and wall-clock duration to
  `>> "${GITHUB_STEP_SUMMARY:-/dev/stdout}"`, following `check-doc-coverage.sh:85`;
- a **concrete promotion bar** in §8, so the decision is falsifiable.

Per CLAUDE.md's own note on scheduled workflows: GitHub can delay or drop cron runs under
load and disables them after 60 days of repository inactivity. The nightly is therefore
best-effort evidence — a missing row is not a passing row.

### 5.5 Failure diagnostics

The suite step gets an `id`, and the diagnostics step is guarded by
`if: always() && steps.<id>.outcome == 'failure'`, printing the server log with a tolerant
fallback:

```bash
cat "${RUNNER_TEMP}/temporal-dev-server.log" || echo "(no server log — did install/start fail?)"
```

The first draft used `failure() && steps.filter.outputs.temporal == 'true'` and claimed the
conjunct narrowed *which* step had failed. It does not — it only re-checks the filter. If the
install or server-start step failed, the dump would still run and `cat` on a nonexistent file
would exit 1, adding a second red step to an already-red job. Keying on the suite step's
`outcome` is what the stated intent actually requires.

Three lines, no artifact upload, no retry. Justification: the first question any red run
raises is "did the server even come up?", and a signal-only job that cannot answer it gets
muted rather than fixed.

## 6. Job B — `agentcore-image`

```yaml
agentcore-image:
  runs-on: ubuntu-24.04-arm
  timeout-minutes: 60
```

**Why arm64 is mandatory, not a preference.** The Dockerfile hardcodes
`--platform linux/arm64` because AWS Bedrock AgentCore's runtime targets are arm64 microVMs,
and the runbook explicitly says not to switch it to amd64 for convenience. On an x86 runner
that means qemu-emulating a musl release build that compiles aws-lc-rs's C and assembly —
plausibly an hour or more, twice. GitHub's free-for-public-repos `ubuntu-24.04-arm` makes the
native path available, and it is the only one that makes this job viable.

**Path filter** (`agentcore`):

```
crates/paigasus-helikon-runtime-agentcore/**
scripts/agentcore-image-check.sh
.dockerignore
Cargo.toml
Cargo.lock
.github/workflows/integration.yml
```

`.dockerignore` is included because it directly controls the build context and can break the
build outright (its own header warns a local `target/` "can reach tens of GB"). Root
`Cargo.toml` is included as the natural home for a future `[profile.release]` (`lto`,
`opt-level`, `strip`, `panic`) — the workspace declares none today, so this is a latent gap,
but adding one would move image size immediately and silently.

Notably **absent**: `core`, `runtime-axum`, `runtime-tokio`, `mcp`, `providers-anthropic`.
All are compiled into the images, so a first-party change there *can* move the numbers — but
the measured margins are wide (~11% of the size budget, ~20% of the latency budget) and
`Cargo.lock` still catches dependency bumps. The trade is explicit and accepted for a job this
expensive. Because the workflow self-reference is now `integration.yml` rather than `ci.yml`,
including it no longer triggers an arm64 double-build (measured at ~4 minutes cold) on every
unrelated CI edit.

**Steps:** checkout → filter → decide → free-disk report (§6.3) →
`bash scripts/agentcore-image-check.sh` with `AGENTCORE_COLD_START_LIMIT_MS: "250"` (quoted,
matching `DOC_COVERAGE_THRESHOLD: "80"` at `ci.yml:143`) → step summary.

No Rust toolchain, no `setup-protoc`, no `rust-cache` on the host — every compile happens
inside the Dockerfile's builder stage. (The SMA-332 protoc lesson applies to jobs compiling
the *workspace*; this one does not.)

### 6.1 Script changes

**One override, not two:**

```bash
COLD_START_LIMIT_MS="${AGENTCORE_COLD_START_LIMIT_MS:-50}"
```

The first draft also added `AGENTCORE_SIZE_LIMIT_BYTES`. **Dropped.** It is YAGNI by the
draft's own argument (the size gate is environment-independent and CI never overrides it),
and worse, it hands an env var to the one gate carrying the explicit STOP RULE
(`agentcore-image-check.sh:147-152`) — a rule that exists precisely to stop someone quietly
relaxing the size limit. `SIZE_LIMIT_BYTES` stays a hardcoded constant.

**An override must be loud.** Nothing in the draft made one visible: a reader of the CI log
would see the table and `All gates passed.` (line 168) and reasonably conclude the SMA-332 AC
was enforced. So whenever the value differs from the default, the script prints, above the
table:

```
NOTE: cold-start gate overridden to 250 ms (default 50 ms) — this is NOT the AC value.
```

**Correcting the first draft's inventory of hardcoded strings.** It claimed "the summary table
and every failure message currently hardcode `< 30 MB` and `< 50 ms`". That is wrong for the
cold-start failures: lines 155 and 160 already interpolate `${COLD_START_LIMIT_MS}`. The
literals needing work are the table cells (lines 131-134) and — now that only the cold-start
limit is overridable — just the two `< 50 ms` cells. The draft also missed the prose: the file
header at lines 16, 22-26, 34 ("sub-50ms budget") and the gate declaration comments at 54-58
all state the values as fixed, and become wrong once one is overridable. They get updated too.

**`DOCKER_BUILDKIT=1` is set explicitly in the script**, because §6.2 makes BuildKit a hard
requirement and `DOCKER_BUILDKIT=0` would otherwise turn a working local invocation into an
error.

**Also written to `$GITHUB_STEP_SUMMARY`** (with the `:-/dev/stdout` fallback so local runs
are unaffected): both sizes, both cold starts, and the effective gates — so the CI cold-start
number is *recorded* rather than merely asserted, which is what makes §6.4's revisit possible.

**Not fixed, but noted so the enumeration is honest:** `date +%s%N` (lines 98, 107) is
GNU-only — BSD `date` emits a literal `N` and the arithmetic at line 109 breaks, meaning the
runbook's macOS validation implicitly required coreutils on `PATH`, a prerequisite the runbook
does not list. It works on the Linux runner, so it is not a CI defect and is out of scope
here; the runbook gains a one-line prerequisite note. `HOST_PORT_ECHO=18080` /
`HOST_PORT_AGENT=18081` (lines 51-52) are likewise hardcoded and not overridable; no conflict
is expected on a fresh runner.

### 6.2 Dockerfile — BuildKit cache mounts

```dockerfile
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/workspace/target,sharing=locked \
    cargo build --release --locked -p paigasus-helikon-runtime-agentcore \
    --example "${EXAMPLE}" ${FEATURES:+--features "${FEATURES}"} \
    && strip "target/release/examples/${EXAMPLE}" \
    && cp "target/release/examples/${EXAMPLE}" /agentcore-example
```

`/usr/local/cargo` is `rust:1.94-alpine`'s `CARGO_HOME`; this is asserted at implementation
time rather than assumed.

**The load-bearing invariant, which gets a comment in the Dockerfile:** the `cp` out of
`target/` must stay inside this same `RUN`. A cache mount is not part of the image filesystem,
so splitting the `cp` into a later `RUN` or a `COPY --from` would silently produce an image
with no binary — a quiet failure a future editor will not anticipate, which is why it is
documented in the file and not only here.

**Two claims from the first draft are retracted as false.**

1. *"Output images are bit-identical."* They are not. The image config embeds a `created`
   timestamp, so image IDs differ per build regardless of content, and the workspace
   configures none of the reproducible-build machinery (`SOURCE_DATE_EPOCH`,
   `--remap-path-prefix`; there is no `[profile.release]` at all) that would make even the
   binary bit-identical. The checkable claim replaces it, and is what AC #5 asserts: the
   images still contain `/agentcore-example` and still pass the four gates.
2. *"Local behaviour is byte-identical."* Also false — `RUN --mount=type=cache` makes BuildKit
   a hard requirement, so `DOCKER_BUILDKIT=0 bash scripts/agentcore-image-check.sh` goes from
   working to erroring. §6.1 sets `DOCKER_BUILDKIT=1` in the script so the requirement cannot
   be defeated by a stale environment, and the runbook records it.

**Cache mounts are never garbage-collected.** A musl release target dir for this workspace
accumulates on every local run, forever. The runbook's disk prerequisite currently reads "a
few hundred MB" (lines 42-43) and becomes wrong; it gains the real figure, a
`docker builder prune` note, and a triage line: *if a measurement looks wrong, re-run with
`--no-cache`* — cargo freshness is mtime-based against a `COPY . .`-provided tree.

**What this buys, honestly:** the two builds share a BuildKit daemon within one job, so the
second image reuses the first's compiled dependencies — expect roughly 40–50% off the second
build. It does **not** help the first build and does **not** persist across runs; every job
gets a fresh runner with an empty cache. Cross-run caching would need `cargo-chef` or
`buildx --cache-to type=gha`, both largely defeated by `COPY . .` invalidating on every
commit. This is the cheap half of the win; the expensive half is a non-goal.

### 6.3 Disk and capacity budget

Two musl release builds of the aws-lc-rs/rustls stack share one cache-mounted
`/workspace/target` inside BuildKit's storage, alongside two image builds. A disk-full inside
BuildKit surfaces as an opaque build error that would read as an AgentCore regression, so a
`df -h` step before and after the build records the real figures. This was explicitly a
*measure first* decision.

**Measured, 2026-08-07, `ubuntu-24.04-arm`:** 4 vCPU, 15 GiB RAM (+3 GiB swap), Docker 28.0.4,
and **145 GB of disk with 109 GB free before the build**. Afterwards: 107 GB free — the two
builds cost roughly **2 GB** (Docker reports 1.88 GB images + 1.11 GB build cache).

That settles the question with a very wide margin, and corrects this section's own pre-run
estimate: it guessed "14 GB storage" from GitHub's published spec, which is off by an order of
magnitude for this runner. None of the mitigations drafted here — dropping the
`/workspace/target` mount, `docker builder prune -f` between builds, or freeing the runner's
preinstalled toolchains — is needed, and the cache-mount reuse §6.2 buys is kept in full.

Docker's presence on `ubuntu-24.04-arm` was likewise confirmed by that run rather than assumed.

### 6.4 The 250 ms budget

The only cold-start measurement in the repo is 9–11 ms on Docker Desktop / macOS
(`docs/runbooks/agentcore-image-check.md:17-18`). Nothing has ever been measured on a Linux
arm64 runner, so "roughly 5–10× the expected native-Linux figure" is an estimate, and this
spec says so rather than dressing it as a derivation. 250 ms is chosen to be loose enough that
runner contention does not redden the job and tight enough that a real regression — a blocking
initialisation, a synchronous network call at startup — still trips it by an order of
magnitude.

Because §6.1 writes the measured value to the step summary and the implementing PR records the
first observed CI figure in the runbook next to the macOS row, the constant becomes
revisitable instead of permanent. §8 commits to that revisit.

## 7. Documentation updated in the same PR

| File | Change |
| --- | --- |
| `CLAUDE.md` | New `integration.yml` paragraph in the CI section: both jobs, signal-only status, promotion bar, arm64 rationale, the `HELIKON_REQUIRE_TEMPORAL` guard, and why CI's cold-start budget differs from the AC. |
| `CONTRIBUTING.md` | Line 242 — *"Live tests are not part of CI."* — becomes false and is corrected (it refers to the `--ignored` OpenAI tests, which remain out of CI; the Temporal live suite no longer is). Plus a note on the two non-required integration jobs. |
| `docs/runbooks/agentcore-image-check.md` | The `AGENTCORE_COLD_START_LIMIT_MS` override and its loud NOTE; that CI runs the script with a deliberately looser budget (an instrument difference, not a relaxed AC); the first observed CI figure alongside the macOS row; corrected disk prerequisite + `docker builder prune`; the BuildKit-required and `--no-cache` triage notes; the coreutils-`date` prerequisite. |
| `crates/paigasus-helikon-runtime-temporal/README.md` | Its testing section says the live suite runs locally; note that CI runs it too, and document `HELIKON_REQUIRE_TEMPORAL`. |
| `docs/book/src/concepts/runtimes.md` | One clause: line 52 says the measured cold starts are "well under **the** 50 ms gate". After this change there are two gates (50 ms local/AC, 250 ms CI) and the definite article is wrong. |

CLAUDE.md's *"eight jobs"* sentence needs care: it reads *"runs eight jobs on every PR (the
`commits` job is PR-only; **the other seven** also run on push to `main`)"*. Both jobs move to
a new workflow, so `ci.yml`'s counts are unchanged and only the surrounding CI narrative gains
a paragraph. While in that sentence, a pre-existing drift is cheap to fix in passing: it still
describes `build-no-default-features` as only `cargo build -p paigasus-helikon-runtime-axum
--no-default-features`, but `ci.yml:107-116` now also builds `runtime-actix` and asserts no
axum leakage.

The mdBook is otherwise untouched — a conscious call per CLAUDE.md, not a silent skip. This
change alters no public API, quickstart flow, crate roster entry, or documented concept; the
`runtimes.md` clause above is an accuracy fix, not a content change.

## 8. Deliberate non-changes, promotion, rollback

- **`.github/rulesets/main-protection-checks.json` is untouched.** Both jobs stay
  signal-only. Promotion means adding `temporal-it` to that file *and* to CONTRIBUTING.md's
  required-contexts table.
- **Concrete promotion bar**, so §5.4's evidence is actually decidable: **≥ 20 runs that
  executed the suite (filter hits plus nightlies) with ≤ 1 flake, or 30 consecutive green
  nightlies.** Whichever comes first, reviewed by the maintainer, tracked as a follow-up
  Linear issue opened when this PR merges. Promotion also revisits the retry decision (§5.3).
- **The 250 ms budget is revisited** once ≥ 10 CI observations exist (§6.4).
- **No `continue-on-error` on either job.** It would make them report green unconditionally
  — not a weaker signal but the absence of one. Signal-only is achieved by not listing the
  contexts as required, not by suppressing failure.
- **Release impact.** The PR touches `crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile`
  and `crates/paigasus-helikon-runtime-temporal/{README.md,tests/}`, and release-plz attributes
  bumps by file path. `.versionrc`'s `increment: None` for `ci` governs convco (the `commits`
  CI gate and the local `commit-msg` hook) only — release-plz does not read `.versionrc`; it has
  its own logic, and every crate in this workspace is still 0.x (`runtime-temporal` 0.2.2,
  `runtime-agentcore` 0.2.0, facade 0.4.x), where release-plz's 0.x SemVer mapping is the
  inverse of the ≥1.0 one: **feat → patch, breaking → minor**, everything else → patch. So the
  squashed
  `ci(...)`-titled commit will still give both `paigasus-helikon-runtime-temporal` and
  `paigasus-helikon-runtime-agentcore` a patch bump, cascading a patch bump to the
  `paigasus-helikon` facade via `dependencies_update = true`. Precedent in this repo's history:
  `6ebac2c` (`docs(repo): SMA-424 ...`), which touched only `crates/*/README.md`, was followed
  by `b0671ec` (`chore: release (#94)`), which bumped and published nine crates including
  `paigasus-helikon-core`. This bump is harmless — no library code changed — but it is real and
  must be expected, with its CI watched after merge, not assumed away. The `ci` type is still
  the right PR-title choice: it keeps the change out of the user-facing CHANGELOG sections
  (`hidden: true` in `.versionrc`), which is what that field actually controls.
- **Rollback.** The two halves are independently revertible. `integration.yml` can be deleted
  on its own, leaving the script/Dockerfile improvements in place — that is the first thing to
  revert if the jobs misbehave. The `agentcore-image-check.sh` and Dockerfile changes are
  shared with the local/runbook workflow and would only be reverted for a defect in them
  specifically (e.g. the disk-growth hazard proving unacceptable locally).

## 9. Risks

| Risk | Assessment |
| --- | --- |
| `crash_resume_mid_tool_call` flakes on shared hardware | Known and expected — it aborts a worker against wall-clock activity timeouts. Precisely why the job starts signal-only with no retry: to measure the rate rather than mask it. The suite's own backstops (60 s file waits, 120 s run timeout) are generous. |
| Cold `rust-cache` on most runs | Mitigated by `timeout-minutes: 60` and `cache-on-failure: true`; the first run is cold by definition. |
| Runner disk exhaustion inside BuildKit | Unquantified until the first run; §6.3 measures it and lists three ordered mitigations. |
| `ubuntu-24.04-arm` capacity or queueing | Path-filtered to rarely run; a queue delay costs latency, not correctness. |
| Docker Hub anonymous pull-rate limit | The arm job pulls `rust:1.94-alpine` unauthenticated from a shared runner IP. A rate-limit hit reddens a job whose credibility depends on not going red for unrelated reasons. Fork PRs are rare on a solo-maintained repo, so this is accepted and documented rather than mitigated; if it bites, the fix is a `docker/login-action` step. |
| Temporal CLI pin goes stale | Accepted, documented (§5.2); same posture as `sessions-it`'s pinned image digests. |
| Nightly cron dropped or disabled | CLAUDE.md already documents that GitHub delays cron under load and disables it after 60 days of inactivity. The flake record is best-effort; a missing row is not a passing row (§5.4). |
| Narrow `agentcore` filter misses a dependency-crate regression | Explicit trade in §6. Wide margins plus `Cargo.lock` coverage make it acceptable. |

## 10. Acceptance criteria

1. `.github/workflows/integration.yml` exists with `temporal-it` and `agentcore-image`, both
   step-guarded so they always report a status (green no-op when their filter misses), on
   `push`/`pull_request`/`schedule`/`workflow_dispatch`, with `agentcore-image` skipping
   `schedule`.
2. On a PR touching the Temporal crate, `temporal-it` installs the checksum-verified CLI,
   starts a dev server, and **executes** all six `temporal_live` tests. With
   `HELIKON_REQUIRE_TEMPORAL=1` set, an unreachable server makes the job **fail** rather than
   pass silently — verified by deliberately pointing `TEMPORAL_TEST_SERVER` at a dead port and
   observing a red job.
3. `cargo test -p paigasus-helikon-runtime-temporal --test temporal_live` with no env set still
   loud-skips and passes — the local contract is unchanged.
4. On a PR touching the AgentCore crate, `agentcore-image` builds both arm64 images natively,
   reports all four gates, prints the override NOTE, and writes sizes, cold starts, effective
   gates, and disk figures to the step summary.
5. `bash scripts/agentcore-image-check.sh` with no env set enforces 30 MB / 50 ms, prints no
   override NOTE, and exits as before. (BuildKit is now required; §6.2.)
6. Both images built with the cache-mounted Dockerfile contain `/agentcore-example` and pass
   the four gates.
7. Neither job appears in `.github/rulesets/main-protection-checks.json`.
8. The five docs in §7 are updated on the same branch; `mdbook build docs/book` stays clean.
9. Every CI gate in CLAUDE.md's "Common commands" list passes locally.
