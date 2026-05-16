# SMA-306 Supply-chain security: `cargo-audit`, `cargo-deny`, SBOM — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land three new GitHub workflows (`audit.yml`, `deny.yml`, `sbom.yml`), a root-level `deny.toml` policy file, a `.github/dependabot.yml`, and matching documentation updates. The PR produces two new required-status-check IDs for SMA-309 (`audit / audit`, `deny / deny`) and an SBOM smoketest tag-push that proves the release-asset upload path works end-to-end.

**Architecture:** Three separate workflow files (mirrors `ci.yml` / `msrv.yml` split — independent triggers, independent failure semantics, independent required-status-check IDs). The `audit` workflow has two jobs gated by `github.event_name`: PR-time `audit` uses `taiki-e/install-action` + `cargo audit`; the daily 06:00 UTC `scheduled-audit` uses `rustsec/audit-check@v2` for its auto-issue-filing on advisory hits. `deny` and `sbom` use `taiki-e/install-action` exclusively. `deny.toml` uses cargo-deny v2 schema (`version = 2` under `[advisories]` and `[licenses]`). SBOM is generated against the facade crate with `--all-features` (single file = full workspace dep graph), uploaded via `softprops/action-gh-release@v2`.

**Tech Stack:** GitHub Actions, `taiki-e/install-action`, `Swatinem/rust-cache@v2`, `dtolnay/rust-toolchain`, `rustsec/audit-check@v2`, `softprops/action-gh-release@v2`, `cargo-audit`, `cargo-deny` (≥ 0.14, v2 schema), `cargo-cyclonedx` (≥ 0.5, CycloneDX 1.5), `jq`.

**Spec:** [`docs/superpowers/specs/2026-05-16-sma-306-supply-chain-security-design.md`](../specs/2026-05-16-sma-306-supply-chain-security-design.md)

