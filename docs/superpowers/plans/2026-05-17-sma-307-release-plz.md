# SMA-307 Automated versioning with release-plz — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire release-plz into the workspace so a Conventional-Commits-style commit on `main` produces a release PR that bumps the affected crates and updates per-crate `CHANGELOG.md`. The bootstrap PR adds `release-plz.toml`, `.github/workflows/release-plz.yml`, breaks `version`-inheritance into per-crate lines, and documents the flow. Workspace-wide `publish = false` keeps crates.io publishing off until Stage 1 lifts it.

**Architecture:** Single `release-plz.toml` at workspace root + single `release-plz.yml` workflow with two sequentially-ordered jobs (`release-plz-release` then `release-plz-pr` via `needs:`). Auth is via the release-plz GitHub App (token minted by `actions/create-github-app-token@v1`) so release PRs trigger downstream CI/audit/deny workflows — `GITHUB_TOKEN` would silently suppress them under GitHub's anti-recursion safety. Per-crate `version = "0.0.0"` in every `crates/*/Cargo.toml` replaces `version.workspace = true`; all other inheritance stays. The workspace-level `[workspace.package].version = "0.0.0"` default is kept as a safety net for new crates.

**Tech Stack:** GitHub Actions, `release-plz/action@v0.5`, `actions/create-github-app-token@v1`, `actions/checkout@v6`, `dtolnay/rust-toolchain`, Cargo workspace inheritance, Conventional Commits.

**Spec:** [`docs/superpowers/specs/2026-05-17-sma-307-release-plz-design.md`](../specs/2026-05-17-sma-307-release-plz-design.md)

