# SMA-745 release-plz ref 403 fix and gap report — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the release-plz App token the `workflows` permission, move the release secrets into a `main`-only `release` environment, and add a gap report that makes a release-plz run red when a published version has no tag or no GitHub release.

**Architecture:** One bash script (`scripts/check-release-gaps.sh`) does the check. A hermetic self-test (`scripts/check-release-gaps-selftest.sh`) drives it with fake `curl`/`gh` on `PATH` and fixture files, and also statically guards `release-plz.yml`. The self-test runs in the required `fmt` job. `release-plz.yml` gets the permission, the environment, a metadata pre-step and the report step. `apply-repo-config.sh` creates the environment.

**Tech Stack:** bash, `jq`, `curl`, `gh`, GitHub Actions, GitHub REST API, crates.io API.

**Spec:** `docs/superpowers/specs/2026-10-31-sma-745-release-plz-workflow-ref-403-design.md` (read it before any task).

**Worktree root (all paths are relative to it):** `/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/5529e711-5b81-4908-bc65-d19712b72564/scratchpad/wt-sma-745`. Use absolute paths with this prefix for Write/Edit. Never run `git checkout`, `git switch`, `git reset` or any command that moves HEAD or changes branches.

## Global Constraints

- Commit type and scope: `chore(workflows): SMA-745 <lowercase subject>` for code/workflow commits, `docs(release): SMA-745 …` or `docs(ci): SMA-745 …` for doc-only commits. Never `feat`/`fix`. Scope must match `.versionrc` `scopeRegex`.
- End every commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Commits are signed through 1Password. If a commit fails with "failed to fill whole buffer", stop and report; do not bypass signing.
- Stage files with explicit paths. Never `git add -A` or `git add .` (`.env` is not ignored).
- Work synchronously in the foreground. Do not end your turn while a command runs in the background.
- No new GitHub Action. Existing SHA pins stay unchanged.
- crates.io User-Agent: `paigasus-helikon-release-gaps (https://github.com/SMK1085/paigasus-helikon)`.
- Tag format: `<package>-v<version>`. Environment name: `release`. Secrets: `RELEASE_PLZ_APP_CLIENT_ID`, `RELEASE_PLZ_APP_PRIVATE_KEY`, `CARGO_REGISTRY_TOKEN`.
- Exit codes of `check-release-gaps.sh`: 0 no gaps, 1 at least one gap, 2 "could not check" and no gap. With `RELEASE_STEP_OUTCOME=success`, a result of 2 prints `::warning::` and exits 0.
- Write all prose (comments, docs, runbook) in ASD-STE100 Simplified Technical English: short sentences, active voice, no idiom.

## Review Focus

1. **Real `gh api --include` output on an HTTP error.** `gh` exits non-zero on 404 but still prints the status line and body to stdout. Expected: a 404 parses as "missing", not "could not check". Pinned by Task 5 Step 2 (live run must show evals 0.1.17 as a gap, exit 1).
2. **HTTP status line forms.** `HTTP/2.0 200 OK`, `HTTP/2 200`, `HTTP/1.1 404 Not Found` must all parse. Pinned by self-test cases in Task 1 (fakes use mixed forms).
3. **`timeout` missing on macOS.** Local runs on the dev host (macOS) have no `timeout` by default. Expected: the script uses `timeout 30` only when `command -v timeout` finds it, and still works without it. Pinned by Task 5 Step 2 (live run on macOS).
4. **`publish` field forms in `cargo metadata`.** `null` = publishable, `[]` = not publishable, `["crates-io"]` = publishable. Pinned by the fixture metadata in Task 1 (contains all three forms).
5. **Yanked versions.** A yanked version is still in crates.io `versions[].num` and counts as published (it needs a tag). Pinned by a Task 1 case.

---

### Task 1: Gap report script and its hermetic self-test

