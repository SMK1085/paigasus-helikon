# SMA-351 — Combine release-plz jobs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse `release-plz.yml`'s two jobs into one job that mints a single GitHub App installation token and revokes it exactly once at job end, eliminating the revoke/mint race that produced an HTTP 401 on the SMA-349 merge run. Also apply the SHA-pin + `persist-credentials: false` hygiene that landed for `ci.yml` in SMA-349.

**Architecture:** Single job, two sequential `release-plz/action` steps sharing one `actions/create-github-app-token` mint. Step ordering (`release` then `release-pr`) preserves the dependency the current `needs:` enforces. `CARGO_REGISTRY_TOKEN` is step-scoped to `release` only. All four `uses:` lines SHA-pinned with above-the-fold `# <action> vX.Y.Z` comments.

**Tech Stack:** GitHub Actions YAML, release-plz, paigasusbot GitHub App (Client ID auth, established in SMA-350).

**Spec:** `docs/superpowers/specs/2026-05-25-sma-351-combine-release-plz-jobs-design.md`

**Branch:** `feature/sma-351-combine-release-plz-jobs-to-eliminate-app-token-revokemint` (already created; spec doc commit `b85455b` is already on it)

---

## Task 1: Rewrite `.github/workflows/release-plz.yml` to single-job structure

**Files:**
- Modify: `.github/workflows/release-plz.yml` (full-file replacement — two jobs → one)

**Why no TDD steps here:** A workflow YAML change has no unit-test surface. The verification path is (a) local YAML syntax check, (b) all SHAs resolve via the GitHub API, (c) post-push `gh workflow view` confirms GitHub parses the file. The acceptance criterion ("a merge to `main` … triggers exactly one release-plz workflow run that completes green") is post-merge by construction because the workflow is `push: [main]`-only and `workflow_dispatch` is an explicit non-goal.

- [ ] **Step 1: Replace `.github/workflows/release-plz.yml` with the target content**

Write the file with exactly this content (every byte locked by the spec §4.2):

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
  release-plz:
    name: release-plz
    runs-on: ubuntu-latest
    steps:
      # actions/checkout v6.0.2
      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd
        with:
          fetch-depth: 0          # release-plz needs full history
          persist-credentials: false
      # dtolnay/rust-toolchain master (no tagged releases)
      - uses: dtolnay/rust-toolchain@3c5f7ea28cd621ae0bf5283f0e981fb97b8a7af9
        with:
          toolchain: stable
      # actions/create-github-app-token v3.2.0
      - uses: actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1
        id: app-token
        with:
          client-id: ${{ secrets.RELEASE_PLZ_APP_CLIENT_ID }}
          private-key: ${{ secrets.RELEASE_PLZ_APP_PRIVATE_KEY }}
      # release-plz/action v0.5.129
      - uses: release-plz/action@064f4d1e36c843611ddf013be726beaa4ad804db
        with:
          command: release
        env:
          GITHUB_TOKEN: ${{ steps.app-token.outputs.token }}
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
      # release-plz/action v0.5.129
      - uses: release-plz/action@064f4d1e36c843611ddf013be726beaa4ad804db
        with:
          command: release-pr
        env:
          GITHUB_TOKEN: ${{ steps.app-token.outputs.token }}
```

Confirm against the spec § 4.2: the file replaces both the `release-plz-release` and `release-plz-pr` jobs with a single `release-plz` job; no `needs:` remains in the file; the second `release-plz/action` step's `env:` block has only `GITHUB_TOKEN` (no `CARGO_REGISTRY_TOKEN`).

- [ ] **Step 2: Diff against `main` to verify the change is exactly what the spec describes**

Run:
```bash
git diff main -- .github/workflows/release-plz.yml
```

Expected: the diff shows
- `release-plz-release` and `release-plz-pr` jobs deleted.
- A single `release-plz` job added with two `release-plz/action` steps.
- Every `uses:` line on the new job has a 40-hex-char SHA, never a moving ref (`@v6`, `@stable`, `@v3`, `@v0.5`).
- The `actions/checkout` step has both `fetch-depth: 0` **and** `persist-credentials: false`.
- The `release-plz/action` step with `command: release-pr` has **no** `CARGO_REGISTRY_TOKEN` in its `env:` block.

If any of these don't match, edit and re-run the diff.

- [ ] **Step 3: Verify YAML syntax with python**

Run:
```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release-plz.yml')); print('ok')"
```

Expected: `ok`

If anything else: fix indentation / quoting and re-run until `ok`.

- [ ] **Step 4: Verify every SHA in the file resolves to a real commit**

Run:
```bash
for entry in \
  "actions/checkout de0fac2e4500dabe0009e67214ff5f5447ce83dd" \
  "dtolnay/rust-toolchain 3c5f7ea28cd621ae0bf5283f0e981fb97b8a7af9" \
  "actions/create-github-app-token bcd2ba49218906704ab6c1aa796996da409d3eb1" \
  "release-plz/action 064f4d1e36c843611ddf013be726beaa4ad804db" ; do
  set -- $entry
  echo -n "$1@$2: "
  gh api "repos/$1/commits/$2" --jq '.sha' || echo "MISSING"
