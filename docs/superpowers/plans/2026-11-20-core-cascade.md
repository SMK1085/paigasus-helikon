# Breaking-cascade repair and guard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Republish the 17 core-0.7 dependents as breaking versions (plus facade 0.7.1 and CLI 0.1.45), then add a CI guard that fails a release whose breaking bump is not cascaded to the dependents.

**Architecture:** Part 1 (PR 1) is a hand bump: crate versions, root `[workspace.dependencies]` pins, `Cargo.lock`, hand-written CHANGELOG sections, two book pins. Its own merge publishes. Part 2 (PR 2, a new branch from `main` after PR 1 published and Sven yanked) adds `scripts/check-breaking-cascade.sh`, its self-test, two `fmt` steps, and the docs.

**Tech Stack:** Cargo workspace, release-plz v0.4.3, bash + jq, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-11-20-core-cascade-design.md`

Paths are relative to the worktree root
`/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-838`.
Guard every `cd` with `|| exit 1`. Never run `git checkout <branch>`, `git switch`,
`git reset --hard`, `git stash`, `git push` or `git add -A`. Stage explicit paths.
Work in the foreground until each command has a final status.

## Global Constraints

- **No file in `crates/paigasus-helikon-core/` or `crates/paigasus-helikon-macros/` changes, in either part.** A change there releases core 0.7.1 and restarts the patch cascade.
- Version table (Part 1), `old → new`: evals 0.1.22→0.2.0; mcp 0.1.26→0.2.0; providers-anthropic 0.1.30→0.2.0; providers-bedrock 0.1.11→0.2.0; providers-gemini 0.1.11→0.2.0; providers-litellm 0.1.11→0.2.0; providers-openai 0.2.34→0.3.0; runtime-actix 0.2.11→0.3.0; runtime-agentcore 0.2.17→0.3.0; runtime-axum 0.2.11→0.3.0; runtime-temporal 0.5.5→0.6.0; runtime-tokio 0.1.27→0.2.0; sessions-postgres 0.1.11→0.2.0; sessions-redis 0.1.10→0.2.0; sessions-sqlite 0.1.30→0.2.0; sessions-testkit 0.1.3→0.2.0; tools 0.2.26→0.3.0; `paigasus-helikon` (facade) 0.7.1→0.7.2; cli 0.1.45→0.1.46 (rebased after #369; see the spec's update note). All crate names carry the `paigasus-helikon-` prefix.
- PR 1 title: `chore(release): SMA-838 republish the core 0.7 dependents as breaking versions`.
- PR 2 title: `ci(release): SMA-838 fail a release that breaks a crate without breaking its dependents`.
- All prose (CHANGELOG, comments, docs) is ASD-STE100 Simplified Technical English.
- Commit trailer: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Commits are signed via 1Password; "failed to fill whole buffer" means the vault is locked: stop, do not bypass signing.
- The five core `trybuild_ui` mismatches under local rustc 1.99.0 are pre-existing; any other test failure is real.

## Review Focus

1. A sibling that depends on another sibling (evals → runtime-tokio, runtime-axum → runtime-tokio, cli → evals) must require the **new** version, or `cargo publish --verify` resolves a yanked or old version. Covered: Task 1 Step 3 (`cargo metadata` check of every internal dependency requirement).
2. A crate whose packaged files did not change still must not be released. Covered: Task 1 Step 5 (the diff guard against core and macros) and Task 3 (release-plz proof).
3. A PR that adds a new crate must pass the guard. Covered: Task 4 case `new_crate_skipped`.
4. A re-run of the `fmt` job must compare against the same base. Covered: Task 4 (base is `HEAD^1`, case `pull_request_uses_parent`).
5. The guard must not pass when `cargo metadata` lists no packages. Covered: Task 4 case `empty_package_list_fails`.

---

## Part 1 — PR 1 (repair), branch `feature/sma-838-release-the-dependents-of-a-core-minor-bump`

### Task 1: Hand-bump the versions, pins, lockfile and book

**Files:**
- Modify: `crates/<each of the 19 crates in the table>/Cargo.toml` (the `version` line only)
- Modify: `Cargo.toml` `[workspace.dependencies]` lines 189-207 (the sibling pins and the facade self-pin; core and macros pins stay)
- Modify: `Cargo.lock`
- Modify: `docs/book/src/concepts/model-providers.md:282` and `:402-410`

**Interfaces:**
- Produces: the branch state that Tasks 2 and 3 use.

- [ ] **Step 1: Set the 19 versions and the 18 workspace pins per the table.** `sessions-testkit` has no workspace pin (its users are path-only dev-dependencies); change only its own `version`.
- [ ] **Step 2: Update the book.** Line 282: `version = "0.1"` → `version = "0.2"`. Lines 402-410: each `version = "0.5"` → `version = "0.7"`.
- [ ] **Step 3: Check every internal requirement.**
Run: `cargo metadata --format-version 1 --no-deps | jq -r '.packages[] | .name as $p | .dependencies[] | select(.name|startswith("paigasus-helikon")) | select(.req != "*") | "\($p) -> \(.name) \(.req)"' | sort -u`
Expected: every requirement on a table crate names its **new** version (`^0.2.0`, `^0.3.0`, `^0.6.0`, `^0.7.1`); core stays `^0.7.0`, macros `^0.2.6`.
- [ ] **Step 4: Build, update the lockfile, run the gates.**
Run, each must exit 0: `cargo build --workspace --all-features`; `cargo fmt --all -- --check`; `cargo clippy --workspace --all-features --all-targets -- -D warnings`; `cargo build --workspace --no-default-features`; `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`; `mdbook build docs/book` (if `mdbook` is installed; otherwise record it as not run).
Expected: `git diff --stat Cargo.lock` shows only `paigasus-helikon*` version lines.
- [ ] **Step 5: Diff guard.**
Run: `git diff --name-only origin/main -- crates/paigasus-helikon-core crates/paigasus-helikon-macros`
Expected: no output.
- [ ] **Step 6: Commit** the 19 manifests, `Cargo.toml`, `Cargo.lock` and the book page with `chore(release): SMA-838 bump the core 0.7 dependents to breaking versions`.

### Task 2: Write the 19 CHANGELOG sections and fix the facade 0.7.0 note

**Files:**
- Modify: `crates/<each of the 19 crates>/CHANGELOG.md`

**Interfaces:**
- Consumes: the versions from Task 1.

- [ ] **Step 1: Insert one section per crate directly under `## [Unreleased]`**, one blank line above and below, heading in release-plz's format:
`## [<new>](https://github.com/SMK1085/paigasus-helikon/compare/<crate>-v<old>...<crate>-v<new>) - 2026-11-20`