**Linear:** [SMA-307](https://linear.app/smaschek/issue/SMA-307/automated-versioning-with-release-plz)

---

## Definition of Done

The plan is complete when **all** of the following hold:

```bash
# Local — workspace still builds with per-crate versions
cargo build --workspace --all-features                            # exit 0
cargo metadata --format-version 1 --no-deps >/dev/null            # exit 0
cargo fmt --all -- --check                                        # exit 0
cargo clippy --workspace --all-features --all-targets -- -D warnings   # exit 0
cargo test --workspace --all-features                             # exit 0
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps   # exit 0

# Local — config files parse
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release-plz.yml'))"   # exit 0
python3 -c "import tomllib,sys; tomllib.load(open('release-plz.toml','rb'))"          # exit 0  (Python 3.11+)

# CI on the feature-branch PR
ci / fmt, ci / clippy, ci / test (ubuntu-latest, stable), ci / docs, ci / doc-coverage   # all green
audit / audit                                                                            # green
deny / deny                                                                              # green
msrv / verify                                                                            # green (signal-only)

# Post-merge operator verification
# 1. release-plz GitHub App installed on the repo
# 2. Repo secrets RELEASE_PLZ_APP_ID and RELEASE_PLZ_APP_PRIVATE_KEY set
# 3. Baseline tags <crate>-v0.0.0 created at the bootstrap-merge commit (13 tags)
# 4. First release-plz workflow run on main: both jobs succeed, no release PR opens (Verification 1)
# 5. Smoketest PR `feat(core): SMA-307 add release-plz smoketest docstring` merged
# 6. Subsequent release-plz workflow run opens a release PR proposing
#    paigasus-helikon-core bump + paigasus-helikon (facade) patch bump (Verification 2)
```

The PR description includes a checklist of the post-merge runbook steps so the operator (Sven) doesn't lose track.

## Conventions used in this plan

- **Commit prefix**: `<type>(<scope>): SMA-307 <message>`. Bootstrap-only commits use **`chore(...)` or `docs(...)` types — never `feat`/`fix`**. release-plz parses every commit since the last per-crate tag; a `feat(workspace): ...` commit that touches every `Cargo.toml` would attribute a bump to every crate. See spec §6.2.
- **Branch**: all work happens on `feature/sma-307-automated-versioning-with-release-plz`. The branch was created earlier with the spec commit `d62e58d` already on it.
- **No remote pushes** until Task 13 (push to open PR). Post-merge operator steps (Tasks 15–19) are user-driven and happen on `main`, not on the feature branch.
- **Path style.** Steps use absolute paths (`/Users/smaschek/dev/paigasus/paigasus-helikon/...`) so an agent operating from any cwd doesn't get confused.
- **Workspace member iteration.** When a step says "for each of the 13 crates", the canonical list is the directories under `crates/`:
  ```
  paigasus-helikon
  paigasus-helikon-cli
  paigasus-helikon-core
  paigasus-helikon-evals
  paigasus-helikon-macros
  paigasus-helikon-mcp
  paigasus-helikon-providers-anthropic
  paigasus-helikon-providers-openai
  paigasus-helikon-runtime-agentcore
  paigasus-helikon-runtime-axum
  paigasus-helikon-runtime-temporal
  paigasus-helikon-runtime-tokio
  paigasus-helikon-tools
  ```
- **Linear ticket status**: per workspace convention, do **not** auto-close SMA-307 from PR merge. Sven moves status manually after review.

---

## File Structure

**Created:**
- `/Users/smaschek/dev/paigasus/paigasus-helikon/release-plz.toml` — workspace-root release-plz config (publish=false workspace-wide, CLI publish=false permanently, dependencies_update=true, sort_commits=newest)
- `/Users/smaschek/dev/paigasus/paigasus-helikon/.github/workflows/release-plz.yml` — single workflow, two sequential jobs (`release-plz-release` → `release-plz-pr`), GitHub-App-token authenticated

**Modified:**
- `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon/Cargo.toml`
- `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-cli/Cargo.toml`
- `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-core/Cargo.toml`
- `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-evals/Cargo.toml`
- `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-macros/Cargo.toml`
- `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-mcp/Cargo.toml`
- `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-providers-anthropic/Cargo.toml`
- `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-providers-openai/Cargo.toml`
- `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-runtime-agentcore/Cargo.toml`
- `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-runtime-axum/Cargo.toml`
- `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-runtime-temporal/Cargo.toml`
- `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-runtime-tokio/Cargo.toml`
- `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-tools/Cargo.toml`
- `/Users/smaschek/dev/paigasus/paigasus-helikon/CLAUDE.md` — two edits (inheritance carve-out + non-obvious-pattern bullet)
- `/Users/smaschek/dev/paigasus/paigasus-helikon/CONTRIBUTING.md` — append "Releases" section

**Untouched:** root `Cargo.toml` (workspace-level `version = "0.0.0"` stays as safety net), `Cargo.lock`, all crate `src/` files, all existing workflows (`ci.yml`, `msrv.yml`, `audit.yml`, `deny.yml`, `sbom.yml`), `deny.toml`, `.github/dependabot.yml`, `scripts/`.

---

### Task 1: Verify branch state

**Files:** none (git operation only).

- [ ] **Step 1: Confirm current branch and that the spec is already committed**

Run:
```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon rev-parse --abbrev-ref HEAD
```
Expected: `feature/sma-307-automated-versioning-with-release-plz`

Run:
```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon log --oneline -2
```
Expected (most recent commit first):
```
d62e58d docs(specs): SMA-307 add release-plz automation design
a0403ba Merge pull request #3 from SMK1085/feature/sma-306-...
```

If the branch is wrong or the spec commit is missing, **stop** and surface the discrepancy. Do not proceed.

- [ ] **Step 2: Confirm working tree is clean**

Run:
```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon status --short
```
Expected: empty output.

If there are uncommitted changes, **stop** and surface them.

No commit in this task.

---

### Task 2: Commit the implementation plan

**Files:**
- Modify: `/Users/smaschek/dev/paigasus/paigasus-helikon/docs/superpowers/plans/2026-05-17-sma-307-release-plz.md` (this very file)

The plan file was authored before the agent started executing. This task commits it as the second commit on the feature branch, matching the SMA-306 pattern (spec + plan land first, implementation commits follow).

- [ ] **Step 1: Verify the plan file exists**

Run:
```bash
ls -la /Users/smaschek/dev/paigasus/paigasus-helikon/docs/superpowers/plans/2026-05-17-sma-307-release-plz.md
```
Expected: file present, non-zero size.

- [ ] **Step 2: Stage and commit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git add docs/superpowers/plans/2026-05-17-sma-307-release-plz.md && \
  git commit -m "docs(plans): SMA-307 add release-plz implementation plan"
```

- [ ] **Step 3: Confirm**

Run:
```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon log --oneline -3
```
Expected:
```
<sha> docs(plans): SMA-307 add release-plz implementation plan
d62e58d docs(specs): SMA-307 add release-plz automation design
a0403ba Merge pull request #3 from SMK1085/feature/sma-306-...
```

---

### Task 3: Break `version` inheritance for every crate

**Files:**
- Modify: all 13 `crates/*/Cargo.toml` (each replaces `version.workspace = true` with `version = "0.0.0"`)

This is the single largest mechanical change in the PR. Every crate's `Cargo.toml` has the same edit: a one-line replacement on the `version.workspace = true` line.

Each modified file ends up looking like the `paigasus-helikon-core` example below — note that **only** the `version` line changes; every other `*.workspace = true` line stays.

```toml
[package]
name        = "paigasus-helikon-core"
description = "Trait surface and concrete types for the Paigasus Helikon AI SDK."
version                = "0.0.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true
# ...the rest of the file (lints, dependencies, features, etc.) is untouched
```

- [ ] **Step 1: Edit `paigasus-helikon/Cargo.toml`**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon/Cargo.toml`, replace:
```toml
version.workspace      = true
```
with:
```toml
version                = "0.0.0"
```

- [ ] **Step 2: Edit `paigasus-helikon-cli/Cargo.toml`**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-cli/Cargo.toml`, replace `version.workspace      = true` with `version                = "0.0.0"`.

- [ ] **Step 3: Edit `paigasus-helikon-core/Cargo.toml`**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-core/Cargo.toml`, replace `version.workspace      = true` with `version                = "0.0.0"`.

- [ ] **Step 4: Edit `paigasus-helikon-evals/Cargo.toml`**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-evals/Cargo.toml`, replace `version.workspace      = true` with `version                = "0.0.0"`.

- [ ] **Step 5: Edit `paigasus-helikon-macros/Cargo.toml`**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-macros/Cargo.toml`, replace `version.workspace      = true` with `version                = "0.0.0"`.

- [ ] **Step 6: Edit `paigasus-helikon-mcp/Cargo.toml`**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-mcp/Cargo.toml`, replace `version.workspace      = true` with `version                = "0.0.0"`.

- [ ] **Step 7: Edit `paigasus-helikon-providers-anthropic/Cargo.toml`**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-providers-anthropic/Cargo.toml`, replace `version.workspace      = true` with `version                = "0.0.0"`.

- [ ] **Step 8: Edit `paigasus-helikon-providers-openai/Cargo.toml`**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-providers-openai/Cargo.toml`, replace `version.workspace      = true` with `version                = "0.0.0"`.

- [ ] **Step 9: Edit `paigasus-helikon-runtime-agentcore/Cargo.toml`**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-runtime-agentcore/Cargo.toml`, replace `version.workspace      = true` with `version                = "0.0.0"`.

- [ ] **Step 10: Edit `paigasus-helikon-runtime-axum/Cargo.toml`**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-runtime-axum/Cargo.toml`, replace `version.workspace      = true` with `version                = "0.0.0"`.

- [ ] **Step 11: Edit `paigasus-helikon-runtime-temporal/Cargo.toml`**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-runtime-temporal/Cargo.toml`, replace `version.workspace      = true` with `version                = "0.0.0"`.

- [ ] **Step 12: Edit `paigasus-helikon-runtime-tokio/Cargo.toml`**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-runtime-tokio/Cargo.toml`, replace `version.workspace      = true` with `version                = "0.0.0"`.

- [ ] **Step 13: Edit `paigasus-helikon-tools/Cargo.toml`**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-tools/Cargo.toml`, replace `version.workspace      = true` with `version                = "0.0.0"`.

- [ ] **Step 14: Verify all 13 crates are updated**

Run (note the `^version` anchor — a loose `version.workspace` pattern would
also match `rust-version.workspace`, producing false positives):
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  grep -cE '^version[[:space:]]*\.workspace' crates/*/Cargo.toml
```
Expected: every line ends `:0` (zero hits per file). If any file shows `:1`, that crate was missed — go back and fix it.

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  grep '^version' crates/*/Cargo.toml
```
Expected: 13 lines, each ending `= "0.0.0"`.

---

### Task 4: Verify the workspace still builds

**Files:** none (verification only).

- [ ] **Step 1: `cargo metadata` parses the manifests**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  cargo metadata --format-version 1 --no-deps >/dev/null
```
Expected: exit 0, no output.

A non-zero exit here means one of the Cargo.toml edits introduced a malformed file. Fix before proceeding.

- [ ] **Step 2: `cargo build` succeeds**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && cargo build --workspace --all-features
```
Expected: `Finished` line at the end, no errors.

First-time cold build takes several minutes (workspace is small, but tokio/serde/etc. need compiling). Incremental builds after the edit should be nearly instant — Cargo only re-checksums the manifests; no source changed.

- [ ] **Step 3: Confirm every crate is still at `0.0.0`**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  cargo metadata --format-version 1 --no-deps | python3 -c "
import json, sys
m = json.load(sys.stdin)
for p in sorted(m['packages'], key=lambda x: x['name']):
    if p['name'].startswith('paigasus-helikon'):
        print(f\"{p['name']}\t{p['version']}\")
"
```
Expected: all 13 lines end with `0.0.0`.

---

### Task 5: Commit the version-inheritance break

**Files:** the 13 modified `crates/*/Cargo.toml` files from Task 3.

- [ ] **Step 1: Stage the manifest edits**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git add crates/*/Cargo.toml
```

- [ ] **Step 2: Confirm the diff is exactly the 13 expected one-liners**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git diff --cached --stat
```
Expected: 13 files changed, 13 insertions(+), 13 deletions(-).

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git diff --cached
```
Expected: 13 hunks, each replacing one `version.workspace      = true` line with `version                = "0.0.0"`. No other changes.

If anything else appears (whitespace edits to other lines, etc.), **unstage and redo** — release-plz reads the manifests strictly and unrelated edits inflate the apparent blast radius.

- [ ] **Step 3: Commit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git commit -m "chore(workspace): SMA-307 break version inheritance to per-crate

Each crate now owns its version line so release-plz can bump crates
independently. All other workspace inheritance (edition, rust-version,
authors, license, repository, homepage, keywords, categories) stays.
The workspace.package.version = \"0.0.0\" default in the root Cargo.toml
is retained as a safety net for new crates. See spec §3."
```

---

### Task 6: Create `release-plz.toml`

**Files:**
- Create: `/Users/smaschek/dev/paigasus/paigasus-helikon/release-plz.toml`

- [ ] **Step 1: Write `release-plz.toml`**

Create `/Users/smaschek/dev/paigasus/paigasus-helikon/release-plz.toml` with this exact content:

```toml
# release-plz.toml — automated versioning, changelogs, GitHub releases.
# Reference: https://release-plz.dev/docs/config

[workspace]
# Bump dependent crates whenever a dep's version changes. The facade
# (paigasus-helikon) depends on every sibling via `[workspace.dependencies]`,
# so a `feat(core):` commit triggers: paigasus-helikon-core bumped →
# facade's `[workspace.dependencies].paigasus-helikon-core` line updated →
# facade itself gets a patch bump.
dependencies_update = true

# Workspace-wide kill-switch for crates.io publishing. release-plz still
# bumps versions, generates CHANGELOG.md, creates per-crate git tags, and
# creates GitHub releases — it just skips `cargo publish`. Stage 1 removes
# this line to enable real publishing. The per-package override on
# `paigasus-helikon-cli` (below) survives that flip.
publish = false

# release-plz uses pre-1.0 (0.x.y) bump semantics automatically when the
# starting version is `0.x.y`. No explicit flag is needed; this comment
# exists so future-you doesn't add one while looking for it.

[workspace.changelog]
sort_commits = "newest"

# Skip the CLI from crates.io even after Stage 1 lifts the workspace-level
# `publish = false`. The CLI's public surface is binary-only and isn't
# intended as a library dependency.
[[package]]
name = "paigasus-helikon-cli"
publish = false
```

- [ ] **Step 2: Verify the file is well-formed TOML**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  python3 -c "import tomllib; tomllib.load(open('release-plz.toml','rb'))"
```
Expected: exit 0, no output. Requires Python ≥ 3.11.

On Python < 3.11, substitute:
```bash
pip install --quiet tomli && \
  python3 -c "import tomli; tomli.load(open('release-plz.toml','rb'))"
```

- [ ] **Step 3: (Optional) Validate the release-plz schema locally**

This step is skippable — the workflow itself will validate the config on first run after merge. Only do this if `release-plz` is already on `$PATH`, or if you want to install it.

If installed:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && release-plz --version
cd /Users/smaschek/dev/paigasus/paigasus-helikon && release-plz check-updates 2>&1 | head -20
```
Expected: `release-plz check-updates` doesn't fail with a config-parse error. It may print "no updates" or a list of proposed bumps — either is acceptable.

If `release-plz` is not installed and you'd like to install it just for this check:
```bash
cargo install release-plz --locked
# or:
cargo binstall release-plz
```
Installation takes 3–8 minutes from source, ~30 seconds via binstall.

- [ ] **Step 4: Stage and commit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git add release-plz.toml && \
  git commit -m "chore(release): SMA-307 add release-plz config

Workspace-wide publish = false (Stage 1 lifts it), per-package
publish = false on paigasus-helikon-cli (permanent — binary, not a
library dep). dependencies_update = true threads facade patch bumps
when any internal dep bumps. sort_commits = newest. Pre-1.0 (0.x.y)
bump semantics are release-plz's automatic default; no explicit flag
needed. See spec §4."
```

---

### Task 7: Create `.github/workflows/release-plz.yml`

**Files:**
- Create: `/Users/smaschek/dev/paigasus/paigasus-helikon/.github/workflows/release-plz.yml`

- [ ] **Step 1: Write the workflow**

Create `/Users/smaschek/dev/paigasus/paigasus-helikon/.github/workflows/release-plz.yml` with this exact content:

```yaml
name: release-plz

on:
  push:
    branches: [main]

concurrency:
  group: release-plz-${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: false   # never cancel mid-tag-push / mid-PR-update

permissions:
  contents: write
  pull-requests: write

env:
  CARGO_TERM_COLOR: always

jobs:
  release-plz-release:
    name: release-plz-release
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0          # release-plz needs full history
      - uses: dtolnay/rust-toolchain@stable
      - uses: actions/create-github-app-token@v1
        id: app-token
        with:
          app-id: ${{ secrets.RELEASE_PLZ_APP_ID }}
          private-key: ${{ secrets.RELEASE_PLZ_APP_PRIVATE_KEY }}
      - uses: release-plz/action@v0.5
        with:
          command: release
        env:
          GITHUB_TOKEN: ${{ steps.app-token.outputs.token }}
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}

  release-plz-pr:
    name: release-plz-pr
    runs-on: ubuntu-latest
    needs: release-plz-release    # tag/release from a merged release PR is
                                  # processed before the rolling PR is recomputed
    steps:
      - uses: actions/checkout@v6
        with:
          fetch-depth: 0
      - uses: dtolnay/rust-toolchain@stable
      - uses: actions/create-github-app-token@v1
        id: app-token
        with:
          app-id: ${{ secrets.RELEASE_PLZ_APP_ID }}
          private-key: ${{ secrets.RELEASE_PLZ_APP_PRIVATE_KEY }}
      - uses: release-plz/action@v0.5
        with:
          command: release-pr
        env:
          GITHUB_TOKEN: ${{ steps.app-token.outputs.token }}
```

- [ ] **Step 2: Verify the file is valid YAML**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release-plz.yml'))"
```
Expected: exit 0, no output.

If `yaml` (`PyYAML`) is not available, install it:
```bash
pip install --quiet pyyaml
```

- [ ] **Step 3: Verify the file is GitHub-Actions-syntax valid (if `actionlint` is available)**

This step is skippable. If `actionlint` is on `$PATH`:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && actionlint .github/workflows/release-plz.yml
```
Expected: no output (silent success).

If `actionlint` reports an error about an unknown action (`release-plz/action@v0.5` may not be in its database), that's acceptable — it doesn't validate third-party action existence. Any structural YAML or workflow-schema error is **not** acceptable.

- [ ] **Step 4: Stage and commit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git add .github/workflows/release-plz.yml && \
  git commit -m "chore(release): SMA-307 add release-plz workflow

Two jobs ordered via needs:. Auth via the release-plz GitHub App
(actions/create-github-app-token@v1) so release PRs trigger downstream
CI/audit/deny workflows — GITHUB_TOKEN would suppress them under
GitHub's anti-recursion safety. CARGO_REGISTRY_TOKEN referenced but
unused until Stage 1 (publish = false in release-plz.toml). See spec §5."
```

---

### Task 8: Update `CLAUDE.md`

**Files:**
- Modify: `/Users/smaschek/dev/paigasus/paigasus-helikon/CLAUDE.md`

Two edits to `CLAUDE.md`. The agent should preserve everything else verbatim.

- [ ] **Step 1: Append the inheritance carve-out sentence**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/CLAUDE.md`, find the paragraph that begins:

> *Workspace inheritance is **mandatory**: per-crate `Cargo.toml`s only set `name`, `description`, and any crate-specific bits. Everything else (`version`, `edition`, `rust-version`, `authors`, `license`, `repository`, `homepage`, `keywords`, `categories`) inherits from `[workspace.package]` in the root `Cargo.toml`. Don't hardcode these per-crate.*

Replace the parenthetical list `(`version`, `edition`, …)` so `version` is dropped, and append one sentence to the end of the paragraph. The full replacement paragraph becomes:

> *Workspace inheritance is **mandatory**: per-crate `Cargo.toml`s only set `name`, `description`, and any crate-specific bits. Everything else (`edition`, `rust-version`, `authors`, `license`, `repository`, `homepage`, `keywords`, `categories`) inherits from `[workspace.package]` in the root `Cargo.toml`. Don't hardcode these per-crate. **Exception**: `version` is per-crate — each `crates/*/Cargo.toml` sets `version = "0.0.0"` explicitly so release-plz can bump crates independently (see SMA-307). The `workspace.package.version = "0.0.0"` default in the root `Cargo.toml` stays as a safety net for new crates that forget to declare their own.*

- [ ] **Step 2: Add a new bullet under "Non-obvious patterns to preserve"**

Find the section that begins:
```markdown
## Non-obvious patterns to preserve
```

Append this bullet to the end of that section's list (after the existing `**Nightly is date-pinned**` bullet):

```markdown
- **Bootstrap commits on release infrastructure must use `chore(...)` or `docs(...)` types**, never `feat`/`fix`. release-plz parses every commit since the last per-crate tag — a `feat(workspace): ...` commit that touches every `Cargo.toml` would attribute a bump to every crate. The SMA-307 bootstrap PR followed this rule; the same rule applies to any future `release-plz.toml` or `release-plz.yml` edits.
```

- [ ] **Step 3: Verify both edits**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  grep -c 'release-plz can bump crates independently' CLAUDE.md
```
Expected: `1` (the inheritance carve-out sentence is present once).

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  grep -c "Bootstrap commits on release infrastructure" CLAUDE.md
```
Expected: `1`.

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  grep -c 'Everything else (\`edition\`' CLAUDE.md
```
Expected: `1` (the parenthetical no longer starts with `version`).

- [ ] **Step 4: Commit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git add CLAUDE.md && \
  git commit -m "docs(claude): SMA-307 document version carve-out

Workspace inheritance is still mandatory for everything else; only
\`version\` is now per-crate so release-plz can bump independently.
Adds a non-obvious-patterns bullet warning future edits to release-plz
config to use chore/docs commit types only. See spec §7.1."
```

---

### Task 9: Append "Releases" section to `CONTRIBUTING.md`

**Files:**
- Modify: `/Users/smaschek/dev/paigasus/paigasus-helikon/CONTRIBUTING.md`

- [ ] **Step 1: Locate the insertion point**

Open `/Users/smaschek/dev/paigasus/paigasus-helikon/CONTRIBUTING.md`. Find the existing `## Supply-chain security` section and scroll to its end (the last paragraph ends with "*Major bumps remain ungrouped so breaking changes are reviewed in isolation.*").

The new "Releases" section is appended immediately after the last line of the file. There is no other content after "Supply-chain security" in the current revision.

- [ ] **Step 2: Append the new section**

Append the following content to the end of `/Users/smaschek/dev/paigasus/paigasus-helikon/CONTRIBUTING.md` (preserve the existing trailing newline, then add a blank line, then the section):

```markdown

## Releases

Releases are automated by [release-plz](https://release-plz.dev). The contract:

1. **Conventional Commits drive bumps.** `feat(<scope>):` → minor (pre-1.0:
   typically patch — release-plz applies pre-1.0 semantics automatically).
   `fix(<scope>):` → patch. `feat!:` / `BREAKING CHANGE:` footer → minor pre-1.0
   / major post-1.0. `chore`, `docs`, `ci`, `refactor`, `test`, `style` → no bump.
   `<scope>` is informational — release-plz attributes by files changed.

2. **A rolling release PR.** The `release-plz` workflow runs on every push to
   `main` and maintains one open release PR titled `chore: release v...`. It
   enumerates which crates will bump and to what version, and shows the
   generated `CHANGELOG.md` additions. Reviewers verify, then squash-merge.

3. **Merging the release PR** triggers the workflow's `release-plz-release`
   job, which creates per-crate git tags (`<crate>-v<version>`), GitHub
   releases, and — once Stage 1 lifts `publish = false` — publishes to
   crates.io. The CLI crate (`paigasus-helikon-cli`) is permanently
   `publish = false` because it's a binary, not a library dependency.

4. **Overriding release-plz.** If the proposed bumps are wrong, edit the
   release PR's `Cargo.toml` / `CHANGELOG.md` directly — release-plz
   respects manual edits and won't clobber them on subsequent runs.

5. **Bootstrap commits on release infrastructure.** Any commit that edits
   `release-plz.toml`, `.github/workflows/release-plz.yml`, or every crate's
   `Cargo.toml` simultaneously must use `chore(...)` or `docs(...)` types —
   never `feat`/`fix`. Otherwise release-plz attributes a workspace-wide bump
   to the infrastructure change. See CLAUDE.md for the full rule.

### Authentication

release-plz authenticates as the [release-plz GitHub App](https://github.com/apps/release-plz)
installation on this repo. The workflow mints a per-job installation token
via `actions/create-github-app-token@v1` from two repo secrets
(`RELEASE_PLZ_APP_ID`, `RELEASE_PLZ_APP_PRIVATE_KEY`). The App identity is
listed as a bypass actor in SMA-309's branch-name ruleset, so release-plz's
`release-plz-<timestamp>` branch prefix is permitted.

A fine-grained PAT (with `contents: write` + `pull-requests: write` scoped
to this repo, stored as `RELEASE_PLZ_TOKEN`) is the documented fallback if
the App becomes unavailable. The workflow would be changed to read
`secrets.RELEASE_PLZ_TOKEN` instead of minting an App token.
```

- [ ] **Step 3: Verify the section is present**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  grep -c '^## Releases$' CONTRIBUTING.md
```
Expected: `1`.

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  grep -c 'release-plz GitHub App' CONTRIBUTING.md
```
Expected: `1`.

- [ ] **Step 4: Commit**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git add CONTRIBUTING.md && \
  git commit -m "docs(contributing): SMA-307 add Releases section

Documents Conventional-Commits → bump mapping, the rolling release PR,
the merge-to-release flow, override mechanics, the chore-only rule for
release-infra commits, and the GitHub-App auth path with PAT fallback.
See spec §7.2."
```

---

### Task 10: Run the full local CI gate

**Files:** none (verification only).

This task runs every check CI runs, in order, to catch any regression introduced by Tasks 3–9 before pushing.

- [ ] **Step 1: `cargo fmt`**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  cargo fmt --all -- --check
```
Expected: exit 0, no output.

- [ ] **Step 2: `cargo clippy`**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  cargo clippy --workspace --all-features --all-targets -- -D warnings
```
Expected: exit 0, no warnings.

- [ ] **Step 3: `cargo test`**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  cargo test --workspace --all-features
```
Expected: exit 0. Crates are stubs, so the test counts will be tiny or zero.

- [ ] **Step 4: `cargo doc`**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```
Expected: exit 0. The expected "filename collision" warning on the facade vs. CLI binary is non-fatal (see CLAUDE.md "Non-obvious patterns to preserve").

- [ ] **Step 5: Doc-coverage script**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
```
Expected: exit 0. (Requires `rustup toolchain install nightly-2026-05-01` once on the contributor's machine. If it's not installed, install it first.)

If any of Steps 1–5 fail, **stop**, diagnose, and fix before proceeding. The bootstrap should be CI-clean before pushing.

No commit in this task.

---

### Task 11: Final pre-push sanity check

**Files:** none.

- [ ] **Step 1: Confirm commit chain**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git log --oneline main..HEAD
```
Expected (newest first):
```
<sha> docs(contributing): SMA-307 add Releases section
<sha> docs(claude): SMA-307 document version carve-out
<sha> chore(release): SMA-307 add release-plz workflow
<sha> chore(release): SMA-307 add release-plz config
<sha> chore(workspace): SMA-307 break version inheritance to per-crate
<sha> docs(plans): SMA-307 add release-plz implementation plan
d62e58d docs(specs): SMA-307 add release-plz automation design
```

That's seven commits ahead of `main`. **Every commit type is `chore` or `docs`** — no `feat`/`fix` (spec §6.2).

- [ ] **Step 2: Confirm no working-tree changes**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git status --short
```
Expected: empty.

- [ ] **Step 3: Confirm the diff against `main` is bounded**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git diff main..HEAD --stat
```
Expected: ~18 files changed
- 13 × `crates/*/Cargo.toml` (one line each)
- `CLAUDE.md`
- `CONTRIBUTING.md`
- `release-plz.toml` (new)
- `.github/workflows/release-plz.yml` (new)
- `docs/superpowers/specs/2026-05-17-sma-307-release-plz-design.md` (new)
- `docs/superpowers/plans/2026-05-17-sma-307-release-plz.md` (new)

No unexpected files. If anything else appears (e.g. `Cargo.lock`, `target/`), investigate.

---

### Task 12: Push the feature branch

**Files:** none.

- [ ] **Step 1: Push with upstream tracking**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git push -u origin feature/sma-307-automated-versioning-with-release-plz
```
Expected: GitHub returns a "Create a pull request" link. Note the URL.

If the push is rejected by the SMA-309 branch-name ruleset (it shouldn't be — SMA-309 isn't landed yet, and the branch name matches the rule anyway), surface the rejection and **stop**.

No commit in this task.

---

### Task 13: Open the PR

**Files:** none.

- [ ] **Step 1: Open the PR via `gh`**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  gh pr create --base main --head feature/sma-307-automated-versioning-with-release-plz \
    --title "feat: SMA-307 automated versioning with release-plz" \
    --body "$(cat <<'EOF'
## Summary

Wires release-plz into the workspace for Conventional-Commits-driven version bumps, changelogs, and GitHub releases. `publish = false` is set workspace-wide for this PR — Stage 1 will lift it.

- `release-plz.toml` at workspace root (`dependencies_update = true`, `sort_commits = "newest"`, CLI `publish = false` permanently)
- `.github/workflows/release-plz.yml` with two sequential jobs, authenticated via the release-plz GitHub App so release PRs trigger downstream CI/audit/deny
- Every `crates/*/Cargo.toml` now sets `version = "0.0.0"` explicitly (only `version` leaves workspace inheritance; everything else stays)
- `CLAUDE.md` documents the carve-out + the chore-only rule for release-infra commits
- `CONTRIBUTING.md` gains a "Releases" section

**Spec**: [\`docs/superpowers/specs/2026-05-17-sma-307-release-plz-design.md\`](docs/superpowers/specs/2026-05-17-sma-307-release-plz-design.md)
**Plan**: [\`docs/superpowers/plans/2026-05-17-sma-307-release-plz.md\`](docs/superpowers/plans/2026-05-17-sma-307-release-plz.md)
**Linear**: [SMA-307](https://linear.app/smaschek/issue/SMA-307/automated-versioning-with-release-plz)

## Post-merge runbook (operator)

The full procedure lives in plan §Task 15–§Task 19. Quick checklist:

- [ ] Install the [release-plz GitHub App](https://github.com/apps/release-plz) on this repo
- [ ] Add repo secrets \`RELEASE_PLZ_APP_ID\` and \`RELEASE_PLZ_APP_PRIVATE_KEY\`
- [ ] **Within ~30 seconds of merge**, run the baseline-tag script (plan §Task 16) to create 13 \`<crate>-v0.0.0\` tags at the merge commit
- [ ] Observe first \`release-plz\` workflow run on \`main\` — both jobs green, no release PR opens (Verification 1)
- [ ] Open the \`feat(core):\` smoketest PR (plan §Task 18) — release-plz opens a release PR proposing core + facade bumps (Verification 2)

## Test plan

- [ ] CI gates green: \`ci / fmt\`, \`ci / clippy\`, \`ci / test (ubuntu-latest, stable)\`, \`ci / docs\`, \`ci / doc-coverage\`
- [ ] Supply-chain gates green: \`audit / audit\`, \`deny / deny\`
- [ ] MSRV signal green: \`msrv / verify\`
- [ ] Post-merge Verification 1: first release-plz workflow run after baseline-tag creation opens no PR
- [ ] Post-merge Verification 2: smoketest \`feat(core): ...\` commit produces a release PR with \`paigasus-helikon-core\` and \`paigasus-helikon\` bumps

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: a PR URL is printed. Capture it.

- [ ] **Step 2: Open the PR in a browser to verify formatting**

Run:
```bash
gh pr view --web
```

Spot-check that:
- The Summary and Post-merge runbook render as expected.
- The Spec / Plan / Linear links are clickable.
- The Test plan checklist renders as checkboxes.

No commit in this task.

---

### Task 14: Wait for CI to go green on the PR

**Files:** none.

- [ ] **Step 1: Watch the PR check run**

Run:
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  gh pr checks --watch
```
Expected: all required checks (`ci / fmt`, `ci / clippy`, `ci / test (ubuntu-latest, stable)`, `ci / docs`, `ci / doc-coverage`, `audit / audit`, `deny / deny`, `msrv / verify`) report `pass`. Other matrix variants (`ci / test (macos-latest, stable)`, `ci / test (windows-latest, stable)`, `ci / test (ubuntu-latest, 1.75)`, etc.) are signal-only and may take longer; they should not block.

- [ ] **Step 2: If a check fails**

Run:
```bash
gh pr checks
```
Identify the failing check. Open its logs via `gh run view <run-id> --log-failed`. Fix on the feature branch, push, and re-watch.

Common pitfalls to check:
- `cargo fmt --check` failed: someone hand-edited a Cargo.toml and shifted alignment. Re-run `cargo fmt --all` and recommit.
- `cargo clippy` failed: shouldn't happen — no source code changed. Investigate the specific lint.
- `cargo test` failed on a specific platform: rare for this PR (no source change), but if it does, look at the platform-specific log.
- `cargo doc` failed with the facade/CLI collision warning: that's the known non-fatal warning (`CLAUDE.md` "Non-obvious patterns"). If it's failing now, somebody added `-D warnings` to a different rustdoc invocation — investigate.

No commit in this task (unless a fix is needed).

---

### Task 15: Operator — install the release-plz GitHub App and set secrets

**Files:** none (GitHub repo settings only). **Operator action — happens before PR merge so secrets are in place at the moment of merge.**

- [ ] **Step 1: Install the App**

Visit https://github.com/apps/release-plz and click **Install**. Select the `paigasus-helikon` repo. Confirm.

On the App's "Installed" confirmation page, note the **App ID** (a numeric value, ~6 digits).

- [ ] **Step 2: Generate the App's private key**

In GitHub: **Settings → Developer settings → GitHub Apps → release-plz** (you should see it listed because you just installed it). Scroll to "Private keys" → **Generate a private key**. A `.pem` file downloads.

Copy the entire PEM file contents (including `-----BEGIN ... PRIVATE KEY-----` and `-----END ... PRIVATE KEY-----` lines).

- [ ] **Step 3: Add the two repo secrets**

In the `paigasus-helikon` repo: **Settings → Secrets and variables → Actions → New repository secret**.

Create:
- `RELEASE_PLZ_APP_ID` — paste the numeric App ID from Step 1.
- `RELEASE_PLZ_APP_PRIVATE_KEY` — paste the full PEM contents from Step 2.

- [ ] **Step 4: Do NOT add `CARGO_REGISTRY_TOKEN`**

This is intentional. Stage 1 owns the publish rollout. Adding the token now means it'll silently be referenced but never used (because `publish = false`), which is fine — but adding it is wasted effort and risks a leaked secret with no benefit.

No commit in this task.

---

### Task 16: Operator — merge the PR and create baseline tags

**Files:** none (post-merge git operation). **Operator action.**

- [ ] **Step 1: Squash-merge the PR**

In the PR UI: **Squash and merge**. The squash-commit subject must remain a `feat:` type (the PR title is `feat: SMA-307 automated versioning with release-plz`).

Wait — but earlier we said all bootstrap commits must be `chore`/`docs`. Why is the PR title `feat:`?

**Answer:** the individual PR commits on the feature branch are all `chore`/`docs` (verified in Task 11). After squash-merge, the SINGLE commit on `main` carries the PR title. release-plz parses commits since the last per-crate tag. The strategy in this plan is to **create the baseline tags AT the squash-merge commit**, so release-plz starts counting commits *after* the merge. The squash-merge commit's type (`feat:`) is therefore invisible to release-plz — it's at or before the baseline tags, not after.

If you'd prefer belt-and-braces, change the PR title to `chore: SMA-307 automated versioning with release-plz` before squash-merging. The plan's correctness doesn't depend on it, but it removes one variable.

- [ ] **Step 2: Switch to main and pull**

```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git checkout main && git pull --ff-only origin main
```

- [ ] **Step 3: Capture the merge SHA**

```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git log --oneline -1
```
Note the most recent commit SHA — this is the bootstrap-merge commit.

- [ ] **Step 4: Create 13 baseline tags at the merge commit**

Run (single multi-line command):
```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  MERGE_SHA="$(git rev-parse HEAD)" && \
  for crate in paigasus-helikon paigasus-helikon-core paigasus-helikon-cli \
               paigasus-helikon-evals paigasus-helikon-macros paigasus-helikon-mcp \
               paigasus-helikon-providers-anthropic paigasus-helikon-providers-openai \
               paigasus-helikon-runtime-agentcore paigasus-helikon-runtime-axum \
               paigasus-helikon-runtime-temporal paigasus-helikon-runtime-tokio \
               paigasus-helikon-tools; do
    git tag "${crate}-v0.0.0" "${MERGE_SHA}"
  done && \
  git push origin --tags
```

Expected: 13 tags pushed. GitHub's response lists each `[new tag]`.

- [ ] **Step 5: Verify the tags exist**

```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git tag --list 'paigasus-helikon-*v0.0.0' | sort
```
Expected: 13 lines.

```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git ls-remote --tags origin 'refs/tags/paigasus-helikon-*v0.0.0' | wc -l
```
Expected: `13`.

**Timing note.** This task must complete **within ~30 seconds** of the merge in Step 1, before the post-merge `release-plz` workflow run finishes. If release-plz beats you to the punch and opens a kitchen-sink release PR, see Task 17 fallback.

No commit in this task (only tag-push).

---

### Task 17: Operator — verify Verification 1 (no spurious release PR)

**Files:** none. **Operator action.**

- [ ] **Step 1: Wait for the first `release-plz` workflow run on `main` to complete**

Run:
```bash
gh run list --workflow=release-plz --branch=main --limit=1
```
Expected: a run is listed and its status is `completed` and conclusion is `success`. If still `in_progress`, re-run after ~30s.

Or watch in real-time:
```bash
gh run watch --workflow=release-plz --exit-status
```
Expected: exit 0.

- [ ] **Step 2: Confirm no release PR was opened**

Run:
```bash
gh pr list --label "" --head 'release-plz-' --state open --limit 5
```
Or simply:
```bash
gh pr list --state open --limit 10
```
Expected: no PR with a `release-plz-<timestamp>` head branch. (Other PRs may be open; only the absence of a release-plz one matters.)

If a release PR is open, the baseline tags weren't in place when release-plz ran. **Apply the fallback below.**

#### Fallback — release-plz beat the operator to it

If Step 2 shows an unwanted release PR:

1. Close that PR (don't merge):
   ```bash
   gh pr close <pr-number> --comment "Baseline tags were created post-hoc; reopening below."
   ```
2. Delete the release-plz branch:
   ```bash
   git push origin --delete release-plz-<timestamp>
   ```
   (Or via the GitHub UI's "Restore branch" toggle.)
3. Confirm baseline tags from Task 16 Step 5 still exist.
4. Trigger a fresh release-plz run by pushing an empty commit:
   ```bash
   cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
     git commit --allow-empty -m "chore: SMA-307 retrigger release-plz post-baseline" && \
     git push origin main
   ```
5. Re-watch the run via `gh run watch`. Confirm no release PR opens.

No commit in this task (the fallback's empty commit is only used if needed).

---

### Task 18: Operator — Verification 2 (the `feat(core):` smoketest)

**Files:**
- Modify: `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-core/src/lib.rs`

**Operator action** — this is a separate, post-merge PR to satisfy the ticket's acceptance criterion.

- [ ] **Step 1: Create the smoketest feature branch**

```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git checkout main && git pull --ff-only origin main && \
  git checkout -b feature/sma-307-smoketest-core-feat
```

- [ ] **Step 2: Make a trivial docstring addition to `paigasus-helikon-core`**

Open `/Users/smaschek/dev/paigasus/paigasus-helikon/crates/paigasus-helikon-core/src/lib.rs`. Append one sentence to the crate-root `//!` docstring (the block at the top of the file). For example, if the docstring currently reads:

```rust
//! Core traits and types for the Paigasus AI SDK.
```

Replace with:

```rust
//! Core traits and types for the Paigasus AI SDK.
//!
//! This crate is the dependency root of the Paigasus Helikon workspace; the
//! facade crate re-exports its surface unconditionally.
```

The exact wording isn't important — the only goal is a single-line content change inside `crates/paigasus-helikon-core/` so release-plz attributes a `feat` to this crate.

- [ ] **Step 3: Verify the change compiles**

```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  cargo build -p paigasus-helikon-core
```
Expected: exit 0.

- [ ] **Step 4: Commit with the `feat(core):` prefix**

```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git add crates/paigasus-helikon-core/src/lib.rs && \
  git commit -m "feat(core): SMA-307 add release-plz smoketest docstring

Verification 2 from the SMA-307 plan: this commit should cause release-plz
to propose a paigasus-helikon-core bump and a paigasus-helikon (facade)
patch bump via dependencies_update."
```

- [ ] **Step 5: Push and open the PR**

```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon && \
  git push -u origin feature/sma-307-smoketest-core-feat && \
  gh pr create --base main --head feature/sma-307-smoketest-core-feat \
    --title "feat(core): SMA-307 add release-plz smoketest docstring" \
    --body "Smoketest commit to satisfy SMA-307 acceptance criterion: \`feat(core):\` should produce a release PR bumping \`paigasus-helikon-core\` (and facade via dependencies_update). Merge after CI is green; observe release-plz workflow opens a release PR within ~1 minute."
```

- [ ] **Step 6: Wait for CI green on the smoketest PR**

```bash
gh pr checks --watch
```
Expected: all required checks pass.

- [ ] **Step 7: Squash-merge with the `feat(core):` title preserved**

The squash-merge commit's subject must remain `feat(core): SMA-307 add release-plz smoketest docstring`. GitHub's default is to use the PR title — confirm before clicking Merge.

- [ ] **Step 8: Observe the release-plz workflow re-run on `main`**

```bash
gh run watch --workflow=release-plz --exit-status
```
Expected: exit 0.

- [ ] **Step 9: Confirm a release PR opens**

```bash
gh pr list --state open --limit 5
```
Expected: a new PR titled approximately `chore: release` opened by the `release-plz[bot]` user. Open it and verify:
- `crates/paigasus-helikon-core/Cargo.toml` is bumped (typically `0.0.0 → 0.1.0` under release-plz's pre-1.0 semantics; the exact bump level is whatever release-plz proposes — the spec doesn't require a specific level).
- `crates/paigasus-helikon-core/CHANGELOG.md` is created with the smoketest commit's message.
- `crates/paigasus-helikon/Cargo.toml` is bumped patch (via `dependencies_update`).
- `crates/paigasus-helikon/CHANGELOG.md` is created.

**Verification 2 success criterion met.**

- [ ] **Step 10: Decide whether to merge the release PR**

The spec recommends merging — gives a real `0.0.x → 0.1.0` baseline for Stage 1 to build from. Alternatively, close the PR and revert the smoketest commit if you want to keep the workspace at `0.0.0`.

If merging: do nothing special. release-plz's `release-plz-release` job runs on next `main` push, creates the `paigasus-helikon-core-v0.1.0` and `paigasus-helikon-v0.0.1` tags, and GitHub releases. `publish = false` keeps crates.io out of it.

No commit in this task on `main` (the smoketest PR + release PR carry the commits).

---

### Task 19: Operator — final hand-off

**Files:** none.

- [ ] **Step 1: Move SMA-307 to Done in Linear**

Per the workspace convention, the operator manually transitions SMA-307 from `Todo` → `In Progress` (during PR work) → `Done` (after smoketest verification). The PR merge does **not** auto-close the ticket.

- [ ] **Step 2: Note follow-ups for Stage 1**

The following items remain as Stage 1 (or later) work — none of them block SMA-307 completion:

1. **Lift `publish = false`** in `release-plz.toml` by removing the `[workspace] publish = false` line. The per-package CLI override stays.
2. **Add `CARGO_REGISTRY_TOKEN` repo secret** (crates.io API token, scoped to the relevant crates).
3. **Update `sbom.yml` trigger** from `tags: [v*]` to `tags: [paigasus-helikon-v*]` so SBOM generation fires on the facade's release-plz tags. The facade with `--all-features` captures the workspace per SMA-306's design, so one trigger covers all releases.
4. **(Optional) Add `workflow_dispatch:` to `release-plz.yml`** if the Task 17 fallback bites in practice — makes manual retriggering trivial.
5. **(Optional) Tighten `semver_check` defaults** once real API surface lands in the SDK.

These can be tracked as a single Stage 1 follow-up ticket or split — operator's call.

No commit in this task. SMA-307 is complete.

---

## Self-Review (filled in during plan authoring)

**Spec coverage** — every section of the spec is implemented by a task:
- Spec §1 (Goal & non-goals) → not a code change; informs the PR description in Task 13.
- Spec §2 (File layout) → matches the "File Structure" section above and the file list in Tasks 6–9.
- Spec §3 (`version` carve-out) → Task 3 (13 sub-steps) and Task 5 (commit).
- Spec §4 (`release-plz.toml`) → Task 6.
- Spec §5 (`release-plz.yml`) → Task 7.
- Spec §6 (Smoketest & acceptance) → Tasks 15–18.
- Spec §7 (CLAUDE.md / CONTRIBUTING.md updates) → Tasks 8 and 9.
- Spec §8 (Testing plan) → Tasks 4, 10, 14, 17, 18.
- Spec §9 (Operator runbook) → Tasks 15–19.
- Spec §10 (Open questions / Stage-1 follow-ups) → enumerated in Task 19 Step 2.

**Placeholder scan** — no `TBD`, `TODO`, "implement later", "add error handling", "similar to Task N", or steps that describe what to do without showing how. Every code/config block is the actual content the agent writes.

**Type consistency** — the per-crate `Cargo.toml` edit pattern is identical across all 13 crates (verified by Task 3 Step 14's grep). The `release-plz.toml` content matches spec §4 verbatim. The workflow YAML matches spec §5 verbatim. The CONTRIBUTING.md section matches spec §7.2 verbatim. The CLAUDE.md edits match spec §7.1 verbatim.

**Known one-off** — the squash-merge commit's `feat:` type (Task 16 Step 1) does not violate the spec's "no `feat`/`fix` in bootstrap" rule because the baseline tags created immediately after merge mean that commit is at-or-before the tag from release-plz's perspective. The plan offers the operator a belt-and-braces option to rename the PR title to `chore:` if desired.