**Linear:** [SMA-306](https://linear.app/smaschek/issue/SMA-306/supply-chain-security-cargo-audit-cargo-deny-sbom)

---

## Definition of Done

The plan is complete when **all** of the following hold:

```bash
# Local
cargo audit --deny warnings                                       # exit 0
cargo deny --all-features check                                   # exit 0
cargo cyclonedx --format json --all-features -p paigasus-helikon  # exit 0
jq -e '.bomFormat == "CycloneDX"' crates/paigasus-helikon/bom.cdx.json   # exit 0

# CI on the feature-branch PR
ci / fmt, ci / clippy, ci / test (ubuntu-latest, stable), ci / docs, ci / doc-coverage   # all green
audit / audit                                                                            # green
deny / deny                                                                              # green
msrv / verify                                                                            # green (signal-only)

# Tag-push smoketest
git tag v0.0.0-sma306-smoketest && git push origin v0.0.0-sma306-smoketest
gh run watch --workflow=sbom --exit-status                        # exit 0
gh release view v0.0.0-sma306-smoketest --json assets             # one *.cyclonedx.json asset present
jq -e '.bomFormat == "CycloneDX"' <downloaded-asset>              # exit 0
# Cleanup: smoketest tag + release removed.

# GitHub repo settings
Dependabot alerts: enabled
Dependabot security updates: enabled
Secret scanning: enabled
Secret scanning push protection: enabled
```

The PR description includes a transcript of the SBOM smoketest commands + outputs.

## Conventions used in this plan

- **Commit prefix**: `<type>(<scope>): SMA-306 <message>`. Examples: `ci(workflows): SMA-306 add audit workflow`, `chore(deps): SMA-306 add dependabot config`, `docs(contributing): SMA-306 supply-chain section`.
- **Branch**: all work happens on `feature/sma-306-supply-chain-security-cargo-audit-cargo-deny-sbom`.
- **No remote pushes** until Task 13 (push to open PR) and Task 15 (tag push for SBOM smoketest). Both are explicit user-driven steps; the executing agent surfaces the command and waits.
- **License allowlist hits**: if `cargo deny check` rejects a license not in the spec's allowlist (Apache-2.0, MIT, BSD-2-Clause, BSD-3-Clause, ISC, Unicode-DFS-2016, Unicode-3.0, MPL-2.0), do **not** silently add it. Stop, capture the offending `(crate, version, license)` tuple in the PR description, and decide: (a) add to allowlist if permissively compatible, (b) carve a per-crate `[licenses].exceptions` entry with justification, or (c) replace the dep.
- **Advisory hits**: if `cargo audit` or `cargo deny check advisories` reports a vulnerability, do **not** add an `[advisories].ignore` entry to make CI green. Fix the dep (bump version) or document the decision to wait in a separate commit with the CVE ID and a removal TODO.
- **Tag cleanup**: the SBOM smoketest tag `v0.0.0-sma306-smoketest` is **not a real release**. Task 15 deletes both the tag and the release. Do not leave it lying around.
- **Linear ticket status**: per workspace convention, do **not** auto-close SMA-306 from PR merge — the user moves status manually after review.

---

## File Structure

**Created:**
- `.github/workflows/audit.yml` — two jobs (`audit`, `scheduled-audit`) gated by `github.event_name`
- `.github/workflows/deny.yml` — single `deny` job
- `.github/workflows/sbom.yml` — single `sbom` job, triggered on `v*` tag push
- `.github/dependabot.yml` — `cargo` + `github-actions` ecosystems, weekly, grouped patch+minor
- `deny.toml` — workspace-root cargo-deny policy

**Modified:**
- `CONTRIBUTING.md` — append "Supply-chain security" section
- `CLAUDE.md` — append a short supply-chain note under the existing "## CI" section

**Untouched:** `Cargo.toml`, all `crates/**`, `scripts/`, `rust-toolchain.toml`, existing workflows (`ci.yml`, `msrv.yml`).

---

### Task 1: Switch to the feature branch

**Files:** none (git operation only).

- [ ] **Step 1: Verify current branch is `main` and clean**

Run:
```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon status --short --branch
```

Expected: `## main...origin/main` and no working-tree changes (the SMA-306 spec was committed in `8e75abd`).

- [ ] **Step 2: Create and check out the feature branch**

Run:
```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon checkout -b feature/sma-306-supply-chain-security-cargo-audit-cargo-deny-sbom
```

Expected: `Switched to a new branch 'feature/sma-306-supply-chain-security-cargo-audit-cargo-deny-sbom'`.

- [ ] **Step 3: Confirm**

Run:
```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon rev-parse --abbrev-ref HEAD
```

Expected: `feature/sma-306-supply-chain-security-cargo-audit-cargo-deny-sbom`.

No commit in this task.

---

### Task 2: Install the three tools locally (one-time)

**Files:** none (toolchain operation).

These three binaries must be on `$PATH` for the local verification tasks (4, 7, 9). CI installs them itself via `taiki-e/install-action`; this task is for the contributor's machine.

- [ ] **Step 1: Check whether the tools are already installed**

Run:
```bash
command -v cargo-audit && command -v cargo-deny && command -v cargo-cyclonedx
```

If all three print paths and exit 0, skip to Step 3.

- [ ] **Step 2: Install whichever are missing**

Pick one of:

```bash
# Option A — cargo binstall (faster, fetches prebuilt binaries):
cargo binstall cargo-audit cargo-deny cargo-cyclonedx

# Option B — source builds (slower, no extra tooling):
cargo install cargo-audit cargo-deny cargo-cyclonedx
```

Either takes 1–10 minutes the first time.

- [ ] **Step 3: Verify versions are recent enough**

Run:
```bash
cargo audit --version
cargo deny --version
cargo cyclonedx --version
```

Expected:
- `cargo-audit ≥ 0.20`
- `cargo-deny ≥ 0.14` (v2 schema required)
- `cargo-cyclonedx ≥ 0.5` (CycloneDX 1.5 output, `bom.cdx.json` filenames)

If `cargo-deny` is older than 0.14, the `version = 2` declarations in Task 3 will fail to parse. Re-run Step 2 with `cargo install --force cargo-deny`.

No commit in this task.

---

### Task 3: Create `deny.toml`

**Files:**
- Create: `/Users/smaschek/dev/paigasus/paigasus-helikon/deny.toml`

- [ ] **Step 1: Write `deny.toml`**

Create `/Users/smaschek/dev/paigasus/paigasus-helikon/deny.toml` with this exact content:

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

# ---------------------------------------------------------------------------
# Licenses — ticket allowlist plus Unicode-3.0 (the rebrand of
# Unicode-DFS-2016 used by unicode-ident >= 1.0.13).
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

# ---------------------------------------------------------------------------
# Bans — empty allow/deny lists; `wildcards = "deny"` enforces precise
# version constraints, `multiple-versions = "warn"` because transitive dupes
# are unavoidable while the workspace is small.
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

- [ ] **Step 2: Verify the file is well-formed TOML**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && cargo deny --version >/dev/null && \
  cargo deny --offline check --config deny.toml --show-stats 2>&1 | head -20
```

Expected: cargo-deny may emit warnings about advisory DB not being fetched (because `--offline`); what matters is that it does **not** fail with a TOML parse error or a schema error like "field `vulnerability` not found". If you see "config validated" or it proceeds to the actual check phase, the TOML is well-formed.

If you see `error parsing config: missing field 'version'`, you're on cargo-deny < 0.14 — return to Task 2 Step 2 and update.

- [ ] **Step 3: Commit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && git add deny.toml && git commit -m "chore(deps): SMA-306 add deny.toml cargo-deny policy

License allowlist matches the SMA-306 ticket plus Unicode-3.0 (transitive
via unicode-ident >= 1.0.13). Advisories: yanked = deny, ignore = empty.
Sources: crates.io only; no git deps. Bans: wildcards denied,
multiple-versions warn (tighten post-Stage-1)."
```

---

### Task 4: Verify `cargo deny check` passes locally

**Files:** none (verification only).

- [ ] **Step 1: Run the full check**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && cargo deny --all-features check
```

Expected: one of
- `all checks passed` (exit 0) — proceed.
- Failure on a license, advisory, source, or ban — **stop** and follow the resolution procedure in the "Conventions" section of this plan (license / advisory hits). Resolve in a follow-up commit before continuing.

The current lockfile contains the 13 internal workspace crates plus whatever direct + transitive deps are actually used. Stub crates with empty `[dependencies]` tables contribute nothing to the lockfile, so the check is expected to be trivially clean at bootstrap.

- [ ] **Step 2: Confirm advisory DB clone path**

Run:
```bash
ls -la ~/.cargo/advisory-dbs 2>&1 | head -5
```

Expected: a directory exists with a clone of the RustSec advisory DB. cargo-deny created this on Step 1's run.

No commit in this task.

---

### Task 5: Create `.github/workflows/deny.yml`

**Files:**
- Create: `/Users/smaschek/dev/paigasus/paigasus-helikon/.github/workflows/deny.yml`

- [ ] **Step 1: Write the workflow**

Create `/Users/smaschek/dev/paigasus/paigasus-helikon/.github/workflows/deny.yml` with this exact content:

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

- [ ] **Step 2: Verify YAML is well-formed**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/deny.yml')); print('OK')"
```

Expected: `OK`.

- [ ] **Step 3: Commit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && git add .github/workflows/deny.yml && git commit -m "ci(workflows): SMA-306 add cargo-deny workflow

Runs cargo deny --all-features check on PR + push to main.
Produces required-status-check id: deny / deny."
```

---

### Task 6: Create `.github/workflows/audit.yml`

**Files:**
- Create: `/Users/smaschek/dev/paigasus/paigasus-helikon/.github/workflows/audit.yml`

- [ ] **Step 1: Write the workflow**

Create `/Users/smaschek/dev/paigasus/paigasus-helikon/.github/workflows/audit.yml` with this exact content:

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

- [ ] **Step 2: Verify YAML is well-formed**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/audit.yml')); print('OK')"
```

Expected: `OK`.

- [ ] **Step 3: Commit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && git add .github/workflows/audit.yml && git commit -m "ci(workflows): SMA-306 add cargo-audit workflow

Two jobs gated by github.event_name:
- audit (PR + push to main): taiki-e/install-action + cargo audit
  --deny warnings. Produces required-status-check id: audit / audit.
- scheduled-audit (daily 06:00 UTC): rustsec/audit-check@v2, auto-files
  an issue on advisory hits (issues: write scoped to this job only)."
```

---

### Task 7: Verify `cargo audit --deny warnings` passes locally

**Files:** none (verification only).

- [ ] **Step 1: Run cargo audit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && cargo audit --deny warnings
```

Expected: one of
- `Crate:  ...` lines for each scanned dep, ending with `No vulnerable packages found` (exit 0) — proceed.
- Network error fetching advisory DB on first run — re-run; cargo-audit caches the DB to `~/.cargo/advisory-db` after the first successful fetch.
- An advisory hit — **stop** and follow the resolution procedure in the "Conventions" section. Resolve in a follow-up commit before continuing.

- [ ] **Step 2: Confirm advisory DB clone path**

Run:
```bash
ls -la ~/.cargo/advisory-db 2>&1 | head -5
```

Expected: a directory exists with a clone of the RustSec advisory DB. Note this is `advisory-db` (singular), distinct from cargo-deny's `advisory-dbs` (plural) in Task 4 — each tool caches independently.

No commit in this task.

---

### Task 8: Create `.github/workflows/sbom.yml`

**Files:**
- Create: `/Users/smaschek/dev/paigasus/paigasus-helikon/.github/workflows/sbom.yml`

- [ ] **Step 1: Write the workflow**

Create `/Users/smaschek/dev/paigasus/paigasus-helikon/.github/workflows/sbom.yml` with this exact content:

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
          # cargo-cyclonedx >= 0.5 writes bom.cdx.json under the package dir.
          # Locate and rename for the release asset (be defensive about
          # filename in case cargo-cyclonedx changes the default again).
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
```

- [ ] **Step 2: Verify YAML is well-formed**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/sbom.yml')); print('OK')"
```

Expected: `OK`.

- [ ] **Step 3: Commit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && git add .github/workflows/sbom.yml && git commit -m "ci(workflows): SMA-306 add CycloneDX SBOM workflow

Triggers on v* tag push. Builds a CycloneDX 1.5 JSON SBOM via
cargo-cyclonedx against the facade crate with --all-features (single
file = full workspace dep graph), verifies it parses, and uploads it
as a release asset via softprops/action-gh-release@v2.

Not a required status check (tag-only trigger; never runs on PRs).
Validated end-to-end via the SMA-306 smoketest tag push."
```

---

### Task 9: Verify cargo-cyclonedx generates a parseable SBOM locally

**Files:** none (verification only). This task is the local equivalent of the workflow's first three job steps.

- [ ] **Step 1: Generate the SBOM**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && cargo cyclonedx --format json --all-features -p paigasus-helikon
```

Expected: exit 0, with output ending in something like `Wrote bom to crates/paigasus-helikon/bom.cdx.json` (exact wording depends on cargo-cyclonedx version).

- [ ] **Step 2: Locate the output file**

Run:
```bash
find /Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon -maxdepth 1 -name '*.cdx.json'
```

Expected: one file path printed, ending in `.cdx.json`. If multiple files print, note all paths — the workflow takes the first via `head -n1`; if there is more than one, investigate.

- [ ] **Step 3: Verify the SBOM is parseable CycloneDX**

Substitute `<path>` with the path from Step 2:

```bash
jq -e '.bomFormat == "CycloneDX" and .specVersion != null' <path>
```

Expected: exit 0 and prints `true`.

- [ ] **Step 4: Clean up the local SBOM artifact**

Run:
```bash
find /Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon -maxdepth 1 -name '*.cdx.json' -delete
```

Expected: silent exit 0. The SBOM is a build artifact; we don't commit it. Verify it's gone:

```bash
find /Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon -maxdepth 1 -name '*.cdx.json'
```

Expected: no output.

No commit in this task. (The `.gitignore` already covers `target/` but not crate-root `bom.cdx.json` — if you want a defensive add, that's a separate small commit at the end of this task: `echo 'crates/**/bom.cdx.json' >> .gitignore && git add .gitignore && git commit -m "chore(workspace): SMA-306 gitignore cargo-cyclonedx output"`.)

---

### Task 10: Create `.github/dependabot.yml`

**Files:**
- Create: `/Users/smaschek/dev/paigasus/paigasus-helikon/.github/dependabot.yml`

- [ ] **Step 1: Write the config**

Create `/Users/smaschek/dev/paigasus/paigasus-helikon/.github/dependabot.yml` with this exact content:

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

- [ ] **Step 2: Verify YAML is well-formed**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && python3 -c "import yaml,sys; yaml.safe_load(open('.github/dependabot.yml')); print('OK')"
```

Expected: `OK`.

- [ ] **Step 3: Commit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && git add .github/dependabot.yml && git commit -m "chore(deps): SMA-306 add Dependabot config

Cargo + github-actions ecosystems, weekly Monday 06:00 UTC (aligned
with the daily audit cron). Patch + minor updates grouped into one
PR per ecosystem; major bumps remain ungrouped. Labels match the
Linear taxonomy: area:deps for cargo, area:ci for github-actions."
```

---

### Task 11: Update `CONTRIBUTING.md`

**Files:**
- Modify: `/Users/smaschek/dev/paigasus/paigasus-helikon/CONTRIBUTING.md` (append a new section after "## Local pre-PR checklist")

- [ ] **Step 1: Read the current end of the file**

Run:
```bash
tail -20 /Users/smaschek/dev/paigasus/paigasus-helikon/CONTRIBUTING.md
```

Confirm the last section is `## Local pre-PR checklist` and the file ends with the `cargo-msrv` block.

- [ ] **Step 2: Append the supply-chain section**

Append this content to `/Users/smaschek/dev/paigasus/paigasus-helikon/CONTRIBUTING.md`:

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

```bash
cargo install cargo-audit cargo-deny cargo-cyclonedx   # one-time
cargo audit --deny warnings
cargo deny --all-features check
cargo cyclonedx --format json --all-features -p paigasus-helikon
```

Adding a new dependency that pulls a license outside the allowlist will fail
`deny`. Either add the license to `deny.toml`'s `[licenses].allow` list (if
permissively compatible) or carve a per-crate exception under
`[licenses].exceptions`. Do **not** lower `confidence-threshold` or add to
`[advisories].ignore` without recording a rationale in the same commit.

Dependabot watches `cargo` and `github-actions` weekly (Monday 06:00 UTC),
grouping patch + minor updates per ecosystem. Major bumps remain ungrouped
so breaking changes are reviewed in isolation.
```

- [ ] **Step 3: Verify the file still renders sensibly**

Run:
```bash
wc -l /Users/smaschek/dev/paigasus/paigasus-helikon/CONTRIBUTING.md
```

Expected: ~120 lines (was 92, added ~28 lines).

Run:
```bash
grep -n '^## ' /Users/smaschek/dev/paigasus/paigasus-helikon/CONTRIBUTING.md
```

Expected: section headings appear in order — `Branch naming`, `Commit messages`, `MSRV`, `Docstring coverage`, `Local pre-PR checklist`, `Supply-chain security`.

- [ ] **Step 4: Commit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && git add CONTRIBUTING.md && git commit -m "docs(contributing): SMA-306 add Supply-chain security section

Documents the audit / deny / sbom workflows, the local repro
commands, the license-allowlist resolution procedure, and
Dependabot's weekly schedule."
```

---

### Task 12: Update `CLAUDE.md`

**Files:**
- Modify: `/Users/smaschek/dev/paigasus/paigasus-helikon/CLAUDE.md` (append a paragraph to the existing "## CI" section)

- [ ] **Step 1: Locate the CI section**

Run:
```bash
grep -n '^## CI' /Users/smaschek/dev/paigasus/paigasus-helikon/CLAUDE.md
```

Expected: one line number printed (e.g. `97:## CI`).

- [ ] **Step 2: Read the current CI section so the append lands sensibly**

Run:
```bash
awk '/^## CI/,/^## /{print}' /Users/smaschek/dev/paigasus/paigasus-helikon/CLAUDE.md | tail -10
```

Confirm the last paragraph of the CI section ends with the `permissions: contents: read` line and is followed by the next `## Cargo.lock` heading (or end-of-file).

- [ ] **Step 3: Insert the supply-chain paragraphs**

Insert this content **before** the `## Cargo.lock` heading (i.e. at the end of the `## CI` section). Use the Edit tool to insert it between the existing last paragraph of `## CI` and the `## Cargo.lock` heading.

```markdown

Supply-chain workflows (`.github/workflows/audit.yml`, `deny.yml`, `sbom.yml`) are separate from `ci.yml` because they have independent triggers and failure semantics. Required status checks added in SMA-306: `audit / audit`, `deny / deny`. The `audit` workflow has two jobs gated by `github.event_name`: the PR-time `audit` job uses `taiki-e/install-action` for deterministic behavior; the daily `scheduled-audit` job uses `rustsec/audit-check@v2` for its auto-issue-filing behavior on advisory hits — these are the only places in the repo where a wrapper action is preferred over direct tool invocation.

The SBOM workflow runs `cargo cyclonedx -p paigasus-helikon --all-features`, not `--workspace`. Workspace mode emits one SBOM per crate; the facade with all features captures the same dep graph as a single file.

`deny.toml` declares `version = 2` under both `[advisories]` and `[licenses]` — v1 fields (`vulnerability`, `unmaintained`, `unsound`, `copyleft`, etc.) are removed in modern cargo-deny and adding them will fail with a schema error. The license allowlist includes `Unicode-3.0` in addition to the ticket-prescribed `Unicode-DFS-2016` because `unicode-ident ≥ 1.0.13` (pulled transitively by `serde_derive`) relicensed in 2024.

Dependabot is configured for `cargo` + `github-actions` ecosystems, weekly Monday 06:00 UTC, with patch + minor updates grouped into one PR per ecosystem.
```

- [ ] **Step 4: Verify section order**

Run:
```bash
grep -n '^## ' /Users/smaschek/dev/paigasus/paigasus-helikon/CLAUDE.md
```

Expected: headings appear in their original order; `## CI` is still followed by `## Cargo.lock`. No new top-level headings added.

- [ ] **Step 5: Commit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && git add CLAUDE.md && git commit -m "docs: SMA-306 document supply-chain workflows in CLAUDE.md

Adds a paragraph to the CI section explaining why audit/deny/sbom
are separate workflows, the hybrid install strategy, the facade-crate
SBOM rationale, the cargo-deny v2 schema requirement, the Unicode-3.0
allowlist addition, and the Dependabot config."
```

---

### Task 13: Push the feature branch and open the PR

**Files:** none (git + GitHub operation).

- [ ] **Step 1: Confirm commit log on the feature branch**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && git log --oneline main..HEAD
```

Expected: 7 commits (one per Task 3, 5, 6, 8, 10, 11, 12). If you ran the optional `.gitignore` commit at the end of Task 9, expect 8 commits.

- [ ] **Step 2: Push the branch to origin**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && git push -u origin feature/sma-306-supply-chain-security-cargo-audit-cargo-deny-sbom
```

Expected: branch is pushed; `gh` prints a "Create a pull request" hint URL.

- [ ] **Step 3: Open the PR**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && gh pr create \
  --base main \
  --head feature/sma-306-supply-chain-security-cargo-audit-cargo-deny-sbom \
  --title "ci: SMA-306 supply-chain security (audit, deny, SBOM)" \
  --body "$(cat <<'EOF'
## Summary

- Adds three workflows: \`audit\` (PR + daily on main, hybrid install), \`deny\` (PR + push), \`sbom\` (v* tag push)
- Adds \`deny.toml\` with the ticket's license allowlist plus \`Unicode-3.0\` (transitive via \`unicode-ident\`)
- Adds \`.github/dependabot.yml\` (cargo + github-actions, weekly Monday 06:00 UTC, grouped patch+minor)
- Adds Supply-chain section to \`CONTRIBUTING.md\` and a supply-chain note to \`CLAUDE.md\`
- Produces two new required-status-check IDs for SMA-309: \`audit / audit\`, \`deny / deny\`

Design spec: [\`docs/superpowers/specs/2026-05-16-sma-306-supply-chain-security-design.md\`](../blob/main/docs/superpowers/specs/2026-05-16-sma-306-supply-chain-security-design.md)
Linear: [SMA-306](https://linear.app/smaschek/issue/SMA-306/supply-chain-security-cargo-audit-cargo-deny-sbom)

## Test plan

- [ ] \`audit / audit\` green on this PR
- [ ] \`deny / deny\` green on this PR
- [ ] Pre-existing required checks still green (\`ci / fmt\`, \`ci / clippy\`, \`ci / test (ubuntu-latest, stable)\`, \`ci / docs\`, \`ci / doc-coverage\`)
- [ ] \`msrv / verify\` green (signal-only)
- [ ] SBOM smoketest: tag \`v0.0.0-sma306-smoketest\` push → \`sbom\` workflow green → release asset present → \`jq\` validates CycloneDX → smoketest tag + release cleaned up. Transcript pasted below after verification.
- [ ] GitHub UI security toggles verified: Dependency graph ✓, Dependabot alerts ✓, Dependabot security updates ✓, Secret scanning ✓, Secret scanning push protection ✓.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: gh prints the PR URL.

- [ ] **Step 4: Record the PR URL for follow-up tasks**

Note the PR URL. You will return to this PR to (a) paste the SBOM smoketest transcript (Task 15) and (b) record the security-toggles checklist outcome (Task 16).

No commit in this task.

---

### Task 14: Wait for `audit / audit` and `deny / deny` to go green on the PR

**Files:** none (CI observation).

- [ ] **Step 1: Watch the PR's check runs**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && gh pr checks --watch
```

Expected: all check rows complete. Required ones must be green:
- `ci / fmt`
- `ci / clippy`
- `ci / test (ubuntu-latest, stable)` (other matrix rows are signal-only)
- `ci / docs`
- `ci / doc-coverage`
- `audit / audit`
- `deny / deny`

`msrv / verify` should also be green (signal-only).
`sbom / sbom` will **not** appear (tag-only trigger).

- [ ] **Step 2: If `audit / audit` fails**

Cause is almost always a new advisory landed in the RustSec DB. Read the failure log via `gh run view --log-failed`; the advisory ID is named (e.g. `RUSTSEC-2026-NNNN`). Resolution: bump the affected dep on `main` first (or wait for upstream patch), rebase the branch, re-push. **Do not** add an `[advisories].ignore` entry without a separate justified commit per the Conventions section.

- [ ] **Step 3: If `deny / deny` fails on a license**

Read the failure log; cargo-deny names the offending `(crate, version, license)` tuple. Resolution: open `deny.toml`, decide whether the license is permissive enough to add to `[licenses].allow` (additive commit) or whether to carve a per-crate exception under `[licenses].exceptions`. Commit the change with a `chore(deps): SMA-306 allow <license> for <crate>` message, rebase if needed, re-push.

- [ ] **Step 4: If `deny / deny` fails on a source or ban**

Same procedure as Step 3 but the failure mode is different: a source failure means a transitive dep is from a git or unknown registry (resolution: replace the dep, or extend `[sources].allow-git` with a one-line justification commit). A ban failure means a dep matches a wildcard or banned crate (resolution: tighten the version constraint).

No commit in this task (any fixes go into commits handled by Steps 2-4 above).

---

### Task 15: SBOM smoketest — tag push, verify, cleanup

**Files:** none (git + GitHub operation). This is the manual verification for the "test tag push produces a release asset with a parseable CycloneDX file" acceptance criterion.

⚠️ **This task pushes a tag to origin and creates a GitHub release.** Both are deleted at the end of the task. Do not skip the cleanup step.

- [ ] **Step 1: Confirm the PR is green except for the SBOM smoketest**

Tasks 13 and 14 must be done. The PR's `audit / audit` and `deny / deny` checks must be green before tagging.

- [ ] **Step 2: Push the smoketest tag**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && git tag v0.0.0-sma306-smoketest && git push origin v0.0.0-sma306-smoketest
```

Expected: tag is pushed.

- [ ] **Step 3: Watch the `sbom` workflow run**

Run:
```bash
gh run watch --workflow=sbom --exit-status
```

Expected: workflow run completes successfully (exit 0). Note the run ID for later reference.

If it fails: read the log via `gh run view --log-failed`. Common failures:
- `find ... | head -n1` returned empty → cargo-cyclonedx output filename differs from `*.cdx.json`. Update the workflow's `find` pattern, re-run from Task 8.
- `jq -e '.bomFormat == "CycloneDX"'` returned non-zero → the SBOM is malformed. Inspect the artifact via `gh run download <run-id> --name <artifact>` if available, or re-run locally (Task 9) to compare.
- `softprops/action-gh-release@v2` 403 → `permissions: contents: write` is missing. Re-check `sbom.yml`.

- [ ] **Step 4: Verify the release asset**

Run:
```bash
gh release view v0.0.0-sma306-smoketest --json assets --jq '.assets[].name'
```

Expected: one filename printed matching `paigasus-helikon-v0.0.0-sma306-smoketest-cyclonedx.json`.

- [ ] **Step 5: Download and validate the asset**

Run:
```bash
gh release download v0.0.0-sma306-smoketest --pattern 'paigasus-helikon-*-cyclonedx.json' --output /tmp/sbom-smoketest.json && \
  jq -e '.bomFormat == "CycloneDX" and .specVersion != null' /tmp/sbom-smoketest.json
```

Expected: exit 0, prints `true`.

- [ ] **Step 6: Capture the transcript for the PR description**

Save the output of steps 2-5 (commands + outputs) and append them as a comment on the PR via `gh pr comment <PR#> --body-file <file>`. This is the evidence the reviewer reads instead of re-running the smoketest.

- [ ] **Step 7: Clean up the smoketest tag and release**

⚠️ **Do not skip this step.** The smoketest is not a real release.

Run:
```bash
gh release delete v0.0.0-sma306-smoketest --cleanup-tag --yes
rm -f /tmp/sbom-smoketest.json
```

Expected: both the release and the tag (`--cleanup-tag`) are deleted. Verify:

```bash
gh release view v0.0.0-sma306-smoketest 2>&1 | head -3
git ls-remote --tags origin | grep sma306-smoketest
```

Expected: first command prints `release not found`. Second command prints nothing.

If the local tag still exists, also run:

```bash
git tag -d v0.0.0-sma306-smoketest 2>&1 | head -3
```

No commit in this task.

---

### Task 16: Enable GitHub repo security settings

**Files:** none (GitHub UI / API operation). These are one-time toggles per repo; they cannot be expressed as files in this branch.

- [ ] **Step 1: Verify Dependency graph is enabled**

Public repos have it on by default. Verify via the UI at `https://github.com/SMK1085/paigasus-helikon/settings/security_analysis`. Look for "Dependency graph" — should say "Enabled".

If not enabled, click "Enable".

- [ ] **Step 2: Enable Dependabot alerts**

Same settings page. Find "Dependabot alerts" → click "Enable" if not already on.

- [ ] **Step 3: Enable Dependabot security updates**

Same settings page. Find "Dependabot security updates" → click "Enable". This is what causes Dependabot to auto-PR CVE fixes (independent of the weekly schedule in `.github/dependabot.yml`).

- [ ] **Step 4: Enable Secret scanning + push protection**

Same settings page. Find "Secret scanning" → "Enable". Then find "Push protection" (the sub-toggle) → "Enable".

- [ ] **Step 5: Verify all four toggles via the API**

Run:
```bash
gh api /repos/SMK1085/paigasus-helikon --jq '{
  dependabot_alerts: .security_and_analysis.dependabot_security_updates.status,
  secret_scanning: .security_and_analysis.secret_scanning.status,
  secret_scanning_push_protection: .security_and_analysis.secret_scanning_push_protection.status
}'
```

Expected: all three values should be `"enabled"`. (The `dependabot_alerts` field name in the API is `dependabot_security_updates`; the alerts toggle is implicit in the dependency-graph + alerts pair on public repos.)

If any field reports `"disabled"`, return to the corresponding step above and click the toggle.

- [ ] **Step 6: Paste the API output into the PR**

Run:
```bash
gh api /repos/SMK1085/paigasus-helikon --jq '{dependabot_security_updates: .security_and_analysis.dependabot_security_updates.status, secret_scanning: .security_and_analysis.secret_scanning.status, secret_scanning_push_protection: .security_and_analysis.secret_scanning_push_protection.status}' \
  | gh pr comment --body-file -
```

Expected: a comment lands on the PR showing the three `"enabled"` lines.

No commit in this task.

---

### Task 17: Final check and hand-off

**Files:** none.

- [ ] **Step 1: Confirm all required checks are green**

Run:
```bash
gh pr checks
```

Expected: all required and signal checks green, no failures.

- [ ] **Step 2: Confirm SBOM smoketest transcript and security-toggles output are on the PR**

Run:
```bash
gh pr view --json comments --jq '.comments[].body' | head -50
```

Expected: at least two comments present — the SBOM smoketest transcript from Task 15 and the security-toggles output from Task 16.

- [ ] **Step 3: Re-check the local Definition of Done**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  cargo audit --deny warnings && \
  cargo deny --all-features check && \
  cargo cyclonedx --format json --all-features -p paigasus-helikon && \
  find crates/paigasus-helikon -maxdepth 1 -name '*.cdx.json' -exec jq -e '.bomFormat == "CycloneDX"' {} \;
```

Expected: all four exit 0.

- [ ] **Step 4: Clean up the local SBOM artifact one more time**

Run:
```bash
find /Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon -maxdepth 1 -name '*.cdx.json' -delete
```

- [ ] **Step 5: Hand off to the user**

Output to the user:

> SMA-306 implementation complete. PR is open with all required checks green, SBOM smoketest transcript pasted to the PR, security toggles enabled and verified. Branch protection (SMA-309) can now reference `audit / audit` and `deny / deny` as required status checks. Move SMA-306 to "In Review" manually in Linear — auto-close is disabled by workspace convention.

No commit in this task. The user reviews + merges the PR themselves.

---

## Self-Review (filled in during plan authoring)

**Spec coverage:**

- §1 Goal / non-goals → Definition of Done + Task 17 hand-off.
- §2 File layout → File Structure section + Tasks 3, 5, 6, 8, 10, 11, 12.
- §3 `deny.toml` → Task 3 (exact content).
- §4.1 `audit.yml` → Task 6 (exact content) + Task 7 (local verification).
- §4.2 `deny.yml` → Task 5 (exact content) + Task 4 (local verification).
- §4.3 `sbom.yml` → Task 8 (exact content) + Task 9 (local verification) + Task 15 (smoketest).
- §5.1 `dependabot.yml` → Task 10 (exact content).
- §5.2 GitHub UI settings → Task 16 (UI + API verification).
- §6.1 Local repro → Tasks 4, 7, 9.
- §6.2 PR-time CI verification → Task 14.
- §6.3 SBOM smoketest → Task 15.
- §6.4 Scheduled audit verification → noted in Conventions; cannot be exercised pre-merge.
- §7.1 CONTRIBUTING.md → Task 11 (exact content).
- §7.2 CLAUDE.md → Task 12 (exact content).
- §8 Risks & deviations → reflected in plan Conventions (license/advisory resolution procedure) and inline comments in `deny.toml` (Task 3).

**Placeholder scan:** no `TBD`, `TODO`, "implement later", "similar to Task N", or unstated code blocks. Every workflow body and every config file is written out in full.

**Type / identifier consistency:**
- Branch name `feature/sma-306-supply-chain-security-cargo-audit-cargo-deny-sbom` is identical across Tasks 1, 13, and the PR title.
- Tag name `v0.0.0-sma306-smoketest` is identical across Task 15 Steps 2, 4, 5, 7.
- Required-check IDs `audit / audit` and `deny / deny` are identical across the spec, Tasks 5/6, the PR body, Task 14.
- Cache directory paths: audit uses `~/.cargo/advisory-db` (singular) consistently in Tasks 6 + 7; deny uses `~/.cargo/advisory-dbs` (plural) consistently in Tasks 3 + 4 + 5.
