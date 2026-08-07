# SMA-457 — `temporal-it` + `agentcore-image` CI jobs implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run SMA-332's two local-only validation suites — the Temporal live integration tests and the AgentCore image size/cold-start gates — as signal-only CI jobs, so regressions surface on the PR that causes them.

**Architecture:** A new `.github/workflows/integration.yml` holds both jobs, deliberately outside `ci.yml` because `temporal-it` is expected to flake while it earns promotion, and a failing job makes its whole workflow run conclude `failure`. Three supporting edits make the jobs meaningful: a `HELIKON_REQUIRE_TEMPORAL` guard so a skipped Temporal suite cannot pass green, an env-overridable cold-start gate in the AgentCore script (the 50 ms AC was measured on a quiet dev machine; a shared runner is a different instrument), and BuildKit cache mounts so the second image build reuses the first's compiled dependencies.

**Tech Stack:** GitHub Actions, `dorny/paths-filter`, `temporalio/cli` v1.8.2, Docker/BuildKit, bash, Rust.

**Design doc:** `docs/superpowers/specs/2026-08-07-sma-457-ci-integration-jobs-design.md` — read it before starting. This plan implements it; where they disagree, the spec wins.

## Global Constraints

- **Commit format:** `<type>(<scope>): SMA-457 <lowercase message>`. Allowed scopes are in `.versionrc`; this plan uses `ci`, `workflows`, `runtime-temporal`, `runtime-agentcore`, `docs`, `claude`, `contributing`, `readme`, `plan`. A local `commit-msg` hook runs `convco check` and will reject anything else.
- **PR title must be `ci(...)` or `ci(workflows): ...`** — never `feat`/`fix`. `ci` carries `increment: None` in `.versionrc`, so release-plz attributes no version bump. A `feat`/`fix` title would bump `runtime-temporal` and `runtime-agentcore` for a CI-only change.
- **Never `git add -A`** — `.env` and `.claude` are untracked but not gitignored. Stage explicit paths and verify with `git show --stat`.
- **Never move HEAD or switch branches.** This worktree is shared with other sessions.
- **Reuse the action SHAs already pinned in `ci.yml`.** Do not look up newer ones: Dependabot's `github-actions` group bumps all occurrences together, and introducing a second pin for the same action fragments that. The exact pins are listed in Task 4.
- **Run `cargo fmt --all` before committing any hand-edited Rust.** The `pre-commit` hook is a deliberate no-op; `pre-push` catches it, but only at push time (and takes 5+ minutes — background it or use `--no-verify` if the gates already ran).
- **Both jobs stay out of `.github/rulesets/main-protection-checks.json`.** Signal-only means "not listed as required" — never `continue-on-error`.

**Local prerequisites (verified present on this machine 2026-08-07):** `protoc` 35.1, `temporal` CLI, `actionlint`, Docker 29.6.2 on arm64 macOS.

---

## File Structure

| File | Action | Responsibility |
| --- | --- | --- |
| `crates/paigasus-helikon-runtime-temporal/tests/temporal_live.rs` | Modify (`gate()`, lines 43-56) | Turn a silent skip into a hard failure under `HELIKON_REQUIRE_TEMPORAL=1` |
| `crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile` | Modify (builder `RUN`) | BuildKit cache mounts for cross-image dependency reuse |
| `scripts/agentcore-image-check.sh` | Modify | Overridable cold-start gate, loud override NOTE, `DOCKER_BUILDKIT=1`, step-summary output |
| `.github/workflows/integration.yml` | Create | Both signal-only jobs |
| `CLAUDE.md` | Modify (CI section) | Document the new workflow; fix pre-existing `build-no-default-features` drift |
| `CONTRIBUTING.md` | Modify (line 242) | "Live tests are not part of CI" becomes false |
| `docs/runbooks/agentcore-image-check.md` | Modify | Override, BuildKit requirement, disk/prune, CI-observed figure |
| `crates/paigasus-helikon-runtime-temporal/README.md` | Modify ("Live validation") | Document `HELIKON_REQUIRE_TEMPORAL` and CI coverage |
| `docs/book/src/concepts/runtimes.md` | Modify (line 52) | "the 50 ms gate" is ambiguous once two gates exist |

---

## Task 1: Never-green-by-skip guard for the Temporal suite

The single most important change in this plan. `gate()` prints `SKIPPED:` and returns `None`; the test then returns normally and **passes**. `cargo test` captures a passing test's output, so the string is never printed. Without this guard, a `temporal-it` job that never reached a server is indistinguishable from a fully green one — reproducing the exact weakness this ticket exists to remove.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/tests/temporal_live.rs:43-56`

**Interfaces:**
- Consumes: nothing.
- Produces: the env var name **`HELIKON_REQUIRE_TEMPORAL`** (value `"1"`), which Task 4's workflow sets at job level.

**Why there is no new `#[test]` here.** The natural unit test would mutate process env, which is global state shared by every test in the binary running in parallel — a guaranteed flake source. The honest test is the observable behaviour of the suite itself under two different environments, which is what Steps 2/4/5 run. Do not "improve" this into an env-mutating `#[test]`.

- [ ] **Step 1: Capture the current (buggy) behaviour**

Run:
```bash
cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1
```
Expected: `test result: ok. 6 passed` — with **no** `SKIPPED:` text visible anywhere. That silence is the bug: six tests just asserted nothing and reported success.

- [ ] **Step 2: Confirm the failure mode the guard must close**

Run:
```bash
HELIKON_REQUIRE_TEMPORAL=1 cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1
```
Expected (before the fix): `test result: ok. 6 passed` — identical to Step 1. The env var does nothing yet. This is the "failing test": CI would be green while asserting nothing.

- [ ] **Step 3: Add the guard**

Replace the `gate()` function (lines 43-56) with:

```rust
/// Returns the configured Temporal server address (`host:port`), or prints a
/// loud skip message and returns `None`.
///
/// Set `HELIKON_REQUIRE_TEMPORAL=1` to turn that skip into a hard failure. CI
/// sets it (`.github/workflows/integration.yml`) because a skipped test *passes*
/// and `cargo test` captures a passing test's output — so without this, a job
/// that never reached a server is indistinguishable from a green one. Mirrors
/// the `HELIKON_REQUIRE_SANDBOX` guard in `paigasus-helikon-tools`.
fn gate() -> Option<String> {
    match std::env::var("TEMPORAL_TEST_SERVER") {
        Ok(addr) if !addr.is_empty() => Some(addr),
        _ => {
            if std::env::var("HELIKON_REQUIRE_TEMPORAL").as_deref() == Ok("1") {
                panic!(
                    "HELIKON_REQUIRE_TEMPORAL=1 but TEMPORAL_TEST_SERVER is unset or empty — \
                     the live Temporal suite would have skipped silently"
                );
            }
            eprintln!(
                "SKIPPED: set TEMPORAL_TEST_SERVER=<host:port> (e.g. localhost:7233) and start a \
                 dev server (`temporal server start-dev --headless`) to run the live Temporal suite"
            );
            None
        }
    }
}
```