done
```

Expected: each line prints the same SHA back, e.g. `actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd: de0fac2e4500dabe0009e67214ff5f5447ce83dd`. Any `MISSING` is a fat-fingered SHA — fix and re-run.

- [ ] **Step 5: Verify the only moving refs left in `.github/workflows/release-plz.yml` are zero**

Run:
```bash
grep -nE 'uses: [^@]+@(v[0-9]|stable|main|master)' .github/workflows/release-plz.yml
```

Expected: no output (exit code 1). If any line matches, the SHA-pin is incomplete — replace the moving ref with the corresponding 40-hex SHA from Step 4.

- [ ] **Step 6: Commit the workflow change**

Run:
```bash
git add .github/workflows/release-plz.yml
git status
```

Expected: only `.github/workflows/release-plz.yml` in the staged list. Then:

```bash
git commit -m "$(cat <<'EOF'
ci(workflows): SMA-351 combine release-plz jobs to share one App token mint

Collapse release-plz-release and release-plz-pr into a single
release-plz job. Both release-plz/action invocations now share one
actions/create-github-app-token mint, which is revoked exactly once
at job end. Removes the revoke -> re-mint race that produced HTTP 401
on the SMA-349 merge run (run 26327278906).

Also applies the SHA-pin + persist-credentials hygiene from ci.yml:
- actions/checkout v6.0.2 SHA-pinned, persist-credentials: false
- dtolnay/rust-toolchain master SHA-pinned
- actions/create-github-app-token v3.2.0 SHA-pinned
- release-plz/action v0.5.129 SHA-pinned (annotated tag dereferenced)

No new release-plz job name (release-plz) is referenced by
.github/rulesets/main-protection-checks.json, so no ruleset edit is
needed.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: one new commit on `feature/sma-351-...`. Confirm with `git log --oneline -3`.

- [ ] **Step 7: Verify convco accepts the commit subject**

Run:
```bash
convco check b85455b..HEAD
```

(Where `b85455b` is the spec-doc commit already on the branch — adjust if your local HEAD has different ancestry.)

Expected: exit 0, no output (or `OK` per convco's format). If convco rejects, the most likely cause is a non-allowlisted scope — re-check that the commit subject starts with `ci(workflows):` not e.g. `ci(release-plz):` (the latter is NOT in `.versionrc`'s scope regex).

---

## Task 2: Push branch and verify GitHub parses the workflow

**Files:** (none modified in this task)

- [ ] **Step 1: Push the feature branch with upstream tracking**

Run:
```bash
git push -u origin feature/sma-351-combine-release-plz-jobs-to-eliminate-app-token-revokemint
```

Expected: branch created on origin, push succeeds.

- [ ] **Step 2: Confirm GitHub recognises the workflow file**

Run:
```bash
gh workflow view release-plz --ref feature/sma-351-combine-release-plz-jobs-to-eliminate-app-token-revokemint
```

Expected: GitHub renders the workflow's trigger/jobs section without parse errors and shows exactly one job (`release-plz`). If GitHub reports a parse error, fix the YAML locally, amend the workflow commit (a brand-new local-only commit is fine to amend; do not amend after the PR is opened), and re-push with `git push --force-with-lease`.

Note: the workflow itself won't *run* on this push because its trigger is `push: [main]`. That's expected.

---

## Task 3: Open the PR and ride the green build

