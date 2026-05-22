# SMA-350 — Unblock release-plz for paigasus-helikon-macros + migrate app-token to client-id — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land a single PR that (a) bumps `paigasus-helikon-macros` from `0.0.0` to `0.1.0` (and the workspace pin to match) to escape the SMA-347 release-plz trap, and (b) migrates both `actions/create-github-app-token@v3` steps in `release-plz.yml` from the deprecated `app-id` input to `client-id`. After merge, the next release-plz workflow run must propose a release PR and emit no `app-id` deprecation annotation.

**Architecture:** Two unrelated micro-fixes bundled into one PR. Two commits on the feature branch (`chore(release): SMA-350 …` for the manifest bumps; `ci(release-plz): SMA-350 …` for the workflow YAML), squash-merged to one `chore(release): SMA-350 …` commit on `main`. An additive new GitHub secret (`RELEASE_PLZ_APP_CLIENT_ID`) is created by the operator before merge; the old `RELEASE_PLZ_APP_ID` secret stays in place for trivial rollback.

**Tech Stack:** Rust workspace (cargo), GitHub Actions, `actions/create-github-app-token@v3`, release-plz v0.5 GitHub Action.

**Branch:** `feature/sma-350-unblock-release-plz-for-paigasus-helikon-macros-migrate-app` (already created and checked out; the design doc `docs/superpowers/specs/2026-05-22-sma-350-release-plz-unblock-design.md` is committed as `3fe672c` on this branch).

**Spec:** [`docs/superpowers/specs/2026-05-22-sma-350-release-plz-unblock-design.md`](../specs/2026-05-22-sma-350-release-plz-unblock-design.md)

---

## Pre-flight

### Task 0: Confirm baseline

**Files:** none modified.

- [ ] **Step 0.1: Confirm the working tree is clean and on the feature branch**

Run:
```bash
git status
```
Expected output (exact branch name; "nothing to commit" line):
```
On branch feature/sma-350-unblock-release-plz-for-paigasus-helikon-macros-migrate-app
nothing to commit, working tree clean
```

If you are not on this branch, run:
```bash
git switch feature/sma-350-unblock-release-plz-for-paigasus-helikon-macros-migrate-app
```

If the branch does not exist yet, run:
```bash
git switch -c feature/sma-350-unblock-release-plz-for-paigasus-helikon-macros-migrate-app
```
(The spec commit lives on this branch — if you have to create it from `main`, the spec is missing. Stop and investigate before continuing.)

- [ ] **Step 0.2: Confirm the workspace builds on the baseline**

Run:
```bash
cargo build --workspace --all-features
```
Expected: `Finished … target(s) in …s` with no errors. This establishes that any build failure after Task 1 is downstream of the version bump, not pre-existing.

- [ ] **Step 0.3: Confirm the spec commit is in place**

Run:
```bash
git log --oneline -3
```
Expected: the top commit is `<sha> docs(spec): SMA-350 add design for release-plz unblock and client-id migration`. The two commits below are the `main` HEAD at the time of branching.

---

## Task 1: Bump paigasus-helikon-macros to 0.1.0

**Files:**
- Modify: `crates/paigasus-helikon-macros/Cargo.toml:4`
- Modify: `Cargo.toml:42`

**Why:** The crate is stuck at `0.0.0` and release-plz reads the matching `v0.0.0` tag as "already published." Bumping to `0.1.0` mirrors the SMA-347 escape pattern. The workspace-pin update is mechanically required — cargo's caret rule for `^0.0.0` is effectively `=0.0.0`, so a member at `0.1.0` with the workspace pin still at `0.0.0` would fail resolution.

- [ ] **Step 1.1: Update the crate's own version**

In `crates/paigasus-helikon-macros/Cargo.toml`, change line 4:

```diff
 [package]
 name        = "paigasus-helikon-macros"
 description = "Proc macros for the Paigasus Helikon AI SDK."
-version                = "0.0.0"
+version                = "0.1.0"
 edition.workspace      = true
```

Resulting line 4 (preserve the original whitespace alignment — `version` is followed by `<spaces>= "0.1.0"`, matching the neighbouring inherited lines):

```toml
version                = "0.1.0"
```

- [ ] **Step 1.2: Update the workspace dependency pin**

In `Cargo.toml` (workspace root), change line 42:

```diff
 paigasus-helikon-core                = { path = "crates/paigasus-helikon-core",                version = "0.1.0" }
-paigasus-helikon-macros              = { path = "crates/paigasus-helikon-macros",              version = "0.0.0" }
+paigasus-helikon-macros              = { path = "crates/paigasus-helikon-macros",              version = "0.1.0" }
 paigasus-helikon-providers-openai    = { path = "crates/paigasus-helikon-providers-openai",    version = "0.0.0" }
```

Preserve column alignment with the surrounding lines.

- [ ] **Step 1.3: Verify cargo resolves and the workspace still builds**

Run:
```bash
cargo build --workspace --all-features
```
Expected: `Finished … target(s) in …s` with no errors. If cargo emits a "no matching package named `paigasus-helikon-macros` found" or similar, the workspace-pin update in Step 1.2 was missed.

- [ ] **Step 1.4: Re-run the full CI gate set locally**

Run (sequentially — each command must exit 0):
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
```
Expected: all three exit 0. `clippy -D warnings` is the gate that would catch any incidental issue introduced by the version change (none are expected, but the macros crate produces user-facing macros so it is worth re-running).

The two heavier CI-only gates (`RUSTDOCFLAGS="-D warnings" cargo doc …` and the doc-coverage script) can be deferred to CI for this PR — there is no surface area in this PR that affects rustdoc or doc-coverage.

- [ ] **Step 1.5: Stage and commit**

Run:
```bash
git add crates/paigasus-helikon-macros/Cargo.toml Cargo.toml
git status
```
Expected `git status` output:
```
On branch feature/sma-350-unblock-release-plz-for-paigasus-helikon-macros-migrate-app
Changes to be committed:
  (use "git restore --staged <file>..." to unstage)
	modified:   Cargo.toml
	modified:   crates/paigasus-helikon-macros/Cargo.toml
```

Commit:
```bash
git commit -m "$(cat <<'EOF'
chore(release): SMA-350 bump paigasus-helikon-macros to 0.1.0

paigasus-helikon-macros is the third Stage-1 crate to escape the
SMA-347 release-plz 0.0.0 trap. SMA-315 just shipped its first real
public API (the `#[tool]` proc-macro and the `tools![ ... ]`
companion); the matching v0.0.0 git tag from the very first release-plz
PR is otherwise interpreted as "already published — nothing to do" and
release-plz never proposes the first bump.

Mirrors the SMA-347 pattern for core and the facade: the per-crate
manifest moves to 0.1.0 and the [workspace.dependencies] pin moves in
lockstep (cargo's caret rule for ^0.0.0 is effectively =0.0.0, so the
pin must follow).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: the commit-msg hook prints OK, and the commit lands. If the hook complains about the scope, the allowlist accepts `release` (this matches the SMA-347 precedent commit `5286b56`).

- [ ] **Step 1.6: Confirm the commit shape**

Run:
```bash
git log --oneline -2
```
Expected top two entries:
```
<sha> chore(release): SMA-350 bump paigasus-helikon-macros to 0.1.0
<sha> docs(spec): SMA-350 add design for release-plz unblock and client-id migration
```

---

## Task 2: Migrate release-plz.yml from `app-id` to `client-id`

