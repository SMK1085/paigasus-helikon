# SMA-306 — Supply-chain security: `cargo-audit`, `cargo-deny`, SBOM

**Linear issue**: [SMA-306](https://linear.app/smaschek/issue/SMA-306/supply-chain-security-cargo-audit-cargo-deny-sbom)
**Status**: design approved 2026-05-16
**Branch**: `feature/sma-306-supply-chain-security-cargo-audit-cargo-deny-sbom`
**Depends on**: SMA-304 (workspace skeleton — landed), SMA-305 (CI — landed)
**Related**: SMA-309 (branch protection consumes the two new required-status-check IDs this PR produces)

## 1. Goal & non-goals

**Goal.** Land three new GitHub workflows and one root-level policy file that establish the supply-chain security baseline for the workspace:

1. `.github/workflows/audit.yml` — `cargo audit` on every PR and push to `main` (PR-gating, deterministic via `taiki-e/install-action`), plus a daily scheduled run on `main` via `rustsec/audit-check@v2` that auto-files a GitHub issue if RustSec discovers a new advisory affecting locked deps.
2. `deny.toml` + `.github/workflows/deny.yml` — `cargo-deny check` across all four categories (`advisories`, `bans`, `licenses`, `sources`) on every PR. License allowlist matches the ticket plus `Unicode-3.0` (see §8.1).
3. `.github/workflows/sbom.yml` — on `v*` tag push, build a CycloneDX SBOM via `cargo-cyclonedx` and upload it as a release asset.
4. `.github/dependabot.yml` — `cargo` + `github-actions` ecosystems, weekly, grouped patch+minor.
5. GitHub Dependency Graph + Dependabot alerts + Dependabot security updates + secret scanning + secret-scanning push protection — enabled via repo settings (documented; not file-config).

The bootstrap PR for SMA-306 produces:

- All three workflows green on the PR itself (`audit` + `deny` gate the PR; `sbom` does not run on PRs).
- A **temporary tag push** (`v0.0.0-sma306-smoketest`) verifying the `sbom` workflow end-to-end, with the test release and tag deleted after verification.

**Required-status-check IDs produced for SMA-309:**

- `audit / audit` (PR-gating; the scheduled job is signal-only and never runs on PRs)
- `deny / deny` (PR-gating)
- `sbom / sbom` is **not** in the required list (tag-only trigger).

**Non-goals.**

- Release-plz integration (separate Stage-N ticket).
- Reproducible builds, SLSA provenance, sigstore signing.
- `cargo-vet` (provenance-of-review attestations) — different problem domain.
- Writing per-crate `#[deny(...)]` attributes; `deny.toml` is the only policy surface here.
- Auto-merging Dependabot PRs.
- SBOM diffing or vuln-correlation against the SBOM artifact (CycloneDX is emitted as-is).

## 2. File layout

```text
.github/
├── dependabot.yml          (new — cargo + actions, weekly, grouped)
└── workflows/
    ├── ci.yml              (untouched)
    ├── msrv.yml            (untouched)
    ├── audit.yml           (new)
    ├── deny.yml            (new)
    └── sbom.yml            (new)
deny.toml                   (new — workspace root, alongside Cargo.toml)
CONTRIBUTING.md             (append §"Supply-chain security")
CLAUDE.md                   (append a short supply-chain note under "## CI")
```

No changes to `Cargo.toml`, `scripts/`, or any crate.

`audit.yml`, `deny.yml`, `sbom.yml` are three files (not one consolidated `supply-chain.yml`) for the same reason `msrv.yml` is separate from `ci.yml`: independent triggers, independent failure semantics, and independent required-status-check IDs that SMA-309 references by workflow name.

`deny.toml` lives at the workspace root because `cargo-deny` discovers it from `cargo metadata`'s `workspace_root`, and a sibling location keeps the policy visible next to `Cargo.toml`.

## 3. `deny.toml`

```toml
# deny.toml — cargo-deny policy for the Helikon workspace.
# Reference: https://embarkstudios.github.io/cargo-deny/

[graph]
# Constrain the graph to the targets we claim to support. A wider target list
# makes cargo-deny's graph quadratically larger for no signal.
targets = [
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc",
]
all-features = true

[output]
feature-depth = 1

# ---------------------------------------------------------------------------
# Advisories — RustSec database. cargo-audit runs the same DB; deny lives
# here so policy (yanked, ignore list) is auditable in a single file.
# ---------------------------------------------------------------------------
[advisories]
version = 2
db-path = "~/.cargo/advisory-dbs"
db-urls = ["https://github.com/rustsec/advisory-db"]
yanked = "deny"
ignore = []
# severity-threshold deliberately unset = report everything

# ---------------------------------------------------------------------------
# Licenses — ticket allowlist plus Unicode-3.0 (the rebrand of
# Unicode-DFS-2016 used by unicode-ident ≥ 1.0.13, pulled transitively by
# serde_derive et al. — see §8.1).
# ---------------------------------------------------------------------------
[licenses]
version = 2
confidence-threshold = 0.93
allow = [
  "Apache-2.0",
  "MIT",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "ISC",
  "Unicode-DFS-2016",
  "Unicode-3.0",
  "MPL-2.0",
]
exceptions = []
# Per-crate carve-outs land here as `{ allow = [...], crate = "..." }` once
# cargo-deny tells us they're needed.

# ---------------------------------------------------------------------------
# Bans — empty allow/deny lists; `wildcards = "deny"` enforces precise
# version constraints, `multiple-versions = "warn"` because transitive dupes
# are unavoidable while the workspace is small (see §8.2).
# ---------------------------------------------------------------------------
[bans]
multiple-versions = "warn"
wildcards = "deny"
highlight = "all"
workspace-default-features = "allow"
external-default-features = "allow"
allow = []
deny = []
skip = []
skip-tree = []

# ---------------------------------------------------------------------------
# Sources — crates.io only; deny git-only deps until we have a concrete
# reason to allow one.
# ---------------------------------------------------------------------------
[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
allow-git = []
```

Key design points:

- **`version = 2`** under `[advisories]` and `[licenses]` opts into the modern cargo-deny config schema; the v1 fields (`vulnerability`, `unmaintained`, `unsound`, `notice`, `copyleft`, `default`, `unlicensed`) are removed.
- **`Unicode-3.0` allowlist entry** is additive to the ticket's seven-license list; rationale in §8.1.
- **`multiple-versions = "warn"`** rather than `"deny"`: the workspace will have transitive duplicate versions during ecosystem transitions (e.g. `syn 1.x` vs `syn 2.x`); tightening to `"deny"` now would force premature dedup. Revisit after Stage-1.
- **No `[advisories].ignore`** carve-outs in the bootstrap config. Surface every advisory; add ignore-with-justification entries only when a specific CVE has a documented reason to wait.
- **`db-path = "~/.cargo/advisory-dbs"`** so the cached DB survives between CI runs when paired with `Swatinem/rust-cache`'s `cache-directories` input.

## 4. Workflows

### 4.1 `.github/workflows/audit.yml`

Two jobs in one workflow, gated by `github.event_name`:

- `audit` (PR / push to `main`) — `taiki-e/install-action` + `cargo audit --deny warnings`. Deterministic, the merge gate.
- `scheduled-audit` (daily 06:00 UTC) — `rustsec/audit-check@v2` runs the same DB and auto-files an issue if a new advisory matches the locked deps. Scoped `issues: write` at the job level so the PR-time job stays read-only.

```yaml
name: audit

on:
  push:
    branches: [main]
  pull_request:
  schedule:
    - cron: "0 6 * * *"   # daily, 06:00 UTC

concurrency:
  group: audit-${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

permissions:
  contents: read

env:
  CARGO_TERM_COLOR: always

jobs:
  audit:
    if: github.event_name != 'schedule'
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
        with:
          cache-directories: "~/.cargo/advisory-db"
      - uses: taiki-e/install-action@v2
        with:
          tool: cargo-audit
      - run: cargo audit --deny warnings

  scheduled-audit:
    if: github.event_name == 'schedule'
    runs-on: ubuntu-latest
    permissions:
      contents: read
      issues: write
    steps:
      - uses: actions/checkout@v6
      - uses: rustsec/audit-check@v2
        with:
          token: ${{ secrets.GITHUB_TOKEN }}
```

Required status check ID: **`audit / audit`** (only the PR-gating job; `scheduled-audit` never runs on a PR so it is never "expected" in PR checks).

`cargo audit`'s default cache path is `~/.cargo/advisory-db` (singular). `cargo-deny`'s default is `~/.cargo/advisory-dbs` (plural). They are distinct directories; each workflow caches its own.

### 4.2 `.github/workflows/deny.yml`

```yaml
name: deny

on:
  push:
    branches: [main]
  pull_request:

concurrency:
  group: deny-${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

permissions:
  contents: read

env:
  CARGO_TERM_COLOR: always

jobs:
  deny:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
        with:
          cache-directories: "~/.cargo/advisory-dbs"
      - uses: taiki-e/install-action@v2
        with:
          tool: cargo-deny
      - run: cargo deny --all-features check
```

`cargo deny check` (no category argument) runs `advisories`, `bans`, `licenses`, `sources` in one pass. Required status check ID: **`deny / deny`**.

### 4.3 `.github/workflows/sbom.yml`

```yaml
name: sbom

on:
  push:
    tags:
      - "v*"

concurrency:
  group: sbom-${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: false   # tags are immutable; never cancel an in-flight run

permissions:
  contents: write   # create release / upload asset

env:
  CARGO_TERM_COLOR: always

jobs:
  sbom:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - uses: taiki-e/install-action@v2
        with:
          tool: cargo-cyclonedx

      - name: Generate CycloneDX SBOM
        env:
          REF_NAME: ${{ github.ref_name }}
        run: |
          # Facade crate with --all-features captures the full workspace
          # surface (paigasus-helikon-core unconditionally + 10 siblings).
          cargo cyclonedx --format json --all-features -p paigasus-helikon
          # cargo-cyclonedx writes the SBOM under the package directory;
          # exact filename depends on installed version (bom.cdx.json on
          # recent releases). Locate and rename for the release asset.
          SBOM="$(find crates/paigasus-helikon -maxdepth 1 -name '*.cdx.json' | head -n1)"
          test -n "$SBOM"
          cp "$SBOM" "paigasus-helikon-${REF_NAME}-cyclonedx.json"

      - name: Verify SBOM is parseable CycloneDX JSON
        run: jq -e '.bomFormat == "CycloneDX"' "paigasus-helikon-${{ github.ref_name }}-cyclonedx.json"

      - name: Upload SBOM to release
        uses: softprops/action-gh-release@v2
        with:
          files: "paigasus-helikon-${{ github.ref_name }}-cyclonedx.json"
          fail_on_unmatched_files: true
          # softprops creates the release if it doesn't exist; otherwise
          # attaches the asset to the existing release for the tag.
```

Design notes:

- **`-p paigasus-helikon --all-features`** rather than `--workspace`: cargo-cyclonedx's workspace mode produces one SBOM per member crate, not an aggregate. The facade re-exports `paigasus-helikon-core` unconditionally and every sibling crate behind features; with `--all-features` its dep graph equals the workspace dep graph.
- **`jq -e '.bomFormat == "CycloneDX"'`** is a cheap in-job gate for the ticket's "parseable CycloneDX file" acceptance criterion; the smoketest in §6.3 re-runs it against the uploaded release asset.
- **`cancel-in-progress: false`** on the SBOM concurrency group — tags are immutable, so cancelling an in-flight run could leave a release in a half-uploaded state.
- **cargo-cyclonedx output filename** has shifted across versions; the `find` step accommodates both `bom.cdx.json` (recent) and `bom.json` (older). The implementation plan pins a version; the smoketest tag push locks the contract.

## 5. Dependabot + GitHub UI settings

### 5.1 `.github/dependabot.yml`

```yaml
version: 2
updates:
  - package-ecosystem: cargo
    directory: "/"
    schedule:
      interval: weekly
      day: monday
      time: "06:00"
      timezone: "Etc/UTC"
    open-pull-requests-limit: 10
    groups:
      cargo-minor-and-patch:
        update-types: ["minor", "patch"]
    commit-message:
      prefix: "chore(deps)"
      include: "scope"
    labels: ["area:deps"]

  - package-ecosystem: github-actions
    directory: "/"
    schedule:
      interval: weekly
      day: monday
      time: "06:00"
      timezone: "Etc/UTC"
    open-pull-requests-limit: 5
    groups:
      actions-minor-and-patch:
        update-types: ["minor", "patch"]
    commit-message:
      prefix: "chore(ci)"
      include: "scope"
    labels: ["area:ci"]
```

Design notes:

- **`directory: "/"`** for the Cargo ecosystem covers the workspace root; Dependabot has understood `[workspace.dependencies]` since 2023 and proposes bumps centrally.
- **`directory: "/"`** for github-actions watches every `.github/workflows/*.yml` from one entry; no need to enumerate workflows.
- **Grouped minor + patch** lands as one PR per ecosystem per week — minimizes noise while ensuring CVE-adjacent bumps land within ~7 days. **Major bumps remain ungrouped** so a breaking change is reviewed in isolation.
- **`commit-message.prefix`** matches the `<type>(<scope>):` convention from CONTRIBUTING.md. Dependabot PRs have no SMA-### (no Linear ticket); SMA-335's PR-title lint will be configured to skip the `dependabot[bot]` author.
- **`labels`** align with the existing Linear taxonomy (`area:deps`, `area:ci`).
- **Monday 06:00 UTC** intentionally aligns with the daily `audit` cron (also 06:00 UTC) so Monday-morning churn concentrates rather than smears.

### 5.2 GitHub UI settings (one-time, outside the bootstrap PR)

Repo settings, not file-config. For a public repo all five are free (no GHAS license).

| Setting | Path | Default on public repo | Action |
|---|---|---|---|
| Dependency graph | Settings → Code security & analysis | Enabled | Verify enabled |
| Dependabot alerts | Settings → Code security & analysis | Off | **Enable** |
| Dependabot security updates | Settings → Code security & analysis | Off | **Enable** (auto-PRs CVE fixes) |
| Secret scanning | Settings → Code security & analysis | Off | **Enable** |
| Secret scanning — push protection | Settings → Code security & analysis | Off | **Enable** |

Reproducible enablement via `gh` CLI (alternative to the UI):

```bash
gh api -X PATCH /repos/SMK1085/paigasus-helikon \
  -f 'security_and_analysis[secret_scanning][status]=enabled' \
  -f 'security_and_analysis[secret_scanning_push_protection][status]=enabled' \
  -f 'security_and_analysis[dependabot_security_updates][status]=enabled'
```

The PR description for SMA-306 includes a checklist with screenshots or CLI output proving each toggle is on. The spec does not gate the PR on these settings (a workflow file cannot verify a repo setting), but the merge checklist does.

## 6. Verification

### 6.1 Local repro (before requesting review)

```bash
# One-time tool installs (binstall or direct cargo install)
cargo install cargo-audit cargo-deny cargo-cyclonedx
# Or via binstall (matches CI's taiki-e/install-action behavior):
# cargo binstall cargo-audit cargo-deny cargo-cyclonedx

cargo audit --deny warnings
cargo deny --all-features check
cargo cyclonedx --format json --all-features -p paigasus-helikon
jq -e '.bomFormat == "CycloneDX"' crates/paigasus-helikon/*.cdx.json
```

All four must exit 0 locally before pushing. These commands also land in `CONTRIBUTING.md` under a new "Supply-chain security" subsection so contributors have a single source.

### 6.2 PR-time CI verification

On the SMA-306 bootstrap PR:

- `audit / audit` — green (no advisory on current lockfile).
- `deny / deny` — green (license allowlist covers every transitive license; sources are crates.io-only; no banned crates; advisory DB clean).
- `sbom / sbom` — **not present** in the PR's check run list (tag-only trigger). Expected; the SBOM contract is validated via §6.3.
- All five pre-existing required checks (`ci / fmt`, `ci / clippy`, `ci / test (ubuntu-latest, stable)`, `ci / docs`, `ci / doc-coverage`) — green.
- `msrv / verify` — green (signal-only).

### 6.3 SBOM smoketest (manual, on the bootstrap PR branch)

The acceptance criterion "a test tag push produces a release asset with a parseable CycloneDX file" needs an actual tag push to exercise `sbom.yml`. Procedure:

```bash
# On feature/sma-306-... branch, after audit + deny are green:
git tag v0.0.0-sma306-smoketest
git push origin v0.0.0-sma306-smoketest

# Wait for sbom workflow to complete.
gh run watch --workflow=sbom --exit-status

# Verify release asset exists and is parseable CycloneDX.
gh release download v0.0.0-sma306-smoketest \
  --pattern 'paigasus-helikon-*-cyclonedx.json' \
  --output sbom-smoketest.json
jq -e '.bomFormat == "CycloneDX" and .specVersion != null' sbom-smoketest.json

# Cleanup — smoketest tag and release are not real releases.
gh release delete v0.0.0-sma306-smoketest --cleanup-tag --yes
rm sbom-smoketest.json
```

The smoketest is recorded in the PR description as a transcript (commands + outputs) so a reviewer can verify the SBOM contract without re-running it.

**Why not automate the smoketest in CI?** Tagging from a workflow requires `contents: write` plus a token with branch-protection-bypass, and synthesizing a tag from every PR would clutter release history. A one-time manual smoketest per workflow change is the cheaper, less-coupled answer.

### 6.4 Scheduled audit verification

The daily `scheduled-audit` job cannot be verified on a PR (it only runs on `schedule` against `main`). After merge, the first daily run (next 06:00 UTC) provides the first signal. If a stale advisory is in the lockfile the day of merge, `rustsec/audit-check` auto-files an issue tagged with the advisory ID — that is the verification.

## 7. Documentation updates

### 7.1 `CONTRIBUTING.md` — append a "Supply-chain security" section

After the existing "Local pre-PR checklist" section, add:

```markdown
## Supply-chain security

Three workflows complement CI and gate PRs alongside the build matrix:

- `audit` — `cargo audit --deny warnings` against the [RustSec Advisory DB](https://rustsec.org/).
  Runs on every PR + push to `main`, plus a daily scheduled run on `main` that
  auto-files a GitHub issue if a new advisory affects the locked deps.
- `deny` — `cargo deny --all-features check` enforces the license allowlist,
  ban list, source registry restrictions, and a second advisory pass. Policy
  lives in `deny.toml` at the workspace root.
- `sbom` — on every `v*` tag push, generates a CycloneDX SBOM via
  `cargo-cyclonedx` and uploads it as a release asset.

Local repro:

    cargo install cargo-audit cargo-deny cargo-cyclonedx   # one-time
    cargo audit --deny warnings
    cargo deny --all-features check
    cargo cyclonedx --format json --all-features -p paigasus-helikon

Adding a new dependency that pulls a license outside the allowlist will fail
`deny`. Either add the license to `deny.toml`'s `[licenses].allow` list (if
permissively compatible) or carve a per-crate exception under
`[licenses].exceptions`. Do **not** lower `confidence-threshold` or add to
`[advisories].ignore` without recording a rationale in the same commit.
```

### 7.2 `CLAUDE.md` — append under the existing "## CI" section

```markdown
Supply-chain workflows (`.github/workflows/audit.yml`, `deny.yml`, `sbom.yml`)
are separate from `ci.yml` because they have independent triggers and failure
semantics. Required status checks added in SMA-306: `audit / audit`,
`deny / deny`. The `audit` workflow has two jobs gated by `github.event_name`:
the PR-time `audit` job uses `taiki-e/install-action` for deterministic
behavior; the daily `scheduled-audit` job uses `rustsec/audit-check@v2` for
its auto-issue-filing behavior on advisory hits — these are the only places
in the repo where a wrapper action is preferred over direct tool invocation.

The SBOM workflow runs `cargo cyclonedx -p paigasus-helikon --all-features`,
not `--workspace`. Workspace mode emits one SBOM per crate; the facade with
all features captures the same dep graph as a single file.

`deny.toml` declares `version = 2` under both `[advisories]` and `[licenses]`
— v1 fields (`vulnerability`, `unmaintained`, `unsound`, `copyleft`, etc.)
are removed in modern cargo-deny and adding them will fail with a schema
error. The license allowlist includes `Unicode-3.0` in addition to the
ticket-prescribed `Unicode-DFS-2016` because `unicode-ident ≥ 1.0.13`
(pulled transitively by `serde_derive`) relicensed in 2024.
```

## 8. Risks & deviations from the ticket

1. **`Unicode-3.0` added to the license allowlist.** The ticket prescribes seven licenses; we add an eighth. `unicode-ident ≥ 1.0.13` shipped a relicense from `Unicode-DFS-2016` to `Unicode-3.0` in 2024, and it is a transitive dep of `serde_derive` (which the workspace will pull as soon as any non-stub crate uses `serde`'s derive macros). Without this entry, the bootstrap PR's `deny / deny` job would fail on a dep we have no quarrel with. The deviation is additive and conservative — both Unicode licenses are FSF-approved permissive terms.

2. **`multiple-versions = "warn"` rather than `"deny"`.** The ticket does not specify this knob. We warn because the workspace's transitive graph will inevitably have duplicate versions (e.g. `syn 1.x` and `syn 2.x` coexist during ecosystem transitions). Tightening to `"deny"` now would force premature dedup work; revisit after Stage-1 lands real implementations and the dep tree stabilizes.

3. **Hybrid action strategy.** The ticket does not specify install method. We use `taiki-e/install-action` for PR-gating jobs (matches `msrv.yml` and is deterministic), and `rustsec/audit-check@v2` only for the daily scheduled run because the auto-issue-filing behavior is the whole point of running on a schedule. Trade-off: one third-party wrapper action in the supply-chain workflows; mitigated by pinning the action version and watching it via the github-actions Dependabot ecosystem.

4. **SBOM via facade crate + `--all-features`, not `--workspace`.** The ticket says "a CycloneDX SBOM" (singular). cargo-cyclonedx's `--workspace` mode produces one SBOM per crate; the facade with all features produces a single SBOM whose dep graph equals the workspace's dep graph. One file is also a cleaner release asset.

5. **Daily audit cron at 06:00 UTC, aligned with Dependabot.** Concentrates Monday-morning churn (Dependabot weekly + first weekday cron run) into a single window rather than smearing across the day. No ticket guidance; design call.

6. **GitHub UI security settings documented but not file-gated.** Dependency graph / Dependabot alerts / secret scanning are repo settings, not files. The PR description includes a checklist verifying each toggle; the workflow files cannot verify these on their own. The `gh api` invocations in §5.2 make the toggles reproducible without manual UI clicks.

7. **`sbom / sbom` is not a required status check.** The tag-only trigger means it never produces a check run on a PR; making it required would block every PR's merge. The SBOM contract is validated on the bootstrap PR via the manual smoketest (§6.3) and afterward on every real `v*` tag.

8. **Manual smoketest, not in-CI tag pushing.** Self-tagging from CI requires bypassing branch protection — not worth the blast radius for what is a one-time-per-workflow-change verification. The smoketest transcript lives in the PR description for review.

9. **No `cargo-vet` and no SBOM-vulnerability correlation.** Ticket does not ask. SBOM is emitted as a passive artifact; downstream tools (e.g. Grype, Trivy) can consume it but are not part of this ticket.

10. **`[advisories].ignore = []` at bootstrap.** No CVE carve-outs at merge time. If the first scheduled run flags an advisory we cannot fix immediately (e.g. waiting on an upstream patch), the resolution is a separate commit adding an `ignore` entry with the CVE ID, a one-line justification, and a TODO for removal.
