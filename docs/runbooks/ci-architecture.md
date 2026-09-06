# CI Architecture Reference

> **Scope:** why the CI, integration and supply-chain workflows are shaped the way
> they are, and the incidents that shaped them (SMA-306/330/335/452/457/458/479/486/487/581/618).
> This runbook is **not** linked from the public mdBook — it lives standalone under
> `docs/runbooks/` to avoid linkcheck coupling. It holds the *rationale and incident
> history*; the operative rules stay in `CLAUDE.md`.

## Workflow inventory

`.github/workflows/ci.yml` runs nine jobs on every PR (the `commits` job is PR-only; the other eight also run on push to `main`): `fmt`, `clippy`, `test` (matrix: `{ubuntu, macos, windows} × {stable, 1.94}`, `fail-fast: false`), `build-no-default-features` (SMA-452: `cargo build --no-default-features` for both `paigasus-helikon-runtime-axum` and `paigasus-helikon-runtime-actix`, catching `openapi`-feature-gating regressions, plus a `cargo tree` assertion that axum does not leak into the `runtime-actix` feature graph), `docs` (with `RUSTDOCFLAGS=-D warnings`), `doc-coverage` (nightly rustdoc `--show-coverage`, aggregated by `scripts/check-doc-coverage.sh`, gated at `DOC_COVERAGE_THRESHOLD` — default 80%), `commits` (SMA-335: `convco check` against the PR's commit range, gated by `if: github.event_name == 'pull_request'`), `sessions-it` (SMA-330: Postgres/Redis session integration suite, path-filtered, on both PR and push to `main`), and `markdown-lint` (SMA-581: `markdownlint-cli2` over every published Markdown surface, with the gated file set and rule policy in `.markdownlint-cli2.jsonc`; deliberately not path-filtered, because a path-filtered *required* check never reports on a PR touching no Markdown and blocks it forever). The `paigasus-helikon-cli` crate is excluded from both the `missing_docs` lint and the coverage aggregator until its public API stabilizes. Three of the six `test` legs are **required** — `ubuntu`, `macos` and `windows` on `stable`; the three `1.94` legs are signals. Windows was promoted in SMA-569: it is the only gate that exercises `spawn_capped`'s Windows timeout path (`cmd /C` spawning plus a `TerminateProcess`-based kill, which has no process-group semantics and assigns a real exit code), and the unix legs cannot fail on that path — SIGKILL makes `ExitStatus::code()` return `None` whether or not the bug is present, so a unix-only assertion is characterization, not regression.

## Workflow permissions

`ci.yml` declares `contents: read` plus `pull-requests: read` — the latter for `sessions-it`'s `dorny/paths-filter`, which calls `pulls.listFiles` on PR events (SMA-487); the workflow comment carries the rationale and the reason it must not be minimised away.

## integration.yml — the signal-only tier

`.github/workflows/integration.yml` (SMA-457) runs two **signal-only** jobs — `temporal-it` and `agentcore-image` — on PR, push to `main`, a nightly cron at 05:00 UTC, and `workflow_dispatch`. Deliberately **not** in `ci.yml`: `temporal-it` is *expected* to flake while it earns promotion (its crash-resume test aborts a real worker against wall-clock activity timeouts), and a failing job makes its whole workflow run conclude `failure` whether or not it is required — so keeping these out of `ci.yml` is what stops "ci is red on `main`" from becoming meaningless. Neither job appears in `main-protection-checks.json`; **"signal-only" means *not listed as required*, never `continue-on-error`** — that would report green unconditionally and remove the signal rather than weaken it.

Both use **step-level `if:` guards**, not a job-level one, for the same reason `sessions-it` does: a skipped *job* reports no status at all, which blocks every PR the moment the context is promoted to required. `dorny/paths-filter` needs a diff base that `schedule` and `workflow_dispatch` do not provide, so the filter step is itself event-guarded and a `decide` step collapses "filter matched" and "manually or nightly triggered" into one output. `agentcore-image` maps `schedule` to `false`. Measured cold on the first CI run, both images build and all four gates run in **~4 minutes**, so this is not a cost decision — re-measuring two numbers that sit far under their budgets every night simply adds no information. `workflow_dispatch` still reaches the job.

`temporal-it` installs a checksum-pinned `temporalio/cli` tarball (version and SHA-256 as literals in the workflow — a checksum fetched from the same host as the artifact would prove only that the download was not corrupted, never its identity; hand-bumped, Dependabot does not track it), runs `temporal server start-dev --headless`, and probes readiness with `temporal operator namespace describe default` rather than `operator cluster health` — the cluster reports healthy *before* the `default` namespace the suite connects to finishes registering, and a namespace-not-found in the first test would look like a real regression. It sets **`HELIKON_REQUIRE_TEMPORAL=1`**, which turns `gate()` in `temporal_live.rs` from a loud skip into a panic. That is load-bearing, not belt-and-braces: a skipped test *passes*, and `cargo test` captures a passing test's output, so without it a job that never reached a server is indistinguishable from a green one (the same reasoning as `HELIKON_REQUIRE_SANDBOX`). It deliberately does **not** retry — unlike `sessions-it`, which is required and retries three times so a flake cannot block a merge. The whole point of the signal-only phase is to measure the flake rate, and a retry loop erases exactly that evidence; the per-run record goes to the job summary. Promotion bar: **≥ 20 executed runs with ≤ 1 flake, or 30 consecutive green nightlies** — at which point `temporal-it` is added to both `main-protection-checks.json` and CONTRIBUTING.md's required-contexts table, and the retry decision is revisited.

`agentcore-image` runs on **`ubuntu-24.04-arm`** (free for public repos) because the Dockerfile hardcodes `--platform linux/arm64` — AgentCore's runtime targets are arm64 microVMs, and qemu-emulating a musl build of aws-lc-rs would take an hour-plus per image. It runs `scripts/agentcore-image-check.sh` with `AGENTCORE_COLD_START_LIMIT_MS=250`, because the 50 ms AC was measured on a quiet developer machine and a shared runner is a different measuring instrument; the script prints a loud `NOTE: … this is NOT the AC value` whenever the effective gate differs from the default. **The 30 MB size gate is deliberately not overridable** — it carries the STOP RULE, and an env knob on it would be precisely the quiet relaxation that rule exists to prevent. The Dockerfile's builder `RUN` uses BuildKit cache mounts so the second image reuses the first's compiled dependencies (~40–50% off the second build; no help to the first, and no persistence across runs — every job gets a fresh runner). **The `cp` out of `target/` must stay inside that same `RUN`**: a cache mount is not part of the image filesystem, so splitting it would silently produce an image with no binary in it.

## protoc (`.github/actions/setup-protoc`)

**`protoc` comes from `.github/actions/setup-protoc`, a repo-local composite action, not from a third-party one** (SMA-458). It installs **protoc 35.1**, pinned exactly and verified against a per-platform SHA-256 **before** extraction, at all nine sites that compile the workspace (`ci.yml` ×6, `msrv.yml`, `release-plz.yml`, `integration.yml`). It replaced `arduino/setup-protoc`, whose `version` input **defaults to `23.x`, not to latest** — the action's README claims otherwise and is wrong, and CI had therefore been running **23.4** since SMA-332. SMA-458 was consequently a deliberate 12-major upgrade as well as a pin, not the no-op its one-line ticket framing implied. `install.sh` does download → verify → extract → export; that order is load-bearing, so an unverified archive never reaches an executable location. It exports `PROTOC` and `PROTOC_INCLUDE` via `$GITHUB_ENV` as well as prepending to `$GITHUB_PATH`, because `prost-build` resolves `PROTOC` **before** falling back to a `PATH` lookup — that makes the install authoritative regardless of `PATH` ordering, and moots the well-known-type `include/` tree having to sit beside the binary. **`verify.sh` must stay its own step**: `$GITHUB_PATH`/`$GITHUB_ENV` writes do not affect the step that makes them, so an assertion folded back into `install.sh` would validate a local `export PATH=` rather than the mechanism cargo sees, and would be structurally blind to the propagation failure it exists to catch. Only `Linux-X64`, `macOS-ARM64` and `Windows-X64` are supported; anything else exits non-zero naming the file to edit. `linux-aarch_64` is deliberately absent even though `agentcore-image` runs on `ubuntu-24.04-arm` (it has no protoc step) — an unexercised digest is an unexercised code path, and a wrong one reads as tampering rather than as a typo.

**Nothing tracks the protoc pin — bumping it is a human act with no prompt.** Dependabot follows action SHAs, and after SMA-458 there is no third-party action here for it to follow at all. It sits alongside the repo's other hand-bumped pins: `TEMPORAL_CLI_VERSION`/`TEMPORAL_CLI_SHA256` in `integration.yml` and `NIGHTLY_TOOLCHAIN` in `ci.yml`. **Bump runbook:** edit `PROTOC_VERSION` and all three digests in `.github/actions/setup-protoc/install.sh` and `EXPECTED_VERSION` in `verify.sh`, then run `bash .github/actions/setup-protoc/selftest.sh` — it re-downloads every published asset and fails if any pinned digest disagrees, and also exercises the tampered-digest and unsupported-platform paths. Neither `actionlint` nor `shellcheck` runs in CI (SMA-618 checked: the only match in the tree is a `# shellcheck` directive comment inside `selftest.sh`), so `selftest.sh` is the **only** coverage the install logic has — which is why it re-downloads every published asset rather than merely asserting the digests are well-formed. **A checksum mismatch is not a signal to update the digest** — the causes are, in order, a truncated download, an upstream re-tag, and tampering; verify upstream independently first. The accepted cost of pinning is that it does not self-heal: if protobuf removes or replaces the v35.1 assets, every required job and `release-plz` go red until someone bumps the pin.

## pr-title.yml

`.github/workflows/pr-title.yml` (SMA-335) runs `amannn/action-semantic-pull-request` on `pull_request_target` to gate the PR title — the squashed commit on `main`. Permissions are minimal (`pull-requests: read`, `statuses: write`); no `actions/checkout` step under `pull_request_target` keeps PR-controlled code out of the runner. Concurrency keys on `github.event.pull_request.number` because `pull_request_target` sets `github.ref` to the base ref and keying on it would cross-cancel different PRs. Dependabot PRs are exempt from the title check via `ignoreLabels: [area:deps]` — their auto-generated `Bump …` titles capitalize the subject and can't be reconfigured, so they'd otherwise block every dependency PR; the ignore label makes the check **pass** for them (not skip-and-block, which would leave the required context unreported and still block). Don't remove it.

## markdownlint pinning

**`markdownlint-cli2` is pinned by `package-lock.json`, not by a GitHub Action.** `DavidAnson/markdownlint-cli2-action` was rejected because the action version *is* the linter version, and markdownlint ships new rules in **minor** releases — which Dependabot's `github-actions` group takes. A routine grouped `chore(deps)` PR could therefore redden a *required* gate for reasons nobody chose, and the `branch-names` ruleset blocks humans pushing to `dependabot/**`, so it could not be fixed in place. `npm` is not a configured Dependabot ecosystem, so this joins the repo's hand-bumped pins alongside `TEMPORAL_CLI_VERSION` in `integration.yml` and `PROTOC_VERSION` in `.github/actions/setup-protoc/install.sh`. **Two of this tool's failure modes report success:** an invalid rule-option value (e.g. `MD060: { style: "consistent" }`) silently disables that rule, and the gated file set can silently collapse or narrow to a subtree. `scripts/check-markdownlint-config.sh` runs as a step in the job and asserts: that several independently-probed gated areas (a deep book path, a crate README location, and the repo root) are each actually linted; that an explicitly excluded area is not; that the configured MD060 rule value is in force; and that rules which are only on by virtue of `"default": true` (not explicitly configured) are still firing, so a `"default": false` regression is caught even though it changes no file's membership. Do not fold it into the lint step; it must be able to fail independently.

## Supply chain: audit, deny, sbom

Supply-chain workflows (`.github/workflows/audit.yml`, `deny.yml`, `sbom.yml`) are separate from `ci.yml` because they have independent triggers and failure semantics. Required status checks added in SMA-306: `audit`, `deny` (declared in `.github/rulesets/main-protection-checks.json` alongside the CI gates). Both `audit.yml` and `deny.yml` run on **push to `main`, PRs, a daily cron, and `workflow_dispatch`** — the cron and the manual trigger were aligned in SMA-479 so that `main` is re-evaluated daily at exactly PR severity.

**The two jobs in `audit.yml` have deliberately different roles, and only one of them is a verdict.** The `audit` job runs `cargo audit --deny warnings` on *every* event — it is the same job definition that gates PRs, un-gated in SMA-479 precisely so the daily and PR severities cannot drift apart. Do not re-add an event filter to it, and do not copy its command into a second step somewhere: one command, in one place, is the whole point. The `scheduled-audit` job runs `rustsec/audit-check` for its auto-issue-filing behaviour (the only place in the repo where a wrapper action is preferred over direct tool invocation) — and **its green status means nothing at any severity**. The action routes `schedule` events to a `reportIssues()` code path that files issues and returns without ever failing, including for critical vulnerabilities; it also files nothing at all for yanked crates, and never re-files an advisory whose issue has been closed. Read the *run* conclusion, never `scheduled-audit`'s job status. Correspondingly, do **not** widen its `if:` to include `workflow_dispatch` — a non-schedule event routes the action to `reportCheck()`, which needs `checks: write` that the job does not grant, and 403s.

Both workflows key their concurrency group on `github.event_name` as well as `github.ref`. This is load-bearing, not decoration: `schedule`, `workflow_dispatch`, and `push` to `main` all resolve `github.ref` to `refs/heads/main`, so a shared group with `cancel-in-progress: false` lets a queued cron run sit *pending* until the next merge cancels it — silently discarding the day's only strict evaluation of `main`. Do not simplify the key back.

Reading the daily signal: **green means the strict `audit` job passed; red means some job in the workflow failed and needs triage; absent or `cancelled` means unverified.** Red is *not* a synonym for "advisory present" — the run also goes red when the advisory-DB or crates.io fetch hits a network failure, and when `scheduled-audit` itself fails (e.g. the GitHub API rejecting an issue write) even though `audit` passed. So check *which job* failed before concluding anything, then reproduce with `cargo audit --deny warnings` on a clean checkout. Scheduled runs are also best-effort — GitHub can delay or drop them under load and disables them entirely after 60 days of repository inactivity — so a missing row is not a passing row, and no upper bound on staleness can actually be guaranteed.

To read the verdict against a commit, use the **Checks API**, not the legacy commit-status API: `gh api repos/SMK1085/paigasus-helikon/commits/main/check-runs --jq '.check_runs[] | select(.name=="audit" or .name=="deny") | {name, status, conclusion}'`. GitHub Actions publishes *check runs*; `/commits/{ref}/status` returns only legacy statuses and on this repo reports just `CodeRabbit` — so it renders a confident `state: success` that contains no audit verdict whatsoever. Reading it would reproduce exactly the bug this section exists to prevent. The `deny` job additionally runs `scripts/check-advisory-ignore-sync.sh`, which asserts that the `[advisories].ignore` lists in `.cargo/audit.toml` and `deny.toml` have not drifted apart — they are policy-mirrored, and both are now evaluated daily against the same database.

The SBOM workflow invokes `cargo cyclonedx --manifest-path crates/paigasus-helikon/Cargo.toml --format json --spec-version 1.5 --all-features`. cargo-cyclonedx 0.5.x has no `-p` flag (must target via `--manifest-path`) and defaults to `--spec-version 1.3`, so 1.5 is pinned explicitly. With `--all-features` the facade's dep graph equals the workspace's dep graph, so one SBOM covers everything. The workflow's `find crates/paigasus-helikon -maxdepth 1 -name '*.cdx.json'` picks the facade's SBOM specifically — cargo-cyclonedx 0.5 walks the workspace and emits one SBOM under each member directory regardless of which member you point at, so scoping the find pattern matters.

`deny.toml` declares `version = 2` under both `[advisories]` and `[licenses]` — v1 fields (`vulnerability`, `unmaintained`, `unsound`, `copyleft`, etc.) are removed in modern cargo-deny and adding them will fail with a schema error. The license allowlist includes `Unicode-3.0` in addition to the ticket-prescribed `Unicode-DFS-2016` because `unicode-ident ≥ 1.0.13` (pulled transitively by `serde_derive`) relicensed in 2024. cargo-deny's advisory DB lives at `~/.cargo/advisory-dbs` (plural) per `deny.toml`'s `db-path`; cargo-audit's DB is at `~/.cargo/advisory-db` (singular) — each tool caches its own, and the CI cache directories are scoped per-workflow.

Dependabot is configured for `cargo` + `github-actions` ecosystems, weekly Monday 06:00 UTC (aligned with the daily audit cron), with patch + minor updates grouped into one PR per ecosystem.

## Actions cache budget

GitHub's Actions cache limit is **10 GB per repository and is not raisable**.
Going over it is not an error anywhere in the UI or API — it is silent LRU
eviction, one entry at a time, with no warning and no red gate. SMA-618 found
this repository sitting **37% over the limit**, which meant a different CI leg
started cold on essentially every run; SMA-618 records a cold `test
(windows-latest, stable)` run costing 42m40s — the true cost of a cold run of
that leg, against an earlier, incomplete cold measurement of 34m09s from a run
that aborted early at a failing test before it ever reached the later
binaries. Full derivation, the two-cause breakdown, and the size-reduction
work reserved for the follow-up PR live in
`docs/superpowers/specs/2026-09-06-sma-618-actions-cache-budget-design.md`.

Two independent causes fed the overage. First, every `Swatinem/rust-cache` site
used the default `save-if: true`, so every PR job saved its own entry scoped to
`refs/pull/N/merge` — read a handful of times, then dead, but evicting `main`'s
entries while it lived. Second, `main`'s own footprint — 15 entries across five
workflows — exceeds 10 GB by itself, independent of any PR activity; that half
is out of scope for this PR and addressed separately. (That 15-entry count
itself carries a caveat: `sessions-it` and `temporal-it` gate their cache steps
behind path filters, so it holds only on a push that touches both `ci.yml` and
`integration.yml` — most `main` pushes see 13. See "Actions cache budget"
below.) This PR (SMA-618, PR 1) fixes the first cause and adds the guards
below; it does not close the ticket by itself.

**PR jobs restore but no longer save.** All twelve `Swatinem/rust-cache` sites
across the seven workflows that use it (`ci.yml`, `msrv.yml`, `bench.yml`,
`audit.yml`, `deny.yml`, `integration.yml`, `sbom.yml`) now carry `save-if: ${{
github.ref == 'refs/heads/main' }}`. `save-if` gates only the action's save
(post) step — the restore step is unconditional, and a `refs/pull/N/merge` run
can still read an entry that `main` wrote, so a PR is no colder than before.
What it no longer does is write a competing `refs/pull/N/merge` entry that
evicts `main`'s. `sbom.yml` is triggered only on `paigasus-helikon-v*` tag pushes, so its
`save-if` expression is never true and the step is permanently restore-only —
intended, and commented in the file, because `main` never runs the `sbom` job
and there is no `sbom`-keyed entry for a tag build to restore either.

**`audit` and `deny` cache no target directory.** Neither job compiles the
workspace — `audit` runs `cargo audit` and `deny` runs `cargo deny check`
against the dependency graph and lockfile — so caching `target/` for either was
pure waste against a fixed budget. Both gained `cache-targets: "false"` on
their `Swatinem/rust-cache` step and keep only their `cache-directories`
advisory-DB entry: `~/.cargo/advisory-db` (singular) for `audit`,
`~/.cargo/advisory-dbs` (plural) for `deny` — genuinely different paths, one
per tool, as already noted above under "Supply chain: audit, deny, sbom".

**The cargo-visible environment must stay identical across every cache-bearing
workflow.** `rust-cache` hashes every environment variable whose name begins
`CARGO`, `CC`, `CFLAGS`, `CXX`, `CMAKE`, or `RUST` into its cache key (verified
in `Swatinem/rust-cache`'s `src/config.ts` at the pinned SHA). Two jobs meant to
share one entry only do so while every such variable agrees byte-for-byte;
drift silently splits a shared entry into two and pushes the repository back
over the 10 GB limit with nothing going red. `scripts/check-cargo-profile-env-sync.sh`
is the guard: it asserts that every workflow containing a `Swatinem/rust-cache`
step declares an identical set of workflow-level `env:` entries matching those
prefixes, rejects job-level env with those prefixes in a cache-bearing
workflow, and rejects env declared on a `rust-cache` step itself. It runs as a
step in `ci.yml`'s `fmt` job, with a self-test at
`scripts/check-cargo-profile-env-sync-selftest.sh` pinning its line-oriented
parsing contract. Fixing this guard's own prerequisite surfaced real
pre-existing drift: `msrv.yml` and `bench.yml` had no workflow-level `env:`
block at all, so they now carry `CARGO_TERM_COLOR: always` like every other
cache-bearing workflow. That addition is itself cache-key-invalidating —
`CARGO_TERM_COLOR` is one of the hashed prefixes, so adding it where it was
previously absent changes the key exactly as removing a cached path does (see
`cache-targets: false` below) — so `msrv.yml`'s `verify` and `bench.yml`'s
`bench` entries are each orphaned once and run cold once under this PR, the
same cost as `audit` and `deny`.

**`cache-budget.yml` is the daily monitor.** It runs on a `schedule` (06:43
UTC daily) plus `workflow_dispatch`, never on `push` or `pull_request`, and it
warns, it never fails — a budget drifting toward the limit is something to fix
deliberately, not something to block a merge on. It reads the Actions cache
**list** endpoint — `gh api repos/SMK1085/paigasus-helikon/actions/caches` —
and never the `actions/cache/usage` summary endpoint: SMA-618 measured the
latter reporting `active_caches_count: 4` against a **list** response that
returned six rows, with the byte totals agreeing between the two, so the count
field is unreliable and the list is the only source of truth. The step prints a
full inventory to the job summary and emits a GitHub `::warning::` once the
total crosses 8.5 GiB, chosen to leave headroom before eviction actually
starts. The `gh api` call is guarded rather than a bare assignment under
`set -e`, so a transient failure (rate limit, a denied `actions: read`, a
network blip) degrades to a warning instead of reddening the run. This
workflow is deliberately **not** a job inside `audit.yml`, even though
`audit.yml` already runs a daily cron against `main` — CLAUDE.md instructs
readers to take the `audit` workflow's *run* conclusion as the supply-chain
verdict, and an unrelated job failing inside that workflow would turn a green
audit run red for a reason with nothing to do with supply chain, corrupting
exactly the signal that document tells people to trust.

**Measurement procedure**, for anyone re-checking the budget by hand:

```bash
gh api repos/SMK1085/paigasus-helikon/actions/caches --paginate \
  --jq '.actions_caches[] | "\(.size_in_bytes) \(.ref) \(.key)"'
```

Live on 2026-09-06, mid-way through this work, the repository measured **10.08
GiB across 7 entries** — still over the 10 GB limit, because the stale
PR-scoped entries from before this fix had not yet aged out and `main`'s own
15-entry footprint (Cause 2, above) is untouched by this PR.

**Purging every cache entry** requires `actions: write`. No workflow in this
repository performs the purge — it is a developer-machine operation, run with
a personal access token carrying that scope; the workflow `GITHUB_TOKEN` is
never involved:

```bash
gh api --paginate repos/SMK1085/paigasus-helikon/actions/caches \
  --jq '.actions_caches[].id' \
| xargs -I{} gh api --method DELETE \
    repos/SMK1085/paigasus-helikon/actions/caches/{}
```

This is **not a one-time operation**. Any PR still based on pre-merge `main`
runs the *old* workflow definitions — the ones without `save-if` — on its next
push, and keeps writing `refs/pull/N/merge` entries until it is rebased onto a
`main` that carries this fix. Re-purge as needed, or wait for the outstanding
PRs to rebase or merge, before trusting a fresh measurement.