For the 17 dependents, the body is (fill `<crate>`, `<old>`, `<new>`, `<req>` = new version without the patch, for example `0.2`):

```markdown
### Breaking changes

- This version replaces `<old>`. Version `<old>` requires `paigasus-helikon-core` 0.7, but it was released as a patch, so `cargo update` gave it to users who stay on core 0.6. Version `<old>` is yanked. The content of this version is the same as `<old>`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, keep the previous version. After the yank, `cargo update` selects it.
- If you already use core 0.7, change your requirement on `<crate>` to `"<req>"`. If you do not, `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.
```

For the facade (`paigasus-helikon`, 0.7.0 → 0.7.1):

```markdown
### Other

- Moves to the new breaking versions of the other `paigasus-helikon-*` crates, which replace the patch versions that release 0.7.0 used. Version 0.7.0 is yanked.
- Each re-exported crate is a new major version (for example `paigasus-helikon-providers-openai` 0.2 → 0.3), so its types are different types. This is released as a patch only because 0.7.0 is yanked. The API is the same as in 0.7.0.
```

For the CLI (0.1.44 → 0.1.45):

```markdown
### Other

- Moves to the new breaking versions of the `paigasus-helikon-*` library crates. Version 0.1.44 needs versions that are now yanked, so it installs only with `cargo install --locked`.
```

- [ ] **Step 2: Fix the facade 0.7.0 note.** In `crates/paigasus-helikon/CHANGELOG.md`, replace the bullet that starts with `- The next patch release of each other` (line 22 before Step 1) with:
`- Each other \`paigasus-helikon-*\` library crate has a new breaking version that requires core 0.7 (see 0.7.1). If your \`Cargo.toml\` requires \`paigasus-helikon-core\` 0.6 directly, change it to \`0.7\`, and move the other crates to their new versions. Two copies of core in one build cause errors such as "trait not implemented". Run \`cargo tree -d\` to find the copies.`
- [ ] **Step 3: Check.**
Run: `npx markdownlint-cli2` (exit 0) and `git diff --name-only origin/main -- crates/paigasus-helikon-core crates/paigasus-helikon-macros` (no output). Then for each of the 19 crates confirm the new heading's version equals its `Cargo.toml` version: `for d in crates/*/; do v=$(grep -m1 '^version' $d/Cargo.toml | cut -d'"' -f2); h=$(grep -m1 '^## \[[0-9]' $d/CHANGELOG.md | sed 's/^## \[\([^]]*\)\].*/\1/'); [ "$v" = "$h" ] || echo "MISMATCH $d $v $h"; done`
Expected: no `MISMATCH` line other than core and macros (which keep their 0.7.0 / 0.2.6 sections and match anyway).
- [ ] **Step 4: Commit** the 19 CHANGELOGs with `docs(release): SMA-838 add the changelog sections for the republished crates`.