- [ ] **Step 4: Verify the guard fires**

Run:
```bash
HELIKON_REQUIRE_TEMPORAL=1 cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1
```
Expected: `test result: FAILED. 0 passed; 6 failed`, each with `HELIKON_REQUIRE_TEMPORAL=1 but TEMPORAL_TEST_SERVER is unset or empty`.

- [ ] **Step 5: Verify the local contract is unchanged**

Run:
```bash
cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1
```
Expected: `test result: ok. 6 passed`. A developer with no server sees exactly what they saw before.

- [ ] **Step 6: Verify a dead server is also caught (the other negative)**

Run:
```bash
HELIKON_REQUIRE_TEMPORAL=1 TEMPORAL_TEST_SERVER=127.0.0.1:79 \
  cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1
```
Expected: FAILED — `connect()`'s `.expect("connect to the Temporal dev server")` panics. Port 79 is chosen because nothing listens on it. This confirms the *set-but-unreachable* case was already covered by `connect()` and needs no extra guard.

- [ ] **Step 7: Run the full suite against a real local server (the strongest available check)**

The `temporal` CLI is installed on this machine, so the whole suite can genuinely run:
```bash
temporal server start-dev --headless --ip 127.0.0.1 --port 7233 > /tmp/sma457-temporal.log 2>&1 &
until temporal operator namespace describe default --address 127.0.0.1:7233 > /dev/null 2>&1; do sleep 1; done
HELIKON_REQUIRE_TEMPORAL=1 TEMPORAL_TEST_SERVER=127.0.0.1:7233 \
  cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1
```
Expected: `test result: ok. 6 passed`. **Record the wall-clock duration** — Task 7 needs it to sanity-check the CI `timeout-minutes`. Then stop the server: `pkill -f 'temporal server start-dev'`.

This also pre-validates the exact readiness probe Task 4's workflow uses.

- [ ] **Step 8: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-temporal --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-temporal/tests/temporal_live.rs
git commit -m "test(runtime-temporal): SMA-457 fail loudly when the live suite would skip in CI"
git show --stat HEAD
```
Expected: exactly one file changed.

---

## Task 2: BuildKit cache mounts in the AgentCore Dockerfile

**Files:**
- Modify: `crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile` (the builder-stage `RUN cargo build ...`)

**Interfaces:**
- Consumes: nothing.
- Produces: a Dockerfile that **requires BuildKit**. Task 3 exports `DOCKER_BUILDKIT=1` in the script to guarantee it.

**Verified facts:** `CARGO_HOME=/usr/local/cargo` in `rust:1.94-alpine` (checked via `docker run --rm rust:1.94-alpine sh -c 'echo $CARGO_HOME'`). Cache mounts work with the built-in frontend on Docker 29.6.2 — no `# syntax=docker/dockerfile:1` directive, so no unpinned frontend image enters the build.

- [ ] **Step 1: Record the baseline**

```bash
time docker build --platform linux/arm64 \
  -f crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile \
  --build-arg EXAMPLE=echo_http -t helikon-agentcore-echo-baseline .
docker image inspect helikon-agentcore-echo-baseline --format '{{.Size}}'
```
Note the size and elapsed time. Expected size ≈ 1.31 MB (1,374,000-ish bytes).

- [ ] **Step 2: Add the cache mounts**

Replace the builder-stage `RUN cargo build ...` block with:

```dockerfile
# BuildKit cache mounts (SMA-457). Both the CI job and the check script build two
# images back to back; these let the second build reuse the first's downloaded
# crates and compiled dependencies — roughly 40-50% off the second build. They do
# NOT help the first build and do NOT persist across CI runs (every job gets a
# fresh runner with an empty cache).
#
# /usr/local/cargo is this base image's CARGO_HOME. The workspace has no git
# dependencies, so a mount on `registry` alone covers the download cache.
#
# Requires BuildKit — the default since Docker 23, and
# scripts/agentcore-image-check.sh exports DOCKER_BUILDKIT=1 so a stale
# DOCKER_BUILDKIT=0 in the environment cannot select the legacy builder and fail
# on this line.
#
# LOAD-BEARING: the `cp` below MUST stay inside this same RUN. A cache mount is
# not part of the image filesystem, so moving the copy into a later RUN (or a
# COPY --from) would silently produce a final image with no binary in it.
#
# Cache mounts are never garbage-collected; see docs/runbooks/agentcore-image-check.md
# for the `docker builder prune` note if local disk use grows.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/workspace/target,sharing=locked \
    cargo build --release --locked -p paigasus-helikon-runtime-agentcore \
    --example "${EXAMPLE}" ${FEATURES:+--features "${FEATURES}"} \
    && strip "target/release/examples/${EXAMPLE}" \
    && cp "target/release/examples/${EXAMPLE}" /agentcore-example
```

- [ ] **Step 3: Rebuild and prove the image still contains the binary**

```bash
docker build --platform linux/arm64 \
  -f crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile \
  --build-arg EXAMPLE=echo_http -t helikon-agentcore-echo .
docker create --name sma457-tmp helikon-agentcore-echo
docker export sma457-tmp | tar -t | grep -E '^agentcore-example$'
docker rm sma457-tmp
docker image inspect helikon-agentcore-echo --format '{{.Size}}'
```
Expected: `agentcore-example` printed by the grep (this is the check that catches the LOAD-BEARING failure above — a `scratch` image has no shell, so `docker run ls` is not available), and a size within a few KB of Step 1's.

- [ ] **Step 4: Prove the second build reuses the first's work**

```bash
time docker build --platform linux/arm64 \
  -f crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile \
  --build-arg EXAMPLE=agent_http --build-arg FEATURES=example-anthropic \
  -t helikon-agentcore-agent .
```
Expected: noticeably faster than a cold build of the same image, because the shared dependencies come from the cache mount. Record the elapsed time; Task 7 reports it.

- [ ] **Step 5: Clean up the baseline image and commit**

```bash
docker image rm helikon-agentcore-echo-baseline
git add crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile
git commit -m "build(runtime-agentcore): SMA-457 cache cargo registry and target across image builds"
git show --stat HEAD
```

---

## Task 3: Overridable cold-start gate + step summary in the check script

**Files:**
- Modify: `scripts/agentcore-image-check.sh`