**Files:**
- Create: `scripts/check-release-gaps.sh`
- Create: `scripts/check-release-gaps-selftest.sh`

**Interfaces:**
- Produces: `bash scripts/check-release-gaps.sh` (no arguments). Environment inputs:
  - `GH_TOKEN` — used by `gh`.
  - `GITHUB_REPOSITORY` — default `SMK1085/paigasus-helikon`.
  - `RELEASE_GAPS_METADATA` — path to a `cargo metadata --no-deps --format-version 1` JSON. If unset, the script runs that command itself from the repo root.
  - `RELEASE_GAPS_CONFIG` — path to `release-plz.toml`. Default `<repo root>/release-plz.toml`.
  - `RELEASE_GAPS_SLEEP` — seconds between crates.io requests. Default `1`.
  - `RELEASE_STEP_OUTCOME` — `success`, `failure`, `cancelled`, or unset (unset behaves as `failure`).
  - `GITHUB_SHA` — default `git rev-parse HEAD`; used only in recovery text.
  - `GITHUB_STEP_SUMMARY` — if set, the report is appended there; the report always also goes to stdout.
- External call forms (the self-test fakes rely on these exact forms):
  - `curl -sS --max-time 20 -A "<UA>" -o <body-file> -w '%{http_code}' "https://crates.io/api/v1/crates/<name>"`
  - `gh api --include "repos/<repo>/git/ref/tags/<tag>"` and `gh api --include "repos/<repo>/releases/tags/<tag>"`, each wrapped in `timeout 30` when `timeout` exists. Status is read from the first line of output (`HTTP/<ver> <code>…`). The `gh` exit code is ignored. No status line → "could not check".
- Produces for Task 2: the self-test file, which Task 2 extends with static guards.

- [ ] **Step 1: Write the self-test with all behavior cases**

  The self-test puts fake `curl` and `gh` executables in a temp `bin/` dir at the front of `PATH`. The fakes answer from fixture files in a per-case dir (`FAKE_DIR`), keyed by the crate name or the API path. A missing fixture for crates.io means "no response" (curl exits 28, prints `000`). Each case writes its own fixture metadata JSON and sets `RELEASE_GAPS_SLEEP=0`, `RELEASE_GAPS_CONFIG=<fixture toml without tag keys>`, `GITHUB_REPOSITORY=o/r`, `GITHUB_SHA=deadbeef`.

  Base fixture metadata: three packages — `alpha` 1.0.0 (`publish: null`), `beta` 2.0.0 (`publish: ["crates-io"]`), `internal` 0.0.0 (`publish: []`). Base "all good": crates.io lists the version for alpha and beta; tag 200 with `HTTP/2.0 200 OK`; release 200 with `{"draft":false}` and `HTTP/2 200`; nothing is requested for `internal` (the fake fails the case if it is).

  Helper: `expect <exit> <name> [<grep-pattern>...]` — runs the script, compares the exit code, and checks that each pattern appears in the output.

  Cases (exit code, required output patterns):
  - all good → 0, `no gaps`
  - alpha tag 404 (`HTTP/1.1 404 Not Found`) → 1, `alpha`, `1.0.0`, `tag`
  - alpha release 404 → 1, `alpha`, `release`
  - alpha release 200 with `"draft":true` → 1, `alpha`, `draft`
  - alpha version not in the crates.io list → 0 (and no tag/release call for alpha)
  - alpha version yanked (present in list with `"yanked":true`) and tag 404 → 1, `alpha`
  - crates.io 404 for alpha → 2, `could not check`, `alpha`
  - crates.io 500 → 2; crates.io 429 → 2; crates.io no response → 2
  - alpha tag 500 → 2; alpha release 401 → 2
  - alpha tag 404 and beta crates.io 500 → 1, `alpha`, `could not check`, `beta`
  - only `alpha-v1.0.0X`-style longer tag exists: the fake answers 404 for the exact path `git/ref/tags/alpha-v1.0.0` → 1 (proves exact-match call)
  - `RELEASE_GAPS_METADATA` points to a missing file → 2
  - metadata with only `internal` → 2, `no publishable`
  - config containing `git_tag_name = "x"` → 2, `git_tag_name`
  - `RELEASE_STEP_OUTCOME=success` with crates.io 500 → 0, `::warning::`
  - `RELEASE_STEP_OUTCOME=success` with alpha tag 404 → 1
  - `GITHUB_STEP_SUMMARY` set, alpha tag 404 → the summary file contains `alpha` and `gh api -X POST`; stdout contains exactly one `::error::` line

  End with `failures` count; exit 1 if any case failed. Follow the style of `scripts/check-cargo-profile-env-sync-selftest.sh` (header comment, `set -euo pipefail`, `mktemp -d` + `trap`, `ok —`/`FAIL —` lines).

