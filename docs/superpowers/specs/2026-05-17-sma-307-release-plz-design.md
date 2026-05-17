# SMA-307 — Automated versioning with release-plz

**Linear issue**: [SMA-307](https://linear.app/smaschek/issue/SMA-307/automated-versioning-with-release-plz)
**Status**: design approved 2026-05-17
**Branch**: `feature/sma-307-automated-versioning-with-release-plz`
**Depends on**: SMA-304 (workspace skeleton — landed), SMA-305 (CI — landed), SMA-306 (supply-chain — landed)
**Related**: SMA-309 (branch protection allow-lists `release-plz[bot]`), SMA-335 (Conventional Commits enforcement — orthogonal; release-plz tolerates non-CC commits)

## 1. Goal & non-goals

**Goal.** Wire [release-plz](https://release-plz.dev) into the workspace so a Conventional-Commits-style commit on `main` produces a release PR that bumps the affected crates, updates per-crate `CHANGELOG.md` files, and — once Stage 1 lifts `publish = false` — creates GitHub releases and pushes to crates.io.

The bootstrap PR for SMA-307 produces:

1. `release-plz.toml` at the workspace root with `dependencies_update = true`, `sort_commits = "newest"`, workspace-wide `publish = false`, and a per-crate `[[package]]` override on `paigasus-helikon-cli` that survives the Stage-1 flip.
2. `.github/workflows/release-plz.yml` with two jobs (`release-plz-release` then `release-plz-pr`, ordered via `needs`).
3. Per-crate `version = "0.0.0"` in each `crates/*/Cargo.toml`, breaking *only* `version` out of workspace inheritance — all other inherited fields (`edition`, `rust-version`, `authors`, `license`, `repository`, `homepage`, `keywords`, `categories`) stay inherited.
4. `CLAUDE.md` updated with the `version`-inheritance carve-out and a new "release infrastructure commit type" bullet under non-obvious patterns.
5. `CONTRIBUTING.md` appended with a "Releases" section.
6. An operator handoff: install the release-plz GitHub App, set `RELEASE_PLZ_APP_ID` + `RELEASE_PLZ_APP_PRIVATE_KEY` repo secrets. Do **not** add `CARGO_REGISTRY_TOKEN` yet (Stage 1 owns it).

**Non-goals.**

- Real crates.io publishing — `publish = false` is set workspace-wide; Stage 1 flips it.
- Setting `CARGO_REGISTRY_TOKEN` — Stage 1 owns the secret rollout.
- Enforcing Conventional Commits — SMA-335 owns the lint gates. release-plz tolerates non-CC commits (they get no bump), so SMA-307 lands cleanly without SMA-335.
- Branch protection allow-listing — SMA-309 owns the `release-plz[bot]` bypass actor.
- Updating the SBOM workflow's tag-glob trigger to match release-plz's crate-prefixed tags (`<crate>-v*`). The current `tags: [v*]` trigger won't fire on release-plz tags, but with `publish = false` there are no real release-plz events to miss yet. Listed as a Stage-1 follow-up in §8.
- Stable 1.0 release of any crate, custom GitHub release templates, sigstore signing, or release-note customization.
- Per-crate `[[package]]` boilerplate for the stub crates (they inherit workspace defaults; nothing to override).

**Required-status-check IDs produced for SMA-309.** None. The release-plz workflow runs only on push to `main`, not on PRs, so it produces no PR-gating checks. release-plz's own release PRs are gated by the existing required checks (`ci / fmt`, `ci / clippy`, `ci / test (ubuntu-latest, stable)`, `ci / docs`, `ci / doc-coverage`, `audit / audit`, `deny / deny`) — which is only true if release-plz authenticates as a non-default identity, so we use a GitHub App token (see §4).

## 2. File layout

```text
release-plz.toml                                    (new — workspace root)
.github/workflows/release-plz.yml                   (new — two-job workflow)
Cargo.toml                                          (unchanged at workspace level;
                                                     `workspace.package.version`
                                                     stays as the safety-net default)
crates/<each>/Cargo.toml                            (modified — add `version = "0.0.0"`,
                                                     remove `version.workspace = true`
                                                     if present)
CLAUDE.md                                           (modified — inheritance carve-out
                                                     + release-infra commit type)
CONTRIBUTING.md                                     (modified — append "Releases")
```

No changes to any crate's `src/`, to `scripts/`, to existing workflows (`ci.yml`, `msrv.yml`, `audit.yml`, `deny.yml`, `sbom.yml`), to `deny.toml`, or to `.github/dependabot.yml`.

`release-plz.toml` lives at the workspace root because release-plz discovers it via `cargo metadata`'s `workspace_root`, and a sibling location keeps the policy visible next to `Cargo.toml` (same rationale as `deny.toml`'s placement).

`.github/workflows/release-plz.yml` is one file with two jobs (not two workflow files) because the two jobs share triggers and state — the release-PR job needs to run *after* the release job processes any merged release PR, which is naturally expressed via `needs:` inside one workflow.

## 3. The `version` carve-out

### 3.1 What changes

Today every crate's `version` comes from `[workspace.package].version = "0.0.0"` in the root `Cargo.toml`, either implicitly or via `version.workspace = true`. release-plz needs each crate to own its own `version` line so it can bump them independently.

The minimum-invasive diff per crate Cargo.toml:

```diff
 [package]
 name        = "paigasus-helikon-core"
 description = "Core traits and types for the Paigasus AI SDK"
-# (version was inherited)
+version     = "0.0.0"
 edition.workspace      = true
 rust-version.workspace = true
 # ...all other workspace inheritance stays
```

Some crates currently set `version.workspace = true` explicitly; that line is replaced with `version = "0.0.0"`. Crates that omitted the line entirely (relying on the workspace default) get a new `version = "0.0.0"` line added.

### 3.2 Why the workspace-level default stays

`workspace.package.version = "0.0.0"` is kept in the root `Cargo.toml`. It's no longer load-bearing for existing crates (every crate now sets its own), but it serves as a safety net: if someone adds a new crate and forgets `version`, cargo falls back to the workspace default rather than failing with an obscure manifest error. The cost of leaving it in is zero.

### 3.3 Why the facade's `[workspace.dependencies]` entries don't change

The root `Cargo.toml` already pins each internal crate at `version = "0.0.0"` inside `[workspace.dependencies]`:

```toml
paigasus-helikon-core = { path = "crates/paigasus-helikon-core", version = "0.0.0" }
```

These stay as-is. When release-plz bumps `paigasus-helikon-core` to (say) `0.1.0`, its `dependencies_update = true` setting (§4) updates this line in lockstep and bumps the facade itself (typically patch).

### 3.4 Alternatives considered

- **Keep `version.workspace = true` and use lockstep workspace releases.** Rejected: conflicts with the ticket's per-crate acceptance criterion (`feat(core):` bumps only `paigasus-helikon-core`). Lockstep is also awkward when only one crate gains a feature.
- **Drop `workspace.package.version` entirely.** Rejected: removing the default means a new crate that forgets `version` fails to build with a non-obvious error. The dead-code default is cheap insurance.

## 4. `release-plz.toml`

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

**Settings deliberately left at upstream defaults:**

- `semver_check` (default `true`). release-plz runs `cargo-semver-checks` against each crate's public API. With the stub crates having effectively no surface, the check is a no-op today and adds ~30s per release-plz run. Worth keeping enabled because (1) it never fails builds (it annotates the release PR), and (2) flipping it on later, *after* writing real API, is the wrong direction.
- `pr_branch_prefix` (default `release-plz-<timestamp>`). The default branch name violates the SMA-309 ruleset `^(feature|hotfix)\/...`, which is exactly why SMA-309 lists `release-plz[bot]` as a bypass actor. Don't fight upstream defaults without cause.
- `git_release_enable` (default `true`). GitHub releases are exactly what we want.
- No per-crate `[[package]]` blocks for the nine stubs. They inherit workspace defaults and won't accrue release PRs until a `feat(<scope>):` commit lands targeting them.

## 5. `.github/workflows/release-plz.yml`

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

### 5.1 Why a GitHub App token (not `GITHUB_TOKEN`, not a PAT)

PRs opened with the default `GITHUB_TOKEN` do **not** trigger workflows on the same repo (GitHub's anti-recursion safety). If release-plz used `GITHUB_TOKEN`, the release PRs it opened would have zero CI runs — no `ci`, no `audit`, no `deny`. Under SMA-309's required-status-check rules that would render every release PR unmergeable.

Two ways to authenticate as an identity *other than* the default token, so PRs trigger workflows:

1. **GitHub App** ([release-plz App](https://github.com/apps/release-plz)). Scoped to this repo, no rotating credential, identity is the App's bot user (which SMA-309 lists as a bypass actor for the branch-name ruleset). Workflow uses `actions/create-github-app-token@v1` with two repo secrets (`RELEASE_PLZ_APP_ID`, `RELEASE_PLZ_APP_PRIVATE_KEY`) to mint a per-job installation token.
2. **Fine-grained PAT.** Simpler one-time setup. Tied to a personal account; expires (12-month max on fine-grained PATs).

This design chose the GitHub App. The PAT fallback is documented in CONTRIBUTING.md "Releases" but not configured.

### 5.2 Why the jobs are sequential (`needs`)

If a release PR was just merged into `main`, the `release` job tags + creates GitHub releases, *then* the `pr` job recomputes the next rolling PR from the new baseline. Reversed, the rolling PR would race against the tagging and could include or exclude the just-released commits unpredictably.

### 5.3 Other decisions

- **`fetch-depth: 0`.** release-plz reads the full commit graph to determine bumps. Shallow clones break it silently.
- **`CARGO_REGISTRY_TOKEN`.** Referenced in the workflow but never read in this PR — the action only consults it during the publish step, which is skipped because `publish = false`. Wiring it now means Stage 1 just adds the secret and removes one line in `release-plz.toml`; no workflow edits.
- **No `Swatinem/rust-cache`.** release-plz doesn't build the workspace; it parses manifests and runs `cargo-semver-checks` against rmeta artifacts. The relevant cache (cargo-semver-checks binary) is handled internally by the release-plz action. Adding workspace-level caching adds complexity for negligible savings at this scale. Easy to add later if runs get slow.
- **`cancel-in-progress: false`.** Same reasoning as `sbom.yml` — cancelling mid-tag-push or mid-PR-update would leave inconsistent state (a partial tag set, an orphaned PR draft).

## 6. Smoketest & acceptance verification

This section is the explicit plan for satisfying the ticket's acceptance criteria. Two subtle gotchas need handling — both stem from release-plz considering the *entire* git history when no per-crate tags exist to anchor on.

### 6.1 Gotcha 1 — historical `feat:` commits

`git log --oneline` on `main` includes three `feat:` commits from SMA-304:

```text
65a061f feat(facade): SMA-304 add paigasus-helikon facade ...
8b15ea0 feat(cli):    SMA-304 add cli crate ...
11db17d feat(evals):  SMA-304 add paigasus-helikon-evals stub crate
```

Without baseline tags, release-plz's first run produces a "kitchen sink" PR proposing bumps for `paigasus-helikon` (facade), `paigasus-helikon-cli`, and `paigasus-helikon-evals` from these historical commits. That has nothing to do with the SMA-307 acceptance criterion and pollutes the first real release.

### 6.2 Gotcha 2 — bootstrap commits touching every Cargo.toml

The §3 carve-out modifies every `crates/*/Cargo.toml` to add `version = "0.0.0"`. If any of those commits uses `feat` or `fix` as its type, release-plz attributes a bump to every crate whose Cargo.toml was touched — the whole workspace.

### 6.3 Mitigation: chore-typed bootstrap + baseline tags

**Part A — all bootstrap commits use `chore(...)` or `docs(...)` types.** The full sequence on the SMA-307 branch: `chore(release): SMA-307 add release-plz workflow and config`, `chore(workspace): SMA-307 break version inheritance to per-crate`, `docs(claude): SMA-307 document version carve-out`, `docs(contributing): SMA-307 add Releases section`. No `feat` or `fix` in this PR. release-plz ignores `chore`/`docs` for bump computation.

**Part B — create 13 baseline tags at the bootstrap merge commit.** Immediately after the SMA-307 bootstrap PR merges:

```bash
git checkout main && git pull
MERGE_SHA="$(git rev-parse HEAD)"
for crate in paigasus-helikon paigasus-helikon-core paigasus-helikon-cli \
             paigasus-helikon-evals paigasus-helikon-macros paigasus-helikon-mcp \
             paigasus-helikon-providers-anthropic paigasus-helikon-providers-openai \
             paigasus-helikon-runtime-agentcore paigasus-helikon-runtime-axum \
             paigasus-helikon-runtime-temporal paigasus-helikon-runtime-tokio \
             paigasus-helikon-tools; do
  git tag "${crate}-v0.0.0" "${MERGE_SHA}"
done
git push origin --tags
```

These tags declare "every crate is at `0.0.0` as of the bootstrap merge; only count commits after this point." The `<crate>-v*` tag format does **not** match `sbom.yml`'s `v*` glob, so baseline-tag creation triggers no other workflows.

### 6.4 Verification 1 — plumbing works, no spurious bumps

Sequence:

1. Bootstrap PR merges.
2. **Operator runs the baseline-tag script within ~30 seconds of merge.**
3. release-plz workflow runs on push to `main`. Both jobs find zero commits since `<crate>-v0.0.0` for every crate. Outcome: workflow succeeds, no release PR opens.

**Fallback if release-plz beats the operator to the punch.** If release-plz opens a kitchen-sink PR before the baseline tags exist: close the PR, run the baseline-tag script, push the tags, and retrigger the workflow (push an empty commit `git commit --allow-empty -m "chore: SMA-307 retrigger release-plz post-baseline"`, or trigger via `workflow_dispatch` if added — not in the initial spec; would require a tweak to the workflow file).

### 6.5 Verification 2 — `feat(core):` produces a core bump

Sequence:

1. Open a follow-up branch `feature/sma-307-smoketest-core-feat` with one trivial change: append a docstring sentence to the crate-root `//!` comment in `crates/paigasus-helikon-core/src/lib.rs`.
2. Commit message: `feat(core): SMA-307 add release-plz smoketest docstring`.
3. Squash-merge into `main` (the squash-merge commit must keep the `feat(core):` type — set the PR title accordingly).
4. release-plz workflow runs. `release-plz-pr` sees one `feat` commit affecting `paigasus-helikon-core` since `paigasus-helikon-core-v0.0.0`. Proposes a release PR: `paigasus-helikon-core` bumped (release-plz pre-1.0 semantics), facade `paigasus-helikon` bumped patch via `dependencies_update`, both `CHANGELOG.md` files generated.

This satisfies the ticket's acceptance criterion: *"A test commit `feat(core): add foo` produces a release PR that bumps `paigasus-helikon-core` and updates `CHANGELOG.md`."*

**Recommendation: merge the smoketest release PR.** Because `publish = false`, merging publishes nothing to crates.io but does create the per-crate git tags and a GitHub release. This becomes the new baseline for Stage 1 to build on. Alternative: close the smoketest release PR and revert the smoketest commit — leaves the workspace at the `0.0.0` baseline. Either is acceptable; the recommendation is to merge.

### 6.6 Trade-off note

The baseline-tag ceremony is one-time operator burden (~13 tags, one shell loop). Alternative: let release-plz roll up history into a "first release" PR and merge it as-is. Rejected because that leaves `paigasus-helikon-core` (which had no `feat:` in history) at `0.0.0` while facade/cli/evals jump to `0.1.0` — an inconsistent starting state that obscures the smoketest signal.

## 7. CLAUDE.md & CONTRIBUTING.md updates

### 7.1 `CLAUDE.md`

Two surgical edits.

**(a)** The "Workspace layout" paragraph that begins *"Workspace inheritance is **mandatory**..."* gets one sentence appended:

> *Exception: `version` is per-crate (set explicitly in each `crates/*/Cargo.toml`) so release-plz can bump crates independently. The `workspace.package.version = "0.0.0"` default in the root `Cargo.toml` stays as a safety net for new crates that forget to declare their own.*

**(b)** A new bullet under "Non-obvious patterns to preserve":

> **Bootstrap commits on release infrastructure must use `chore(...)` or `docs(...)` types**, never `feat`/`fix`. release-plz parses every commit since the last per-crate tag — a `feat(workspace): ...` commit that touches every `Cargo.toml` would attribute a bump to every crate. The SMA-307 bootstrap PR followed this rule; the same rule applies to any future `release-plz.toml` or `release-plz.yml` edits.

### 7.2 `CONTRIBUTING.md`

A new "Releases" section appended after "Supply-chain security":

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

## 8. Testing & verification plan

| What | How |
|---|---|
| Manifest changes don't break anything | `cargo build --workspace --all-features` succeeds locally and in CI |
| Cargo accepts the per-crate `version` lines | `cargo metadata --format-version 1 --no-deps > /dev/null` (parses, no errors) |
| Existing CI gates still pass on the PR | All of `fmt`, `clippy`, `test`, `docs`, `doc-coverage`, `audit`, `deny`, `msrv` green |
| `release-plz.toml` is syntactically valid | Operator runs `cargo install release-plz --locked && release-plz check-config` locally (not added to CI to avoid pulling a heavy tool for one check) |
| Workflow YAML is valid | Implicit: GitHub rejects malformed workflows on push; visible on the PR Checks tab |
| App token integration works | Post-merge: the first `release-plz` workflow run on `main` completes without auth errors (operator-observed) |
| Baseline tags suppress kitchen-sink PR (Verification 1, §6.4) | After tag push, the next release-plz run opens no PR |
| Acceptance: `feat(core):` produces a core bump (Verification 2, §6.5) | Smoketest PR lands → release PR proposes core + facade bumps with `CHANGELOG.md` entries |

There's no per-PR check that the release-plz config produces sensible output, because release-plz only runs on `main`. The §6 smoketest is the only end-to-end verification, and it intentionally happens *after* merge.

## 9. Operator runbook (post-merge)

In order, immediately after the SMA-307 bootstrap PR merges:

1. **Install the [release-plz GitHub App](https://github.com/apps/release-plz)** on the `paigasus-helikon` repo (Settings → GitHub Apps → Install). Note the App ID shown in the install confirmation page.
2. **Add repo secrets** (Settings → Secrets and variables → Actions → New repository secret):
   - `RELEASE_PLZ_APP_ID` = the App ID from step 1.
   - `RELEASE_PLZ_APP_PRIVATE_KEY` = the App's private key PEM (download from the App settings page; paste the entire PEM including `-----BEGIN`/`-----END` lines).
3. **Run the baseline-tag script** (§6.3, Part B) within ~30 seconds of the bootstrap merge.
4. **Observe the first `release-plz` workflow run** on `main`. Expected: both jobs succeed in <2 minutes. No release PR is opened.
5. **Push the smoketest PR** (§6.5). Expected: the release-plz workflow runs again and a release PR appears within ~1 minute.

If step 3 is missed (release-plz opens a kitchen-sink PR first), follow the §6.4 fallback.

## 10. Open questions & Stage-1 follow-ups

- **SBOM trigger glob.** `sbom.yml` triggers on `tags: [v*]`. release-plz produces `<crate>-v<version>` tags, which don't match. Once Stage 1 enables real publishing, change `sbom.yml`'s trigger to `tags: ["paigasus-helikon-v*"]` (facade-only — the facade with `--all-features` already captures the workspace, per SMA-306's design). Out of scope for SMA-307 because `publish = false` means no real release-plz tags are produced yet.
- **`CARGO_REGISTRY_TOKEN` rollout.** Stage 1 ticket adds the secret. Workflow already references `secrets.CARGO_REGISTRY_TOKEN`, so the only required change at that point is removing `publish = false` from `release-plz.toml` (and from the per-crate `[[package]]` override on the CLI — that one stays).
- **`semver_check` noise.** If release-plz runs become slow once real API surface lands, revisit the default. No action needed now.
- **Auto-merging release-plz PRs.** Out of scope. Reviewers manually merge today.
- **release-plz `workflow_dispatch` trigger.** Not added in this PR. If the §6.4 fallback bites in practice, a one-line addition to the workflow (`workflow_dispatch:` under `on:`) makes manual retriggering trivial. Document as a follow-up if observed.