### Task 3: Prove the publish set before the PR (Opus, no repo change)

**Files:** none in the repo. Scratch dir: `/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/9134c40b-e34c-4e03-aba6-d1c143283d73/scratchpad/sma-838-rp`. release-plz v0.4.3 is already installed at `…/scratchpad/sma-831-rp/tools/bin`.

- [ ] **Step 1:** Clone the worktree into the scratch dir; fetch GitHub `main` with tags into `refs/remotes/gh/main`; create a local branch `main` at `gh/main` (release-plz reads the local `main`); apply the branch diff `gh/main...<branch-head>` as one commit with the PR 1 title plus ` (#999)` and no body.
- [ ] **Step 2:** Run `release-plz update` (expect: no crate version changes, no CHANGELOG changes). Try `release-plz release --dry-run`; if it runs without a token, record the publish list and order (expect exactly the 19 table crates, dependencies first). If it needs a token, record that and rely on `update`.
- [ ] **Step 3:** Record the results in the task report for the PR body.

---

## Part 2 — PR 2 (guard and rule)

Start only after PR 1 has published all 19 versions and Sven has run the yanks.
New worktree from current `main`, branch `feature/sma-838-breaking-cascade-guard`.

### Task 4: The guard script and its self-test (TDD)

**Files:**
- Create: `scripts/check-breaking-cascade.sh`
- Create: `scripts/check-breaking-cascade-selftest.sh`