- [ ] **Step 2: Run the self-test and verify it fails**

  Run: `bash scripts/check-release-gaps-selftest.sh`
  Expected: non-zero exit; cases fail because `scripts/check-release-gaps.sh` does not exist.

- [ ] **Step 3: Implement `scripts/check-release-gaps.sh`**

  Structure: `set -euo pipefail`; header comment that states purpose, SMA-745, the three results and the env inputs. Order of work: tag-config guard → read metadata (jq: `.packages[] | select(.publish != []) | "\(.name) \(.version)"`) → empty list = could not check → per package: crates.io (sleep between requests), then tag, then release only if published → collect gaps and could-not-check items → print report → exit code per Global Constraints.

  crates.io rule: status 200 and body parses with jq → version in `.versions[].num` = published, else not published. Any other status (including 404) or jq failure → could not check.

  Tag rule: 200 → present, 404 → missing, other → could not check. Release rule: 200 and `.draft == false` → present; 200 and draft, or 404 → missing; other → could not check. The body starts after the first empty line of `gh api --include` output (strip `\r`).

  Recovery text per gap (in the report body, not in the annotation):
  - missing tag: look for the tag object in the failed run's log first; else `gh api -X POST repos/<repo>/git/tags -f tag=<tag> -f message=<tag> -f object=<GITHUB_SHA> -f type=commit`, then `gh api -X POST repos/<repo>/git/refs -f ref=refs/tags/<tag> -f sha=<tag-object-sha>`.
  - missing release: `gh release create <tag> --verify-tag --latest=false --title <tag> --notes-file <CHANGELOG section of crates/<dir>/CHANGELOG.md>` (dir from the package's `manifest_path`).
  - one line: the token needs the `workflow` scope (`gh auth refresh -s workflow`).

  Annotation: exactly one `::error::` line for all gaps (`::error::release gaps: <n> published version(s) without tag or release — see the job summary`). Could-not-check on a non-success outcome: one `::error::`-free line per item in the report, plus exit 2.

- [ ] **Step 4: Run the self-test and verify it passes**

  Run: `bash scripts/check-release-gaps-selftest.sh`
  Expected: every line starts with `ok`, exit 0.
  Also run: `shellcheck scripts/check-release-gaps.sh scripts/check-release-gaps-selftest.sh` if `shellcheck` is installed; fix every warning.

- [ ] **Step 5: Commit**

```bash
git add scripts/check-release-gaps.sh scripts/check-release-gaps-selftest.sh
git commit -m "chore(workflows): SMA-745 add release gap report and its self-test"
```

---

### Task 2: Wire release-plz.yml and the fmt gate

**Files:**
- Modify: `.github/workflows/release-plz.yml`
- Modify: `.github/workflows/ci.yml` (the `fmt` job, after the "Self-test the cargo-visible env parser" step)
- Modify: `scripts/check-release-gaps-selftest.sh` (add static guards)

**Interfaces:**
- Consumes: `scripts/check-release-gaps.sh` env inputs from Task 1.
- Produces: the workflow shape that Task 4 documents.

- [ ] **Step 1: Add static guards to the self-test (failing)**

  New section at the end of the self-test, reading the real `<repo root>/.github/workflows/release-plz.yml` with `grep`. Each guard is one `ok —`/`FAIL —` line:
  - contains `permission-workflows: write`
  - contains `environment: release`
  - contains `id: release`
  - contains `scripts/check-release-gaps.sh`
  - contains `RELEASE_GAPS_METADATA`
  - contains `GH_TOKEN: ${{ github.token }}`

- [ ] **Step 2: Run the self-test and verify the guards fail**

  Run: `bash scripts/check-release-gaps-selftest.sh`
  Expected: the behavior cases print `ok`; the six guards print `FAIL`; exit 1.

- [ ] **Step 3: Edit `release-plz.yml`**

  - Job `release-plz`: add `environment: release` with a comment that the release secrets live in this `main`-only environment (SMA-745).
  - After the `dtolnay/rust-toolchain` step, add a step `name: record workspace versions for the gap report` that runs `cargo metadata --no-deps --format-version 1 > "$RUNNER_TEMP/release-gaps-metadata.json"`.
  - `create-github-app-token` step: add `permission-workflows: write` with a comment: a ref at a commit behind a workflow change needs it (SMA-745).
  - The `command: release` step: add `id: release`.
  - New last step, exactly as in spec section 3, plus `RELEASE_GAPS_METADATA: ${{ runner.temp }}/release-gaps-metadata.json` in its `env:`.

- [ ] **Step 4: Add the self-test to `ci.yml` `fmt`**

  New step after "Self-test the cargo-visible env parser": `name: Self-test the release gap report`, `run: bash scripts/check-release-gaps-selftest.sh`, with a two-line comment: no PR runs `release-plz.yml`, so this self-test and its static guards are the only pre-merge check of the gap report (SMA-745).

- [ ] **Step 5: Verify**

  Run: `bash scripts/check-release-gaps-selftest.sh` → all `ok`, exit 0.
  Run: `bash scripts/check-cargo-profile-env-sync.sh && bash scripts/check-cargo-profile-env-sync-selftest.sh` → exit 0 (the new env keys are not `CARGO_*`/`RUST*`; confirm the guard agrees).
  Run: `python3 -c 'import sys,yaml; [yaml.safe_load(open(f)) for f in sys.argv[1:]]' .github/workflows/release-plz.yml .github/workflows/ci.yml` → no error (if PyYAML is missing, use `ruby -ryaml -e 'ARGV.each{|f| YAML.load_file(f)}'`).

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release-plz.yml .github/workflows/ci.yml scripts/check-release-gaps-selftest.sh
git commit -m "chore(workflows): SMA-745 grant workflows permission, add release environment and gap report step"
```

---

### Task 3: Create the `release` environment in apply-repo-config.sh

**Files:**
- Modify: `scripts/apply-repo-config.sh` (header "What this script does" list, and a new section `# ---------- 5. Apply the release environment ----------` before the final `echo`)

**Interfaces:**
- Produces: an idempotent environment `release` with `deployment_branch_policy: {protected_branches: false, custom_branch_policies: true}` and exactly one branch policy `{name: "main", type: "branch"}`.

- [ ] **Step 1: Implement the section**

  `gh api -X PUT "repos/$REPO/environments/release" --input <json>` with the policy above (no reviewers, no wait timer). Then list `repos/$REPO/environments/release/deployment-branch-policies`; add `main` with `POST … -f name=main -f type=branch` only if no policy with name `main` and type `branch` exists. Delete any other branch policy so re-running converges. Update the header list (item 5) and the final `echo` line to mention the environment. Add a comment that secrets are set by hand (values cannot be read back) and point to `docs/runbooks/release-ascend.md`.

- [ ] **Step 2: Verify syntax only**

  Run: `sh -n scripts/apply-repo-config.sh` → exit 0. `shellcheck -s sh scripts/apply-repo-config.sh` if installed → no new warnings.
  Do **not** run the script. It changes live repository settings; Sven runs it (spec section 6, step 2).

- [ ] **Step 3: Commit**

```bash
git add scripts/apply-repo-config.sh
git commit -m "chore(repo): SMA-745 create the main-only release environment"
```

---

### Task 4: Documentation

**Files:**
- Modify: `docs/runbooks/release-ascend.md` (new section at the end, before "Implementation status snapshot" if that section is last-of-history; keep the scope blockquote at the top accurate)
- Modify: `CLAUDE.md` (the "reproduce every CI gate" block, after the env-sync self-test line)
- Modify: `CONTRIBUTING.md` (the gate list near line 235, and "Repo configuration" near line 306)
- Modify: `docs/runbooks/ci-architecture.md` ("Workflow permissions" section, and the `fmt` text near line 207)

- [ ] **Step 1: Runbook section** "A red release-plz run with 403 on `git/refs` (SMA-745)" with the content list in spec section 5. Include the cause test table, the three report results, the recovery commands from spec section 7 in generic form, and the rule "a red release-plz run can still have published crates".

- [ ] **Step 2: Gate lists.** Add `bash scripts/check-release-gaps-selftest.sh            # release gap report + release-plz.yml guards (fmt runs it)` to `CLAUDE.md` and `CONTRIBUTING.md`. In `ci-architecture.md`, describe the new `fmt` step next to the env-sync self-test, and state in "Workflow permissions" that the release-plz App token requests `contents`, `pull-requests` and `workflows` write, and why.

- [ ] **Step 3: Repo configuration.** In `CONTRIBUTING.md` "Repo configuration", add the `release` environment: what `apply-repo-config.sh` creates, that the three secrets are environment secrets set by hand, and that repository-level copies must not exist after the rollout.

- [ ] **Step 4: Verify**

  Run: `npx markdownlint-cli2` → 0 errors. (Run `npm ci` first if `node_modules` is missing.)

- [ ] **Step 5: Commit**

```bash
git add docs/runbooks/release-ascend.md CLAUDE.md CONTRIBUTING.md docs/runbooks/ci-architecture.md
git commit -m "docs(release): SMA-745 document the ref 403, the gap report and the release environment"
```

---

### Task 5: Live verification and the evals 0.1.17 recovery handoff

**Files:**
- Create (scratch, not committed): `<scratchpad>/evals-0.1.17-notes.md` — the 0.1.17 section of `crates/paigasus-helikon-evals/CHANGELOG.md`.

- [ ] **Step 1: Run every local gate that the change touches**

  `cargo fmt --all -- --check`, `bash scripts/check-cargo-profile-env-sync.sh`, `bash scripts/check-cargo-profile-env-sync-selftest.sh`, `bash scripts/check-release-gaps-selftest.sh`, `bash scripts/check-markdownlint-config.sh`, `npx markdownlint-cli2`. All exit 0.

- [ ] **Step 2: Live run against the real repository (read-only)**

  Run: `GH_TOKEN=$(gh auth token) bash scripts/check-release-gaps.sh; echo "exit=$?"`
  Expected: exit 1, exactly one gap: `paigasus-helikon-evals` 0.1.17, tag missing and release missing. No "could not check" items. If the result differs, stop: it is a bug in Task 1 or an unknown live-API form (Review Focus 1–3).

- [ ] **Step 3: Prepare the recovery for Sven**

  Extract the 0.1.17 section to the scratch file. Give Sven the exact commands from spec section 7 with the real `--notes-file` path. Do not run them (the classifier blocks ref creation by the agent, and the token lacks `workflow`).

- [ ] **Step 4: After Sven reports the recovery done**, re-run Step 2. Expected: exit 0, `no gaps`.