**Files:**
- Modify: `.github/workflows/release-plz.yml:27-31` (release-plz-release job's app-token step)
- Modify: `.github/workflows/release-plz.yml:49-53` (release-plz-pr job's app-token step)

**Why:** `actions/create-github-app-token@v3` deprecated the `app-id` input — the previous run emitted `Input 'app-id' has been deprecated with message: Use 'client-id' instead.` v4 is expected to remove the input. Switching now also rotates the secret name from `RELEASE_PLZ_APP_ID` to a new additive `RELEASE_PLZ_APP_CLIENT_ID` for safer rollback (the design doc's §3 decision row).

- [ ] **Step 2.1: Update the `release-plz-release` job**

In `.github/workflows/release-plz.yml`, lines 27–31:

```diff
       - uses: actions/create-github-app-token@v3
         id: app-token
         with:
-          app-id: ${{ secrets.RELEASE_PLZ_APP_ID }}
+          client-id: ${{ secrets.RELEASE_PLZ_APP_CLIENT_ID }}
           private-key: ${{ secrets.RELEASE_PLZ_APP_PRIVATE_KEY }}
```

`private-key` stays exactly as written.

- [ ] **Step 2.2: Update the `release-plz-pr` job**

In `.github/workflows/release-plz.yml`, lines 49–53 — identical swap:

```diff
       - uses: actions/create-github-app-token@v3
         id: app-token
         with:
-          app-id: ${{ secrets.RELEASE_PLZ_APP_ID }}
+          client-id: ${{ secrets.RELEASE_PLZ_APP_CLIENT_ID }}
           private-key: ${{ secrets.RELEASE_PLZ_APP_PRIVATE_KEY }}
```

- [ ] **Step 2.3: Diff-check both edits**

Run:
```bash
git diff .github/workflows/release-plz.yml
```
Expected: exactly two `-app-id` / `+client-id` hunks, both with `RELEASE_PLZ_APP_ID` → `RELEASE_PLZ_APP_CLIENT_ID` on the value side. Nothing else changes. If you see only one hunk, you missed the second job. If you see anything touching `private-key` or the action's `uses:` line, you over-edited — revert and redo.

- [ ] **Step 2.4: Verify the YAML still parses**

Run:
```bash
python3 -c "import yaml, sys; yaml.safe_load(open('.github/workflows/release-plz.yml')); print('ok')"
```
Expected: `ok`.

(If `python3` is unavailable, `actionlint .github/workflows/release-plz.yml` is the preferred check if you have it installed. If neither is available, skip — GitHub Actions itself will reject malformed YAML on push, and the diff in 2.3 is small enough that visual inspection is sufficient.)

- [ ] **Step 2.5: Stage and commit**

Run:
```bash
git add .github/workflows/release-plz.yml
git status
```
Expected:
```
On branch feature/sma-350-unblock-release-plz-for-paigasus-helikon-macros-migrate-app
Changes to be committed:
  (use "git restore --staged <file>..." to unstage)
	modified:   .github/workflows/release-plz.yml
```

Commit:
```bash
git commit -m "$(cat <<'EOF'
ci(release-plz): SMA-350 migrate app-token action from app-id to client-id

actions/create-github-app-token@v3 deprecated the app-id input in favour
of client-id; v4 is expected to remove app-id entirely. The previous
release-plz run emitted `Input 'app-id' has been deprecated with
message: Use 'client-id' instead.` on both jobs.

Adopts an additive-secret strategy: a new RELEASE_PLZ_APP_CLIENT_ID
secret holds the paigasusbot App's Client ID (Iv23li... prefix);
RELEASE_PLZ_APP_ID and RELEASE_PLZ_APP_PRIVATE_KEY stay in place for
trivial rollback (revert this commit and the old wiring is intact). The
old RELEASE_PLZ_APP_ID secret can be deleted as a follow-up once the
first post-merge release-plz run on the new path succeeds.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit lands. `ci` is an allowed conventional-commit type per the convco config.

- [ ] **Step 2.6: Confirm the three-commit branch shape**

Run:
```bash
git log --oneline -3
```
Expected:
```
<sha> ci(release-plz): SMA-350 migrate app-token action from app-id to client-id
<sha> chore(release): SMA-350 bump paigasus-helikon-macros to 0.1.0
<sha> docs(spec): SMA-350 add design for release-plz unblock and client-id migration
```

---

## Task 3: Operator prerequisite — create the GitHub secret

**Files:** none. This task is performed by the human operator in the GitHub web UI before the PR merges.

**Why:** The release-plz workflow only runs on `push: branches: [main]`. The first execution of the new workflow file is the post-merge run on `main`. If `RELEASE_PLZ_APP_CLIENT_ID` does not exist at that moment, the `actions/create-github-app-token` step fails and the release pipeline is broken on `main` until the secret is added. Creating it pre-merge is the only way to avoid that window.

- [ ] **Step 3.1: Locate the paigasusbot App's Client ID**

Path: GitHub → top-right avatar → Settings → Developer settings (left sidebar) → GitHub Apps → **paigasusbot** → "About" section → "Client ID" field.

The value is a string with the form `Iv23li…` — distinct from the numeric App ID `3742291` that lives in the existing `RELEASE_PLZ_APP_ID` secret.

- [ ] **Step 3.2: Add the new repository secret**

Path: GitHub → `SMK1085/paigasus-helikon` repo → Settings → Secrets and variables → Actions → "New repository secret".

- **Name:** `RELEASE_PLZ_APP_CLIENT_ID` (exact spelling, case-sensitive)
- **Secret value:** the `Iv23li…` Client ID from Step 3.1, with no surrounding whitespace.

Do **not** modify `RELEASE_PLZ_APP_ID` or `RELEASE_PLZ_APP_PRIVATE_KEY` at this stage.

- [ ] **Step 3.3: Confirm the secret exists**

Path: same secrets page, list view. `RELEASE_PLZ_APP_CLIENT_ID` should appear in the list with an "Updated now" timestamp. The value is masked (this is expected — repo secrets are write-only after creation).

Optional verification via `gh`:
```bash
gh secret list --repo SMK1085/paigasus-helikon | grep RELEASE_PLZ_APP_CLIENT_ID
```
Expected: one line showing `RELEASE_PLZ_APP_CLIENT_ID  Updated <recent timestamp>`.

---

## Task 4: Push the branch and open the PR

**Files:** none modified. This task pushes the existing three commits and opens a PR.

- [ ] **Step 4.1: Push the branch with upstream tracking**

Run:
```bash
git push -u origin feature/sma-350-unblock-release-plz-for-paigasus-helikon-macros-migrate-app
```
Expected: `Branch 'feature/sma-350-…' set up to track 'origin/feature/sma-350-…'.`

- [ ] **Step 4.2: Open the pull request**

The PR title must satisfy `.github/workflows/pr-title.yml`'s regex `^([A-Z]{2,4}-\d+ )?[^A-Z].+$` — the first character after `SMA-### ` must be lowercase. Lead with the verb `unblock`.

Run:
```bash
gh pr create \
  --base main \
  --head feature/sma-350-unblock-release-plz-for-paigasus-helikon-macros-migrate-app \
  --title "chore(release): SMA-350 unblock release-plz for macros and migrate to client-id" \
  --body "$(cat <<'EOF'
## Summary

Closes SMA-350. Two unrelated bug-fixes bundled in one PR; both surfaced by the post-SMA-315 release-plz run.

- **`paigasus-helikon-macros` 0.0.0 → 0.1.0** in both `crates/paigasus-helikon-macros/Cargo.toml` and the workspace `[workspace.dependencies]` pin. Mirrors SMA-347's escape of the release-plz 0.0.0 trap; SMA-315 just shipped this crate's first real public API.
- **`actions/create-github-app-token@v3`** — both `app-id` references in `.github/workflows/release-plz.yml` migrate to `client-id` (the previous value `RELEASE_PLZ_APP_ID` is replaced by a new additive secret `RELEASE_PLZ_APP_CLIENT_ID`). Silences the deprecation annotation; v4 is expected to remove `app-id`.

Design: `docs/superpowers/specs/2026-05-22-sma-350-release-plz-unblock-design.md`.

## Operator prerequisite

⚠️ **Do not merge until** the `RELEASE_PLZ_APP_CLIENT_ID` repo secret has been created with the paigasusbot App's Client ID (the `Iv23li…` string, distinct from the numeric App ID `3742291`). The first execution of the new workflow file is the post-merge run on `main`; if the secret is missing the release pipeline breaks on `main` until it is added.

`RELEASE_PLZ_APP_ID` and `RELEASE_PLZ_APP_PRIVATE_KEY` are intentionally left in place — additive-secret strategy means a revert of this PR restores the previous wiring without further action.

## Test plan

- [ ] Local: `cargo build --workspace --all-features` clean
- [ ] Local: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`, `cargo test --workspace --all-features` all green
- [ ] CI: all required checks green (`fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`)
- [ ] Post-merge: release-plz workflow on `main` shows no `app-id … deprecated` annotation
- [ ] Post-merge: `release-plz-pr` opens a `chore: release` PR including at minimum a `paigasus-helikon-macros` bump (and very likely `paigasus-helikon-core` + `paigasus-helikon` as well, picking up the SMA-315 changes that the previous run dropped)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: `gh` prints the new PR URL. Open it in a browser to sanity-check the rendered description.

- [ ] **Step 4.3: Confirm CI starts**

Run:
```bash
gh pr checks --watch
```
This streams check status until they all complete. Expected (in any order): `fmt`, `clippy`, `test (ubuntu-latest, stable)` (plus the macOS / Windows / 1.75 signal variants), `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny` all reach `pass`. If `pr-title` fails, the title's first character after `SMA-350 ` was uppercased — fix via `gh pr edit --title "<new-title-starting-with-lowercase>"`.

If any required check fails for an unrelated reason (flaky test, transient network), follow up in the normal way — the plan does not prescribe a fix because the failure mode isn't predictable.

- [ ] **Step 4.4: Confirm the PR title passes the lowercase rule**

The squashed-merge commit on `main` will be the PR title verbatim. The PR title in Step 4.2 starts with `chore(release): SMA-350 unblock …` — the `u` after `SMA-350 ` is lowercase. The `pr-title` check enforces this; if it passes, you're good.

---

## Task 5: Merge and observe post-merge verification

**Files:** none. This task lives on `main` after the PR merges.

**Why:** The two fixes are only observable on the `push: branches: [main]` release-plz workflow run. Local CI proves the manifest change compiles; only the post-merge run proves the YAML change works and the macros trap is escaped.

- [ ] **Step 5.1: Confirm Task 3 is complete**

Re-confirm via `gh secret list --repo SMK1085/paigasus-helikon | grep RELEASE_PLZ_APP_CLIENT_ID` (or the GitHub UI) that the secret still exists. Do not merge if it does not.

- [ ] **Step 5.2: Squash-merge the PR**

Once all required checks are green and the secret is verified:
```bash
gh pr merge --squash --delete-branch
```
The squash-merge collapses the three feature-branch commits into one `chore(release): SMA-350 unblock release-plz for macros and migrate to client-id` commit on `main`.

- [ ] **Step 5.3: Wait for the release-plz workflow run to start**

The workflow triggers on the post-merge `push` to `main`. Find the run:
```bash
gh run list --workflow=release-plz.yml --limit=1
```
or visit https://github.com/SMK1085/paigasus-helikon/actions/workflows/release-plz.yml.

- [ ] **Step 5.4: Verify no deprecation annotation**

Open the run in the Actions UI. In each of the two jobs (`release-plz-release`, `release-plz-pr`), expand the `Create GitHub App token` step.

Expected: **no** `Input 'app-id' has been deprecated …` warning. The step should complete in a couple of seconds with no annotations.

If the deprecation warning still appears, the workflow edits in Task 2 did not land — re-check the squash-merged commit's diff.

- [ ] **Step 5.5: Verify token minting succeeded**

In the same step's log, expected:
```
Created GitHub App installation token.
```
(Exact wording from `actions/create-github-app-token@v3`.)

If the step fails with a credential error (401, "invalid client id", etc.), the secret value in Task 3 is wrong — go fix the secret in the GitHub UI and re-run the workflow from the Actions tab.

- [ ] **Step 5.6: Verify the release PR is non-empty**

In the `release-plz-pr` job, expand the `release-plz/action` step. The previous (pre-fix) run logged `release_pr_output: {"prs":[]}` — this run should log a non-empty `prs` array. The minimum acceptable outcome: a new `chore: release` PR appears in https://github.com/SMK1085/paigasus-helikon/pulls including a `paigasus-helikon-macros` version bump in its changelog/diff.

If `prs` is still empty, the macros trap was the only known blocker and yet something else is preventing the release proposal — that is the "out of scope" follow-up the Linear ticket flagged. File a follow-up issue but consider SMA-350 itself done (the trap and the deprecation are both resolved as the AC requires).

- [ ] **Step 5.7: Update Linear (auto)**

The PR merge triggers Linear's GitHub integration to close SMA-350 automatically. No manual status move is required. Confirm by visiting the Linear issue — `status` should read `Done` / `completedAt` should be set.

- [ ] **Step 5.8: Optional follow-up — schedule deletion of the old secret**

Once Step 5.5 has succeeded on at least one post-merge run, `RELEASE_PLZ_APP_ID` is no longer referenced anywhere. It can be deleted from the repo secrets page as a janitorial follow-up. This is **not** part of SMA-350's acceptance criteria — leave it for a separate ticket or a quiet-period cleanup pass.

---

## Acceptance criteria checklist

(Mirrors the Linear ticket; check these off after Task 5.)

- [ ] `paigasus-helikon-macros` is at `0.1.0` in both `crates/paigasus-helikon-macros/Cargo.toml` and `Cargo.toml`'s `[workspace.dependencies]` table.
- [ ] After the PR merges, release-plz opens a `chore: release` PR that includes at minimum a `paigasus-helikon-macros` version bump.
- [ ] The post-merge release-plz workflow run shows no `app-id … deprecated` annotation.
- [ ] No regression: the local commit-msg hook, branch protection, and the `pr-title` gate all stay green on this PR and on the auto-generated release PR that follows.