**Interfaces:**
- Produces: `bash scripts/check-breaking-cascade.sh` — exit 0 pass, exit 1 violation or loud failure, with messages on stderr. Environment overrides (used by the self-test; in CI they are unset):
  - `CASCADE_EVENT` — overrides `GITHUB_EVENT_NAME`.
  - `CASCADE_METADATA` — path to a `cargo metadata --no-deps --format-version 1` JSON for the head (default: run `cargo metadata`).
  - `CASCADE_BASE_VERSIONS` — path to a file of `<name> <version>` lines for the base (default: read each head package's `manifest_path`, relative to the repo root, at the base revision with `git show <base>:<path>` and take its `version`).
  - `CASCADE_BASE_REV` — overrides the base revision.
- Base revision default: `pull_request` → `HEAD^1`; `push` → print `notice: push event, no PR base; skipping` and exit 0; empty event (local run) → `git merge-base origin/main HEAD`; any other event → fail.

- [ ] **Step 1: Write the self-test first**, in the style of `scripts/check-release-gaps-selftest.sh` (mktemp fixtures, a `case` helper that runs the script with overrides and checks the exit code and a substring of the output, a final failure count). Fixture metadata has core, a dependent `dep-a` (normal dependency on core), a facade `facade` (normal dependency on core and `dep-a`), `paigasus-helikon-cli` (normal dependency on core), an unpublished member (`publish: []`) and a dev-only dependent of core. Cases (name → event, base → head, expected):

| Case | Change | Expect |
|---|---|---|
| `no_version_change` | nothing | exit 0 |
| `core_patch_dependents_patch` | core 0.7.0→0.7.1, dep-a/facade patch | exit 0 |
| `core_breaking_all_breaking` | core 0.7→0.8, dep-a 0.2→0.3, facade 0.7→0.8 | exit 0 |
| `core_breaking_dependent_patch` | core 0.7→0.8, dep-a 0.2.0→0.2.1 | exit 1, output names `dep-a` and `0.3.0` |
| `cli_exempt` | core 0.7→0.8, dependents breaking, cli patch | exit 0 |
| `sibling_breaking_facade_patch` | dep-a 0.2→0.3, facade 0.7.0→0.7.1 | exit 1, names `facade` |
| `major_scheme` | core 0.9.0→1.0.0, dep-a 1.2.0→1.3.0 | exit 1, names `dep-a` and `2.0.0` |
| `version_down` | dep-a 0.2.0→0.1.9 | exit 1, output contains `goes down` |
| `new_crate_skipped` | new crate absent at base | exit 0 |
| `unreadable_base` | `CASCADE_BASE_VERSIONS` points to a missing file | exit 1 |
| `empty_package_list_fails` | metadata with no publishable packages | exit 1 |
| `push_event_skips` | `CASCADE_EVENT=push` | exit 0, output contains `notice` |
| `unknown_event_fails` | `CASCADE_EVENT=schedule` | exit 1 |
| `dev_and_unpublished_ignored` | core breaking; dev-only dependent and unpublished member patch; others breaking | exit 0 |

- [ ] **Step 2: Run it.** `bash scripts/check-breaking-cascade-selftest.sh` → FAIL (script missing).
- [ ] **Step 3: Implement the script** per the Interfaces block and spec "PR 2". Minimum version for a breaking bump of `old`: `0.y.z` → `0.(y+1).0`; `x.y.z` with `x ≥ 1` → `(x+1).0.0`. The failure message for a violation ends with the fix text: open a `!` PR that adds an upgrade note under `## [Unreleased]` in each listed crate's CHANGELOG, and let release-plz regenerate the release PR.
- [ ] **Step 4: Run** the self-test → all pass. Run `bash scripts/check-breaking-cascade.sh` locally on the branch → exit 0 (no version changes on this branch).
- [ ] **Step 5: Commit** both scripts with `ci(release): SMA-838 add the breaking-cascade guard and its self-test`.

### Task 5: Wire the guard into CI and write the rule down

**Files:**
- Modify: `.github/workflows/ci.yml` (`fmt` job: `fetch-depth: 2` on the checkout; two steps after `Self-test the release gap report`)
- Modify: `docs/runbooks/release-ascend.md` (new section; correct the `dependencies_update` text near `:40`)
- Modify: `CLAUDE.md` (release caveats; `dependencies_update` text)
- Modify: `release-plz.toml:5-9` (comment only)

- [ ] **Step 1: ci.yml.** Add `fetch-depth: 2` under the `fmt` checkout's `with:`. Add steps `Self-test the breaking-cascade guard` (`bash scripts/check-breaking-cascade-selftest.sh`) and `Check that a breaking bump reaches every dependent` (`bash scripts/check-breaking-cascade.sh`), each with a short comment that names SMA-838. The `fmt` job has no Rust cache, so the CARGO_/RUST env-sync guard is unaffected; still run `bash scripts/check-cargo-profile-env-sync.sh` and its self-test.
- [ ] **Step 2: Runbook section** "A breaking change in a crate with dependents": the rule; the procedure (`!` title plus an upgrade note under `## [Unreleased]` in each dependent's CHANGELOG); what the guard checks and how to read its failure; the repair procedure (hand-bump to the next breaking version, publish, then yank) with the SMA-838 incident as the example; "never touch core or macros files in a repair". Correct the `dependencies_update` sentence: it only makes release-plz run `cargo update` on the lockfile.
- [ ] **Step 3: CLAUDE.md.** In "Workspace layout" caveats, add one bullet: a breaking bump of any crate needs a breaking bump of each dependent; the `fmt` guard enforces it; see the runbook section. Correct the `dependencies_update` description wherever `CLAUDE.md` calls it a cascade.
- [ ] **Step 4: release-plz.toml** comment above `dependencies_update`: say it makes the release PR run `cargo update` on `Cargo.lock`, and that release-plz gives a dependent without own commits a patch bump only (release-plz#1599).
- [ ] **Step 5: Check.** `npx markdownlint-cli2`; `bash scripts/check-markdownlint-config.sh`; `bash scripts/check-cargo-profile-env-sync.sh`; `bash scripts/check-cargo-profile-env-sync-selftest.sh`; `bash scripts/check-breaking-cascade-selftest.sh` — all exit 0. `git diff --name-only origin/main -- crates/` → no output.
- [ ] **Step 6: Commit** with `ci(release): SMA-838 run the breaking-cascade guard in fmt and document the rule`.