**Files:** (none modified in this task)

- [ ] **Step 1: Open the PR**

Run:
```bash
gh pr create \
  --base main \
  --head feature/sma-351-combine-release-plz-jobs-to-eliminate-app-token-revokemint \
  --title "SMA-351 combine release-plz jobs to share one App token mint" \
  --body "$(cat <<'EOF'
## Summary
- Collapses `release-plz-release` and `release-plz-pr` into a single `release-plz` job. Both `release-plz/action` invocations now share one `actions/create-github-app-token` mint, revoked exactly once at job end — removes the revoke→re-mint race that produced HTTP 401 on the SMA-349 merge run.
- Applies the SHA-pin + `persist-credentials: false` hygiene from `ci.yml` to every `uses:` line in `release-plz.yml`.

Linear: [SMA-351](https://linear.app/smaschek/issue/SMA-351/combine-release-plz-jobs-to-eliminate-app-token-revokemint-race)

Spec: `docs/superpowers/specs/2026-05-25-sma-351-combine-release-plz-jobs-design.md`

## Test plan
- [x] Local: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release-plz.yml'))"` returns ok.
- [x] Local: every `uses:` SHA resolves via `gh api repos/<owner>/<repo>/commits/<sha>`.
- [x] Local: `grep -nE 'uses: [^@]+@(v[0-9]|stable|main|master)' .github/workflows/release-plz.yml` returns no matches.
- [x] Local: `convco check` accepts the commit subject (`ci(workflows): SMA-351 …`).
- [ ] CI: `commits`, `pr-title`, `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `audit`, `deny`, `book-build` all green.
- [ ] CI: `gh workflow view release-plz --ref <branch>` shows a single `release-plz` job.
- [ ] Post-merge: first push-to-main triggers exactly one `release-plz` workflow run, completes green, logs show one `Token revoked` line (not two) and no HTTP 401.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: PR URL printed. Capture it for the user.

Subject-format check: the title `SMA-351 combine release-plz jobs to share one App token mint` starts with lowercase `c` after `SMA-351 ` — matches `pr-title.yml`'s `^([A-Z]{2,4}-\d+ )?[^A-Z].+$` regex.

- [ ] **Step 2: Wait for required checks and confirm green**

Run:
```bash
gh pr checks --watch
```

Expected: every required context goes green: `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `book-build`, `commits`, `pr-title`, `audit`, `deny`. (Other matrix variants — macos/windows/1.75 — are signals only per CLAUDE.md, but should also pass since this change doesn't touch Rust source.)

If `commits` fails: the commit subject scope isn't in `.versionrc`. Re-check Task 1 Step 6's subject.
If `pr-title` fails: the PR title's subject starts with an uppercase letter. Edit via `gh pr edit --title "..."`.

- [ ] **Step 3: Report PR URL to the user**

Print the PR URL. The user merges (squash) when satisfied. Post-merge verification is theirs: watch the first push-to-main trigger exactly one `release-plz` run, confirm it goes green, confirm the run's logs show one `Token revoked` line and no HTTP 401. Linear auto-closes SMA-351 on merge.

---

## Self-review checklist (run before declaring the plan ready)

- **Spec coverage:** Every spec §3 decision is implemented in Task 1 Step 1's YAML (single job, shared mint, step-scoped CARGO_REGISTRY_TOKEN, no re-checkout, SHA pins with above-the-fold comments, `persist-credentials: false`, top-level blocks unchanged). Spec §5's pre-merge verification path is encoded in Task 1 Steps 3–5, Task 2 Step 2, and Task 3 Step 2. Spec §5's post-merge verification is documented as the user's responsibility in Task 3 Step 3.
- **Placeholder scan:** No `TBD`, `TODO`, `appropriate`, or unfilled code blocks. Every shell command is runnable as-written.
- **Type consistency:** Branch name in Task 2 Step 1 matches Task 3 Step 1's `--head` matches the spec's branch name matches Linear's `gitBranchName`. All four SHAs match the spec §4.1 table. Commit subject (`ci(workflows): SMA-351 …`) and PR title (`SMA-351 combine release-plz jobs …`) are aligned and both pass the relevant gates.
- **Out-of-scope:** Plan does not add caching, `workflow_dispatch`, or any other non-goal from spec §1.