**Interfaces:**
- Consumes: Task 2's BuildKit-requiring Dockerfile.
- Produces: the env var **`AGENTCORE_COLD_START_LIMIT_MS`** (Task 5's workflow sets `"250"`). No size override exists, by design.

**Design constraint, do not "simplify" it away:** `SIZE_LIMIT_BYTES` stays a hardcoded constant. It carries the STOP RULE at lines 147-152, and an env knob on it would be exactly the quiet relaxation that rule exists to prevent.

- [ ] **Step 1: Require BuildKit explicitly**

Immediately after `set -euo pipefail` (line 41), insert:

```bash
# The Dockerfile's builder stage uses `RUN --mount=type=cache`, which requires
# BuildKit. It is the default since Docker 23, but export it so a stale
# DOCKER_BUILDKIT=0 in the caller's environment cannot select the legacy builder
# and fail the build on a confusing syntax error (SMA-457).
export DOCKER_BUILDKIT=1
```

- [ ] **Step 2: Make the cold-start limit overridable**

Replace the `SIZE_LIMIT_BYTES` / `COLD_START_LIMIT_MS` block (lines 54-58) with:

```bash
# The AC gate (spec §6.4 / task brief): both the model-backed image and the echo
# image must stay under this many bytes — see the module doc comment above.
# Deliberately NOT overridable: this gate carries the STOP RULE below, and an env
# knob on it would be precisely the quiet relaxation that rule exists to prevent.
SIZE_LIMIT_BYTES=$((30 * 1024 * 1024))
# Cold-start gate: exec (docker run) → first `/ping` 200, in milliseconds.
# Overridable via AGENTCORE_COLD_START_LIMIT_MS, because unlike image size this
# measures the host as much as the image: the 50 ms AC was measured on a quiet
# developer machine, and CI runs on a shared runner where that budget is
# instrument noise rather than a regression signal. Any override prints a loud
# NOTE below so no reader mistakes the effective gate for the AC (SMA-457).
COLD_START_LIMIT_MS_DEFAULT=50
COLD_START_LIMIT_MS="${AGENTCORE_COLD_START_LIMIT_MS:-${COLD_START_LIMIT_MS_DEFAULT}}"
```

- [ ] **Step 3: Correct the header comments that state the limits as fixed**

In the module doc comment, replace this paragraph (lines 22-26):

```
# All four measured metrics are AC-gated (see the SIZE_LIMIT_BYTES/
# COLD_START_LIMIT_MS checks below): both images' size and both images'
# exec->/ping cold start. The echo image also demonstrates the framework's own
# minimal-overhead footprint, but that is in addition to — not instead of —
# being checked against the same gates as the agent image.
```

with:

```
# All four measured metrics are gated (see the SIZE_LIMIT_BYTES/
# COLD_START_LIMIT_MS checks below): both images' size and both images'
# exec->/ping cold start. The echo image also demonstrates the framework's own
# minimal-overhead footprint, but that is in addition to — not instead of —
# being checked against the same gates as the agent image.
#
# The two size gates are always the AC value. The cold-start gate defaults to the
# AC value but can be raised via AGENTCORE_COLD_START_LIMIT_MS for environments
# whose container-start overhead is not comparable to a quiet developer machine
# (CI uses 250 ms); an override prints a loud NOTE above the summary table.
```

And on line 34, replace `which would swamp the sub-50ms budget with measurement noise` with `which would swamp the sub-50ms default budget with measurement noise`.

- [ ] **Step 4: Print the override NOTE and make the cold-start table cells dynamic**

Replace the summary block (lines 127-134) with:

```bash
echo
if [[ "${COLD_START_LIMIT_MS}" != "${COLD_START_LIMIT_MS_DEFAULT}" ]]; then
  echo "NOTE: cold-start gate overridden to ${COLD_START_LIMIT_MS} ms (default ${COLD_START_LIMIT_MS_DEFAULT} ms) — this is NOT the AC value."
  echo
fi
echo "== Summary =="
printf '| %-32s | %14s | %10s |\n' "Metric" "Value" "Gate"
printf '| %-32s | %14s | %10s |\n' "--------------------------------" "--------------" "----------"
printf '| %-32s | %11s MB | %10s |\n' "echo image size (AC gate)" "$(to_mb "${echo_size}")" "< 30 MB"
printf '| %-32s | %11s MB | %10s |\n' "agent image size (AC gate)" "$(to_mb "${agent_size}")" "< 30 MB"
printf '| %-32s | %12s ms | %10s |\n' "echo exec->200 (gate)" "${echo_cold_start_ms}" "< ${COLD_START_LIMIT_MS} ms"
printf '| %-32s | %12s ms | %10s |\n' "agent exec->200 (gate)" "${agent_cold_start_ms}" "< ${COLD_START_LIMIT_MS} ms"
```

The cold-start row labels drop "AC" because the printed gate is no longer necessarily the AC value; the size rows keep it because theirs always is.

- [ ] **Step 5: Emit a job summary**

Directly after the `echo "agent image app-side log: ..."` line and its trailing `echo` (around line 137-138), and **before** `failed=0`, insert:

```bash
# Mirrors scripts/check-doc-coverage.sh:85 — appends to the GitHub job summary in
# CI, and to stdout when run locally. Emitted before the gate checks below so a
# failing run still records its numbers.
{
  echo "## AgentCore image gates"
  echo
  if [[ "${COLD_START_LIMIT_MS}" != "${COLD_START_LIMIT_MS_DEFAULT}" ]]; then
    echo "> **NOTE:** cold-start gate overridden to ${COLD_START_LIMIT_MS} ms (default ${COLD_START_LIMIT_MS_DEFAULT} ms) — this is NOT the AC value."
    echo
  fi
  echo "| Metric | Value | Gate |"
  echo "| --- | ---: | --- |"
  echo "| echo image size | $(to_mb "${echo_size}") MB | < 30 MB (AC) |"
  echo "| agent image size | $(to_mb "${agent_size}") MB | < 30 MB (AC) |"
  echo "| echo exec→200 | ${echo_cold_start_ms} ms | < ${COLD_START_LIMIT_MS} ms |"
  echo "| agent exec→200 | ${agent_cold_start_ms} ms | < ${COLD_START_LIMIT_MS} ms |"
} >> "${GITHUB_STEP_SUMMARY:-/dev/stdout}"
```

Placement matters: after `to_mb()` is defined (line 123) and before the `exit 1` path.

- [ ] **Step 6: Verify the default path is unchanged**

```bash
bash scripts/agentcore-image-check.sh
```
Expected: **no** `NOTE:` line; table shows `< 30 MB` and `< 50 ms`; ends `All gates passed.`; exit 0. Fast, because Task 2's cache mount and the image layers are warm. **Record the four measured numbers** — Task 7 puts them in the runbook.

- [ ] **Step 7: Verify the override path**

```bash
AGENTCORE_COLD_START_LIMIT_MS=250 bash scripts/agentcore-image-check.sh
```
Expected: the `NOTE: cold-start gate overridden to 250 ms (default 50 ms) — this is NOT the AC value.` line above the table; cold-start rows read `< 250 ms`; size rows still `< 30 MB`; exit 0.

- [ ] **Step 8: Verify an override cannot relax the size gate**

```bash
AGENTCORE_SIZE_LIMIT_BYTES=1 bash scripts/agentcore-image-check.sh
```
Expected: exit 0, gates unchanged — the variable is inert. This is the assertion that the STOP RULE has no env escape hatch.

- [ ] **Step 9: Verify the gate can still fail**

```bash
AGENTCORE_COLD_START_LIMIT_MS=1 bash scripts/agentcore-image-check.sh; echo "exit=$?"
```
Expected: `FAILED: ... >= the 1ms gate.` on stderr and `exit=1`. Without this, Steps 6-7 only prove the script prints things, not that it still gates.

- [ ] **Step 10: Shellcheck and commit**

```bash
shellcheck scripts/agentcore-image-check.sh || echo "(shellcheck not installed — skip)"
git add scripts/agentcore-image-check.sh
git commit -m "ci(runtime-agentcore): SMA-457 allow a looser cold-start gate and report to the job summary"
git show --stat HEAD
```

---

## Task 4: `integration.yml` scaffold + the `temporal-it` job

**Files:**
- Create: `.github/workflows/integration.yml`

**Interfaces:**
- Consumes: `HELIKON_REQUIRE_TEMPORAL` from Task 1.
- Produces: the workflow file Task 5 appends its second job to, and the check-run context name **`temporal-it`** (referenced by the promotion note in Task 6's docs).

**Action pins — copy these exactly from `ci.yml`, do not look up newer ones:**

| Action | SHA | Comment to keep above the `uses:` |
| --- | --- | --- |
| `actions/checkout` | `9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0` | `# actions/checkout v6.0.2` |
| `dorny/paths-filter` | `7b450fff21473bca461d4b92ce414b9d0420d706` | `# dorny/paths-filter v4.0.1` |
| `dtolnay/rust-toolchain` | `2c7215f132e9ebf062739d9130488b56d53c060c` | `# dtolnay/rust-toolchain master (no tagged releases)` |
| `arduino/setup-protoc` | `c65c819552d16ad3c9b72d9dfd5ba5237b9c906b` | `# arduino/setup-protoc v3.0.0` |
| `Swatinem/rust-cache` | `c19371144df3bb44fab255c43d04cbc2ab54d1c4` | `# Swatinem/rust-cache v2.9.1` |

- [ ] **Step 1: Create the file with the scaffold and the `temporal-it` job**

```yaml
name: integration

# Signal-only integration jobs (SMA-457): the live Temporal suite and the
# AgentCore image gates, both shipped local-only by SMA-332.
#
# Deliberately NOT in ci.yml. `temporal-it` is *expected* to flake while it earns
# promotion to a required check — crash_resume_mid_tool_call aborts a real worker
# against wall-clock activity timeouts — and a failing job makes its whole
# workflow run conclude `failure`, required or not. Keeping these out of ci.yml is
# what stops "ci is red on main" from becoming meaningless. They also need
# triggers ci.yml does not have: the nightly cron is what accumulates the flake
# record the promotion decision depends on.
#
# Neither job is in .github/rulesets/main-protection-checks.json. "Signal-only"
# means *not listed as required* — never `continue-on-error`, which would report
# green unconditionally and remove the signal rather than weaken it.

on:
  push:
    branches: [main]
  pull_request:
  # Nightly, so the flake record accumulates even in weeks when nothing Temporal
  # changes. Only temporal-it runs on this event — see each job's `decide` step.
  # Per CLAUDE.md: GitHub delays cron under load and disables it after 60 days of
  # repository inactivity, so this is best-effort evidence — a missing run is not
  # a passing run.
  schedule:
    - cron: "0 5 * * *"
  workflow_dispatch:

concurrency:
  # Keyed on github.event_name as well as github.ref — load-bearing, not
  # decoration, and the same rule CLAUDE.md records for audit.yml/deny.yml.
  # push-to-main, schedule, and workflow_dispatch all resolve github.ref to
  # refs/heads/main, so a shared group with cancel-in-progress: false would let a
  # queued nightly sit pending until the next merge cancels it — silently
  # discarding the flake record this workflow exists to accumulate. Do not
  # simplify the key back.
  group: integration-${{ github.workflow }}-${{ github.ref }}-${{ github.event_name }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

permissions:
  contents: read

env:
  CARGO_TERM_COLOR: always

jobs:
  temporal-it:
    runs-on: ubuntu-latest
    # Must absorb a cold build of temporalio-sdk-core + prost/tonic + protoc
    # codegen — the heaviest dependency tree in the workspace. rust-cache usually
    # misses here: the job is path-filtered, so pushes to main rarely populate the
    # cache other branches read from.
    timeout-minutes: 60
    env:
      # Never green-by-skip: makes gate() panic instead of quietly passing when
      # TEMPORAL_TEST_SERVER is missing. A skipped test *passes* and cargo test
      # captures a passing test's output, so without this a job that never
      # reached a server is indistinguishable from a green one. Mirrors
      # HELIKON_REQUIRE_SANDBOX in ci.yml's `test` job.
      HELIKON_REQUIRE_TEMPORAL: "1"
      # Pinned literals, not fetched from the release's own checksums.txt: a
      # checksum served by the same host as the artifact proves only that the
      # download was not corrupted, never the artifact's identity. Hand-bumped;
      # Dependabot does not track these.
      TEMPORAL_CLI_VERSION: "1.8.2"
      TEMPORAL_CLI_SHA256: "d8421bda989e6514b4bdb4d63a9012a8a05a806892e881a5aad8510496349a94"
    steps:
      # actions/checkout v6.0.2
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
        with:
          persist-credentials: false
      # dorny/paths-filter v4.0.1 — needs a base to diff against, which schedule
      # and workflow_dispatch do not provide; hence the event guard here and the
      # `decide` step below.
      - if: github.event_name == 'push' || github.event_name == 'pull_request'
        uses: dorny/paths-filter@7b450fff21473bca461d4b92ce414b9d0420d706
        id: filter
        with:
          filters: |
            temporal:
              - 'crates/paigasus-helikon-runtime-temporal/**'
              - 'crates/paigasus-helikon-core/src/**'
              - 'crates/paigasus-helikon-core/Cargo.toml'
              - 'Cargo.toml'
              - 'Cargo.lock'
              - '.github/workflows/integration.yml'
      - id: decide
        env:
          EVENT: ${{ github.event_name }}
          MATCHED: ${{ steps.filter.outputs.temporal }}
        run: |
          # When the filter step is skipped its outputs are empty, hence :-false.
          if [ "${EVENT}" = "workflow_dispatch" ] || [ "${EVENT}" = "schedule" ]; then
            echo "run=true" >> "$GITHUB_OUTPUT"
          else
            echo "run=${MATCHED:-false}" >> "$GITHUB_OUTPUT"
          fi
      - if: steps.decide.outputs.run != 'true'
        run: echo "No Temporal-related changes; nothing to run."
      # Every remaining step is guarded at STEP level, not with a job-level `if:`.
      # A skipped *job* reports no status at all, which would block every PR the
      # moment this context is promoted to required. sessions-it does the same.
      - if: steps.decide.outputs.run == 'true'
        # dtolnay/rust-toolchain master (no tagged releases)
        uses: dtolnay/rust-toolchain@2c7215f132e9ebf062739d9130488b56d53c060c
        with:
          toolchain: stable
      # arduino/setup-protoc v3.0.0 — temporalio-protos compiles .proto at build
      # time (prost-build); a system protoc is required (SMA-332). repo-token
      # avoids unauthenticated GitHub API rate limits.
      - if: steps.decide.outputs.run == 'true'
        uses: arduino/setup-protoc@c65c819552d16ad3c9b72d9dfd5ba5237b9c906b
        with:
          repo-token: ${{ secrets.GITHUB_TOKEN }}
      # Swatinem/rust-cache v2.9.1 — cache-on-failure because this job is expected
      # to go red sometimes, and a red run must still warm the cache for the next.
      - if: steps.decide.outputs.run == 'true'
        uses: Swatinem/rust-cache@c19371144df3bb44fab255c43d04cbc2ab54d1c4
        with:
          cache-on-failure: "true"
      - if: steps.decide.outputs.run == 'true'
        name: Install the Temporal CLI
        run: |
          tarball="temporal_cli_${TEMPORAL_CLI_VERSION}_linux_amd64.tar.gz"
          url="https://github.com/temporalio/cli/releases/download/v${TEMPORAL_CLI_VERSION}/${tarball}"
          curl -sSfL -o "${RUNNER_TEMP}/${tarball}" "${url}"
          echo "${TEMPORAL_CLI_SHA256}  ${RUNNER_TEMP}/${tarball}" | sha256sum -c -
          tar -xzf "${RUNNER_TEMP}/${tarball}" -C "${RUNNER_TEMP}" temporal
          sudo install -m 0755 "${RUNNER_TEMP}/temporal" /usr/local/bin/temporal
          temporal --version
      - if: steps.decide.outputs.run == 'true'
        name: Start the Temporal dev server
        run: |
          # Redirect BOTH streams to a file: a background process holding the
          # step's stdio pipes open can stop the step from ever completing, and
          # the diagnostics step below needs a log to print. The process survives
          # into later steps — the runner does not reap it until the job ends.
          temporal server start-dev --headless --ip 127.0.0.1 --port 7233 \
            > "${RUNNER_TEMP}/temporal-dev-server.log" 2>&1 &
          # Probe the `default` namespace, not `operator cluster health`: the
          # cluster reports healthy before `default` finishes registering, and the
          # suite connects with ClientOptions::new("default"). A
          # namespace-not-found in the first test would look like a real regression.
          for i in $(seq 1 30); do
            if temporal operator namespace describe default --address 127.0.0.1:7233 > /dev/null 2>&1; then
              echo "dev server ready after $((i * 2))s"
              exit 0
            fi
            sleep 2
          done
          echo "::error::Temporal dev server did not become ready within 60s"
          cat "${RUNNER_TEMP}/temporal-dev-server.log" || true
          exit 1
      - if: steps.decide.outputs.run == 'true'
        id: suite
        name: Run the live Temporal suite
        timeout-minutes: 20
        env:
          # 127.0.0.1, not localhost: on a dual-stack runner `localhost` can
          # resolve to ::1 first while the dev server is bound to IPv4, producing
          # a connection failure that looks like a Temporal bug.
          TEMPORAL_TEST_SERVER: 127.0.0.1:7233
        run: |
          # Deliberately a SINGLE attempt, unlike sessions-it's 3x retry loop.
          # sessions-it is required, so a flake there blocks a merge; this job is
          # signal-only precisely to measure how often the wall-clock-sensitive
          # crash-resume test flakes. A retry loop would erase that evidence.
          start=$(date +%s)
          set +e
          cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1
          status=$?
          set -e
          echo "duration_s=$(( $(date +%s) - start ))" >> "$GITHUB_OUTPUT"
          exit "${status}"
      - if: steps.decide.outputs.run == 'true'
        name: Self-test the never-green-by-skip guard
        # Regression test for gate()'s HELIKON_REQUIRE_TEMPORAL branch. The job's
        # HELIKON_REQUIRE_TEMPORAL=1 is inherited from job env; TEMPORAL_TEST_SERVER
        # is scoped to the suite step above, so it is absent here — exactly the
        # condition the guard exists to catch. The suite MUST fail. If someone
        # deletes those five lines, this step is what notices; without it the job
        # would quietly return to passing while asserting nothing. The test binary
        # is already compiled, so this costs seconds.
        run: |
          if cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1; then
            echo "::error::HELIKON_REQUIRE_TEMPORAL=1 with no TEMPORAL_TEST_SERVER must FAIL, but the suite passed — the never-green-by-skip guard in gate() is gone."
            exit 1
          fi
          echo "guard OK: the suite fails when TEMPORAL_TEST_SERVER is absent"
      - if: always() && steps.suite.outcome != 'skipped'
        name: Record the run in the job summary
        env:
          EVENT: ${{ github.event_name }}
          OUTCOME: ${{ steps.suite.outcome }}
          DURATION: ${{ steps.suite.outputs.duration_s }}
        run: |
          # This is the flake record the promotion decision depends on.
          {
            echo "## temporal-it"
            echo
            echo "- event: \`${EVENT}\`"
            echo "- outcome: **${OUTCOME}**"
            echo "- suite wall-clock: ${DURATION}s"
          } >> "${GITHUB_STEP_SUMMARY:-/dev/stdout}"
      - if: always() && steps.suite.outcome == 'failure'
        name: Dump the dev-server log
        # Keyed on the suite step's own outcome, not a bare failure(): the first
        # question a red run raises is "did the server even come up?", and a
        # signal-only job that cannot answer it gets muted rather than fixed.
        run: cat "${RUNNER_TEMP}/temporal-dev-server.log" || echo "(no server log — did install/start fail?)"
```

- [ ] **Step 2: Lint the workflow**

```bash
actionlint .github/workflows/integration.yml
```
Expected: no output (clean). Fix anything it reports before continuing — `actionlint` catches expression-syntax and shell errors that would otherwise cost a full push/CI round trip.

- [ ] **Step 3: Verify the YAML parses and the trigger/job shape is right**

```bash
python3 -c "
import yaml, sys
d = yaml.safe_load(open('.github/workflows/integration.yml'))
on = d.get(True) or d.get('on')
print('triggers:', sorted(on))
print('jobs:', list(d['jobs']))
print('runs-on:', d['jobs']['temporal-it']['runs-on'])
print('guarded steps:', sum(1 for s in d['jobs']['temporal-it']['steps'] if 'if' in s))
"
```
Expected: `triggers: ['pull_request', 'push', 'schedule', 'workflow_dispatch']`, `jobs: ['temporal-it']`, `runs-on: ubuntu-latest`, and a guarded-step count of **11** out of 13 total steps. The two unguarded steps are `actions/checkout` (the filter needs a tree to diff) and `decide` (it must always run to produce its output).

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/integration.yml
git commit -m "ci(workflows): SMA-457 add a signal-only temporal-it integration job"
git show --stat HEAD
```

---

## Task 5: The `agentcore-image` job

**Files:**
- Modify: `.github/workflows/integration.yml` (append a second job)

**Interfaces:**
- Consumes: `AGENTCORE_COLD_START_LIMIT_MS` from Task 3; the BuildKit-requiring Dockerfile from Task 2.
- Produces: the check-run context name **`agentcore-image`**.

- [ ] **Step 1: Append the job**

Add under `jobs:`, after `temporal-it`:

```yaml
  agentcore-image:
    # Native arm64 is mandatory, not a preference: the Dockerfile hardcodes
    # --platform linux/arm64 because AgentCore's runtime targets are arm64
    # microVMs. On an x86 runner this would be a qemu-emulated musl build of
    # aws-lc-rs's C and assembly — plausibly an hour-plus, twice. GitHub's arm64
    # runners are free for public repos, which is what makes this job viable.
    runs-on: ubuntu-24.04-arm
    timeout-minutes: 60
    steps:
      # actions/checkout v6.0.2
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
        with:
          persist-credentials: false
      # dorny/paths-filter v4.0.1
      - if: github.event_name == 'push' || github.event_name == 'pull_request'
        uses: dorny/paths-filter@7b450fff21473bca461d4b92ce414b9d0420d706
        id: filter
        with:
          # Deliberately narrow. core/runtime-axum/runtime-tokio/mcp/
          # providers-anthropic are all compiled into the images, so a first-party
          # change there *can* move the numbers — but the measured margins are
          # wide (~11% of the size budget, ~20% of the latency budget) and
          # Cargo.lock still catches dependency bumps. Explicit trade for a job
          # this expensive. .dockerignore controls the build context and can break
          # the build outright; root Cargo.toml is the natural home for a future
          # [profile.release], which would move image size immediately.
          filters: |
            agentcore:
              - 'crates/paigasus-helikon-runtime-agentcore/**'
              - 'scripts/agentcore-image-check.sh'
              - '.dockerignore'
              - 'Cargo.toml'
              - 'Cargo.lock'
              - '.github/workflows/integration.yml'
      - id: decide
        env:
          EVENT: ${{ github.event_name }}
          MATCHED: ${{ steps.filter.outputs.agentcore }}
        run: |
          # No nightly: a 40-60 minute arm64 double-build every night to
          # re-measure two numbers sitting at ~11% and ~20% of their budgets is
          # not worth it. workflow_dispatch still reaches this job.
          if [ "${EVENT}" = "schedule" ]; then
            echo "run=false" >> "$GITHUB_OUTPUT"
          elif [ "${EVENT}" = "workflow_dispatch" ]; then
            echo "run=true" >> "$GITHUB_OUTPUT"
          else
            echo "run=${MATCHED:-false}" >> "$GITHUB_OUTPUT"
          fi
      - if: steps.decide.outputs.run != 'true'
        run: echo "No AgentCore image-related changes; nothing to run."
      - if: steps.decide.outputs.run == 'true'
        name: Report runner capacity
        run: |
          # Two musl release builds share one cache-mounted /workspace/target
          # inside BuildKit's storage. A disk-full there surfaces as an opaque
          # build error that reads like an AgentCore regression, so measure first.
          {
            echo "## agentcore-image runner"
            echo
            echo '```'
            nproc
            free -h
            df -h /
            docker version --format 'docker {{.Server.Version}}'
            echo '```'
          } >> "${GITHUB_STEP_SUMMARY:-/dev/stdout}"
      - if: steps.decide.outputs.run == 'true'
        name: Build both images and check the gates
        env:
          # The 50 ms AC was measured on a quiet developer machine; a shared
          # runner is a different instrument. ~5-10x the expected native-Linux
          # figure: loose enough that contention does not redden the job, tight
          # enough that a blocking-init regression still trips it by an order of
          # magnitude. The script prints a loud NOTE whenever this differs from
          # the default. The 30 MB size gate is NOT overridable.
          AGENTCORE_COLD_START_LIMIT_MS: "250"
        run: bash scripts/agentcore-image-check.sh
      - if: always() && steps.decide.outputs.run == 'true'
        name: Report disk after the build
        run: |
          {
            echo
            echo "Disk after build:"
            echo '```'
            df -h /
            docker system df
            echo '```'
          } >> "${GITHUB_STEP_SUMMARY:-/dev/stdout}"
```

- [ ] **Step 2: Lint and verify shape**

```bash
actionlint .github/workflows/integration.yml
python3 -c "
import yaml
d = yaml.safe_load(open('.github/workflows/integration.yml'))
j = d['jobs']
print('jobs:', list(j))
print('agentcore runs-on:', j['agentcore-image']['runs-on'])
print('cold-start env:', [s.get('env',{}).get('AGENTCORE_COLD_START_LIMIT_MS') for s in j['agentcore-image']['steps'] if s.get('env',{}).get('AGENTCORE_COLD_START_LIMIT_MS')])
"
```
Expected: clean actionlint; `jobs: ['temporal-it', 'agentcore-image']`; `runs-on: ubuntu-24.04-arm`; `cold-start env: ['250']`.

- [ ] **Step 3: Confirm neither job is a required check**

```bash
grep -E 'temporal-it|agentcore-image' .github/rulesets/main-protection-checks.json || echo "OK: neither job is required"
```
Expected: `OK: neither job is required`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/integration.yml
git commit -m "ci(workflows): SMA-457 add a signal-only agentcore-image job on arm64"
git show --stat HEAD
```

---

## Task 6: Documentation

**Files:**
- Modify: `CLAUDE.md` (CI section, around line 97)
- Modify: `CONTRIBUTING.md:242`
- Modify: `docs/runbooks/agentcore-image-check.md`
- Modify: `crates/paigasus-helikon-runtime-temporal/README.md` ("Live validation")
- Modify: `docs/book/src/concepts/runtimes.md:52`

**Interfaces:**
- Consumes: everything from Tasks 1-5. The measured numbers from Task 3 Step 6 and Task 2 Step 4.

- [ ] **Step 1: Fix the pre-existing `build-no-default-features` drift in `CLAUDE.md`**

In the line-97 paragraph, replace:
```
`build-no-default-features` (SMA-452: `cargo build -p paigasus-helikon-runtime-axum --no-default-features`, catching `openapi`-feature-gating regressions)
```
with:
```
`build-no-default-features` (SMA-452: `cargo build --no-default-features` for both `paigasus-helikon-runtime-axum` and `paigasus-helikon-runtime-actix`, catching `openapi`-feature-gating regressions, plus a `cargo tree` assertion that axum does not leak into the `runtime-actix` feature graph)
```

Leave the "eight jobs / the other seven" counts alone — both new jobs live in a different workflow, so `ci.yml`'s counts are unchanged.

- [ ] **Step 2: Add the `integration.yml` documentation to `CLAUDE.md`**

Insert after the `ci.yml` paragraph and before the `.github/workflows/pr-title.yml` paragraph:

```markdown
`.github/workflows/integration.yml` (SMA-457) runs two **signal-only** jobs — `temporal-it` and `agentcore-image` — on PR, push to `main`, a nightly cron at 05:00 UTC, and `workflow_dispatch`. Deliberately **not** in `ci.yml`: `temporal-it` is *expected* to flake while it earns promotion (its crash-resume test aborts a real worker against wall-clock activity timeouts), and a failing job makes its whole workflow run conclude `failure` whether or not it is required — so keeping these out of `ci.yml` is what stops "ci is red on `main`" from becoming meaningless. Neither job appears in `main-protection-checks.json`; **"signal-only" means *not listed as required*, never `continue-on-error`** — that would report green unconditionally and remove the signal rather than weaken it.

Both use **step-level `if:` guards**, not a job-level one, for the same reason `sessions-it` does: a skipped *job* reports no status at all, which blocks every PR the moment the context is promoted to required. `dorny/paths-filter` needs a diff base that `schedule` and `workflow_dispatch` do not provide, so the filter step is itself event-guarded and a `decide` step collapses "filter matched" and "manually or nightly triggered" into one output. `agentcore-image` maps `schedule` to `false` — a 40–60 minute arm64 double-build every night, to re-measure two numbers sitting at ~11% and ~20% of their budgets, is not worth it.

`temporal-it` installs a checksum-pinned `temporalio/cli` tarball (version and SHA-256 as literals in the workflow — a checksum fetched from the same host as the artifact would prove only that the download was not corrupted, never its identity; hand-bumped, Dependabot does not track it), runs `temporal server start-dev --headless`, and probes readiness with `temporal operator namespace describe default` rather than `operator cluster health` — the cluster reports healthy *before* the `default` namespace the suite connects to finishes registering, and a namespace-not-found in the first test would look like a real regression. It sets **`HELIKON_REQUIRE_TEMPORAL=1`**, which turns `gate()` in `temporal_live.rs` from a loud skip into a panic. That is load-bearing, not belt-and-braces: a skipped test *passes*, and `cargo test` captures a passing test's output, so without it a job that never reached a server is indistinguishable from a green one (the same reasoning as `HELIKON_REQUIRE_SANDBOX`). It deliberately does **not** retry — unlike `sessions-it`, which is required and retries three times so a flake cannot block a merge. The whole point of the signal-only phase is to measure the flake rate, and a retry loop erases exactly that evidence; the per-run record goes to the job summary. Promotion bar: **≥ 20 executed runs with ≤ 1 flake, or 30 consecutive green nightlies** — at which point `temporal-it` is added to both `main-protection-checks.json` and CONTRIBUTING.md's required-contexts table, and the retry decision is revisited.

`agentcore-image` runs on **`ubuntu-24.04-arm`** (free for public repos) because the Dockerfile hardcodes `--platform linux/arm64` — AgentCore's runtime targets are arm64 microVMs, and qemu-emulating a musl build of aws-lc-rs would take an hour-plus per image. It runs `scripts/agentcore-image-check.sh` with `AGENTCORE_COLD_START_LIMIT_MS=250`, because the 50 ms AC was measured on a quiet developer machine and a shared runner is a different measuring instrument; the script prints a loud `NOTE: … this is NOT the AC value` whenever the effective gate differs from the default. **The 30 MB size gate is deliberately not overridable** — it carries the STOP RULE, and an env knob on it would be precisely the quiet relaxation that rule exists to prevent. The Dockerfile's builder `RUN` uses BuildKit cache mounts so the second image reuses the first's compiled dependencies (~40–50% off the second build; no help to the first, and no persistence across runs — every job gets a fresh runner). **The `cp` out of `target/` must stay inside that same `RUN`**: a cache mount is not part of the image filesystem, so splitting it would silently produce an image with no binary in it.
```

- [ ] **Step 3: Correct `CONTRIBUTING.md:242`**

Replace:
```
To exercise the OpenAI provider against the real API, set `OPENAI_API_KEY` and run `cargo test -p paigasus-helikon-providers-openai -- --ignored`. Live tests are not part of CI.
```
with:
```
To exercise the OpenAI provider against the real API, set `OPENAI_API_KEY` and run `cargo test -p paigasus-helikon-providers-openai -- --ignored`. Those `--ignored` provider tests are not part of CI. The other env-gated live suites are: `.github/workflows/integration.yml` runs the Temporal integration suite (`temporal-it`, with `HELIKON_REQUIRE_TEMPORAL=1` so a missing server fails the job instead of skipping silently) and the AgentCore image size/cold-start gates (`agentcore-image`, on an arm64 runner) as signal-only, non-required jobs. See CLAUDE.md's CI section for the promotion bar.
```

- [ ] **Step 4: Update `docs/runbooks/agentcore-image-check.md`**

Four edits:

(a) In the Prerequisites list, replace the Docker/BuildKit bullet's first line with a BuildKit-is-mandatory statement:
```markdown
- **Docker with BuildKit — now mandatory, not merely default.** The Dockerfile's
  builder stage uses `RUN --mount=type=cache` (SMA-457), which the legacy builder
  cannot parse. The script exports `DOCKER_BUILDKIT=1` itself, so a stale
  `DOCKER_BUILDKIT=0` in your environment cannot break it.
```

(b) Replace the disk prerequisite (currently "a few hundred MB"):
```markdown
- Disk: the images themselves are a few MB each, but the BuildKit **cache mounts**
  (`$CARGO_HOME/registry` and the musl `target/` dir) persist between runs and are
  **never garbage-collected** — budget a few GB, and reclaim with
  `docker builder prune` when it grows. If a measurement ever looks wrong, re-run
  with `--no-cache`: cargo's freshness check is mtime-based against a tree
  supplied by `COPY . .`.
- GNU `date` on `PATH` (for `date +%s%N`). macOS's BSD `date` emits a literal `N`
  and the cold-start arithmetic breaks; `brew install coreutils` provides it.
```

(c) After "The one-liner", add:
```markdown
### The cold-start gate is overridable; the size gate is not

`AGENTCORE_COLD_START_LIMIT_MS` raises the cold-start budget (default `50`).
Unlike image size, that number measures the *host* as much as the image —
container-start overhead is not comparable between a quiet developer machine and
a shared CI runner. Any override prints a loud
`NOTE: … this is NOT the AC value.` above the summary table, so a reader of the
output can never mistake the effective gate for the acceptance criterion.

There is deliberately **no** size override. The 30 MB gate carries the STOP RULE,
and an env knob on it would be exactly the quiet relaxation that rule exists to
prevent. (`AGENTCORE_SIZE_LIMIT_BYTES` is inert — setting it does nothing.)

CI runs this script from `.github/workflows/integration.yml`'s `agentcore-image`
job on `ubuntu-24.04-arm` with `AGENTCORE_COLD_START_LIMIT_MS=250`, as a
signal-only (non-required) check.
```

(d) Update the "Expected output" block so the cold-start rows read `(gate)` rather than `(AC gate)`, matching Task 3 Step 4.

- [ ] **Step 5: Update the Temporal crate README**

In the "Live validation" section, after the existing `cargo test` block, add:

```markdown
Set `HELIKON_REQUIRE_TEMPORAL=1` to turn the suite's loud skip into a hard
failure. Without a server the tests *pass* (a skipped test is a passing test, and
`cargo test` captures its output), so this is what stops an unattended run from
reporting green while asserting nothing:

```bash
HELIKON_REQUIRE_TEMPORAL=1 TEMPORAL_TEST_SERVER=127.0.0.1:7233 \
  cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1
```

CI runs exactly this in the `temporal-it` job
(`.github/workflows/integration.yml`), currently as a signal-only, non-required
check.
```

- [ ] **Step 6: Fix the ambiguous gate reference in the book**

In `docs/book/src/concepts/runtimes.md:52`, replace `well under the 50 ms gate` with:
```
well under the 50 ms acceptance-criteria gate (CI re-measures on a shared arm64 runner against a deliberately looser 250 ms budget — a difference of measuring instrument, not a relaxed criterion)
```

- [ ] **Step 7: Verify the book still builds**

```bash
mdbook build docs/book
```
Expected: clean, no linkcheck warnings (`warning-policy = "error"`). If `mdbook` is not installed, note it and let CI's `book-build` job be the gate.

- [ ] **Step 8: Commit**

```bash
git add CLAUDE.md CONTRIBUTING.md docs/runbooks/agentcore-image-check.md \
        crates/paigasus-helikon-runtime-temporal/README.md docs/book/src/concepts/runtimes.md
git commit -m "docs(claude): SMA-457 document the integration workflow and its gate overrides"
git show --stat HEAD
```
Expected: exactly five files.

---

## Task 7: Full gate run, push, and record the measured numbers

**Files:**
- Modify: `docs/runbooks/agentcore-image-check.md` (add the CI-observed cold-start figure)
- Modify: `docs/superpowers/plans/2026-08-07-sma-457-ci-integration-jobs.md` (tick the boxes)

- [ ] **Step 1: Run every CI gate locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
```
All must pass. Note: run the **exact** `--workspace --all-features` test command, not per-crate — feature unification differs and per-crate runs hide failures.

- [ ] **Step 2: Verify the commit range is clean**

```bash
convco check origin/main..HEAD
git log --oneline origin/main..HEAD
```
Expected: convco clean; commits for Tasks 1-6 plus the two spec commits.

- [ ] **Step 3: Push and watch both new jobs**

```bash
git push -u origin feature/sma-457-ci-add-temporal-it-and-agentcore-docker-build-jobs
```
The pre-push hook runs fmt + full-workspace clippy and takes 5+ minutes — this is expected, not a hang. Background it or use `--no-verify` since Step 1 already ran those gates.

Then watch:
```bash
gh run list --workflow=integration.yml --limit 3
gh run watch $(gh run list --workflow=integration.yml --limit 1 --json databaseId --jq '.[0].databaseId')
```

Both jobs **must actually run on this PR**: it touches `.github/workflows/integration.yml` (in both filters), `crates/paigasus-helikon-runtime-temporal/**`, `crates/paigasus-helikon-runtime-agentcore/**`, and `scripts/agentcore-image-check.sh`. If either reports a no-op, the filter is wrong — fix it before proceeding.

- [ ] **Step 4: Confirm `temporal-it` genuinely executed the suite**

In the job log, confirm: `6 passed` (not `0 passed`), and **no** `SKIPPED:` line. Confirm the "Self-test the never-green-by-skip guard" step passed — it must print `guard OK`, meaning the suite genuinely failed when the server address was withheld. Confirm the job summary shows the outcome and wall-clock. Compare that duration against Task 1 Step 7's local figure — an order-of-magnitude difference means the runner is far more contended than assumed and `timeout-minutes` needs revisiting.

- [ ] **Step 5: Record the CI-observed numbers**

From `agentcore-image`'s job summary, take both image sizes, both cold starts, and the `df -h` figures. Add a row to the runbook's validated-run table:

```markdown
> **CI-observed (`ubuntu-24.04-arm`, `agentcore-image` job, <DATE>):** echo
> <X> ms / agent <Y> ms exec→`/ping`-200 against the 250 ms CI budget; image
> sizes <A> MB / <B> MB, unchanged from the macOS run. Peak disk use during the
> two builds: <Z> GB of the runner's <T> GB.
```

Substitute the real measured values. If disk headroom is thin (< 3 GB free after the build), apply mitigation (a) from the spec's §6.3 — drop the `/workspace/target` cache mount and keep only `$CARGO_HOME/registry` — and note the trade in the runbook.

- [ ] **Step 6: Tick this plan's checkboxes and commit**

```bash
git add docs/runbooks/agentcore-image-check.md docs/superpowers/plans/2026-08-07-sma-457-ci-integration-jobs.md
git commit -m "docs(plan): SMA-457 record CI-observed image gate figures"
git push
```

---

## Plan self-review

**Spec coverage:** §3 workflow placement + triggers → Task 4 Step 1. §3.1 decide-step mechanics → Tasks 4 & 5. §4 rejected alternatives → no task (spec-only). §5 `temporal-it` → Task 4. §5.1 green-by-skip guard → Task 1. §5.2 CLI pin → Task 4 Step 1. §5.3 server + suite → Task 4 Step 1. §5.4 flake record → Task 4 Step 1 (summary step) + nightly cron. §5.5 diagnostics → Task 4 Step 1. §6 `agentcore-image` → Task 5. §6.1 script changes → Task 3. §6.2 Dockerfile → Task 2. §6.3 disk budget → Task 5 Step 1 + Task 7 Step 5. §6.4 the 250 ms revisit → Task 7 Step 5. §7 docs (5 files) → Task 6. §8 non-changes → Task 5 Step 3 asserts the ruleset is untouched. §10 ACs → AC1 Task 4/5, AC2 Task 1 Steps 4/6 + Task 7 Step 4, AC3 Task 1 Step 5, AC4 Task 5 + Task 7, AC5 Task 3 Steps 6/8/9, AC6 Task 2 Step 3, AC7 Task 5 Step 3, AC8 Task 6 Step 7, AC9 Task 7 Step 1.

**Placeholder scan:** the only intentional placeholders are `<X>`/`<Y>`/`<A>`/`<B>`/`<Z>`/`<T>`/`<DATE>` in Task 7 Step 5, which are measured values that cannot exist before the run produces them; the step says exactly where each comes from.

**Type/name consistency:** `HELIKON_REQUIRE_TEMPORAL` (Task 1 → Task 4 job `env`, Task 6 docs); `AGENTCORE_COLD_START_LIMIT_MS` (Task 3 → Task 5 step `env`, Task 6 docs); `COLD_START_LIMIT_MS_DEFAULT` used by both the NOTE and the summary block in Task 3; step ids `filter`/`decide`/`suite` referenced consistently within each job; job names `temporal-it`/`agentcore-image` match between Tasks 4/5, Task 5 Step 3's ruleset assertion, and Task 6's docs.
