# SMA-487 `pull-requests: read` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `pull-requests: read` to `.github/workflows/ci.yml`'s top-level `permissions:` block, with a comment that survives a permission-minimising reader, and correct the one sentence in `CLAUDE.md` that the change falsifies.

**Architecture:** Two text edits in one atomic commit. No Rust code, no tests, no new files. The `CLAUDE.md` sentence is wrong *only* because of the workflow edit, so the two ship together.

**Tech Stack:** GitHub Actions workflow YAML; Markdown.

**Spec:** `docs/superpowers/specs/2026-08-08-sma-487-ci-pull-requests-read-design.md`

## Global Constraints

- **Do not broaden any other permission in any workflow.** The ticket says this explicitly. The only permissions line that changes anywhere is the one added in Task 1 Step 1.
- **Workflow-level, not job-level.** A job-level `permissions:` block replaces rather than merges (see `docs.yml:45-47`). Do not add a `permissions:` block to the `sessions-it` job.
- **Comment leads with the imperative.** "Required … — do not remove" comes first; the "why it looks removable" nuance comes after. Reversing this defeats the ticket's stated purpose.
- **Do not claim this makes `ci.yml` private-repo-safe.** It closes the `pull_request` path only; the `push` path has a separate unauthenticated-fetch problem that is out of scope.
- Commit type/scope must be in `.versionrc`'s allowlist: use `ci(workflows)`.
- Commit message prefix: `ci(workflows): SMA-487 <lowercase subject>`.
- No mdBook or crate README changes — pure-internal CI plumbing, conscious call per CLAUDE.md.

---

### Task 1: Grant the scope and fix the doc sentence

**Files:**
- Modify: `.github/workflows/ci.yml:12-13`
- Modify: `CLAUDE.md:111`
- Test: none — this is workflow YAML and prose. Verification is CI observation, Task 2.

**Interfaces:**
- Consumes: nothing.
- Produces: the `pull-requests: read` scope on `ci.yml`'s `GITHUB_TOKEN`, which Task 2 verifies by reading the job log.

- [ ] **Step 1: Add the grant to `ci.yml`**

The block at `.github/workflows/ci.yml:12-13` currently reads exactly:

```yaml
permissions:
  contents: read
```

Replace it with exactly:

```yaml
permissions:
  contents: read
  # Required by dorny/paths-filter in sessions-it — do not remove. On a
  # pull_request it resolves changed files through `pulls.listFiles` (job log:
  # "Fetching list of changed files for PR#N from GitHub API"). It looks
  # removable because it is: the repo is public, so that call succeeds without
  # the scope and has since SMA-330. That is an accident of repository
  # visibility, not a property of the workflow — and sessions-it is a
  # *required* check, so the cost of being wrong is every PR blocked.
  pull-requests: read
```

Note the log string is `GitHub API` with a capital H — verified against the real
`sessions-it` log for run `31216942714`. Do not "correct" it to `Github`.

- [ ] **Step 2: Verify the YAML still parses**

Run:

```bash
python3 -c "import yaml,sys; d=yaml.safe_load(open('.github/workflows/ci.yml')); print(d['permissions'])"
```

Expected output exactly:

```
{'contents': 'read', 'pull-requests': 'read'}
```

If this raises, the indentation is wrong — the two scope keys must sit at the
same level, with the comment lines between them at that same indentation.

- [ ] **Step 3: Verify no other permissions line changed**

Run:

```bash
git diff -U0 .github/workflows/ci.yml | grep -E '^[+-]' | grep -v '^[+-][+-]'
```

Expected: added lines only (the comment block plus `  pull-requests: read`), and
**zero** removed lines. Any `-` line other than none means something else was
touched — revert and redo Step 1.

- [ ] **Step 4: Confirm `sessions-it` gained no job-level block**

Run:

```bash
sed -n '179,200p' .github/workflows/ci.yml | grep -c 'permissions:'
```

Expected: `0`. The grant belongs at workflow level only.

- [ ] **Step 5: Fix the `CLAUDE.md` sentence**

In `CLAUDE.md:111`, find this exact text (it is the tail of a long paragraph):

```
Concurrency cancels in-flight PR runs but lets `main` pushes complete; both workflows declare `permissions: contents: read`.
```

Replace it with exactly:

```
Concurrency cancels in-flight PR runs but lets `main` pushes complete. `ci.yml` declares `contents: read` plus `pull-requests: read` — the latter for `sessions-it`'s `dorny/paths-filter`, which calls `pulls.listFiles` on PR events (SMA-487); the workflow comment carries the rationale and the reason it must not be minimised away.
```

Do **not** expand this into a per-workflow permissions inventory. That would be a
new drift surface requiring an update on every future permissions edit, in the
file the repo has already run two catch-ups against (SMA-423, SMA-424).

- [ ] **Step 6: Verify the stale claim is gone**

Run:

```bash
grep -c 'both workflows declare' CLAUDE.md
```

Expected: `0`.

Then run:

```bash
grep -c 'pull-requests: read' CLAUDE.md
```

Expected: `1`.

- [ ] **Step 7: Confirm the diff touches exactly two files**

Run:

```bash
git status --short
```

Expected exactly two modified entries: `.github/workflows/ci.yml` and
`CLAUDE.md`. If anything else appears — especially `.env` or `.claude/` — do
**not** proceed; those are untracked-but-not-ignored in this repo and must never
be staged. Stage explicit paths only, never `git add -A`.

- [ ] **Step 8: Commit**

```bash
git add .github/workflows/ci.yml CLAUDE.md
git commit -m "ci(workflows): SMA-487 grant pull-requests read for sessions-it's paths-filter"
git show --stat HEAD
```

Expected: 2 files changed. The commit is signed via the 1Password SSH key; if it
fails with "failed to fill whole buffer", the vault is locked — ask the user to
unlock it and retry rather than bypassing signing.

---

### Task 2: Verify the grant actually took effect

**Blocked until the PR is open** (pipeline Stage 5). This is the spec's *real*
verification and the only check that distinguishes this change from its absence.

**Files:** none — this task reads CI output and writes no code.

**Interfaces:**
- Consumes: the scope added in Task 1.
- Produces: the evidence quoted in the PR body.

- [ ] **Step 1: Locate this PR's `sessions-it` job**

```bash
RUN=$(gh run list --workflow=ci.yml --branch=feature/sma-487-ci-grant-pull-requests-read-to-sessions-its-paths-filter-in \
  --limit 1 --json databaseId --jq '.[0].databaseId')
JOB=$(gh api repos/SMK1085/paigasus-helikon/actions/runs/$RUN/jobs \
  --jq '.jobs[] | select(.name=="sessions-it") | .id')
echo "run=$RUN job=$JOB"
```

- [ ] **Step 2: Read the token-permissions block — the actual verification**

```bash
gh api repos/SMK1085/paigasus-helikon/actions/jobs/$JOB/logs 2>/dev/null \
  | grep -A4 'GITHUB_TOKEN Permissions'
```

Expected: the group now lists **both** `Contents: read` and
`PullRequests: read`.

The captured baseline from before this change (run `31216942714`) is:

```
##[group]GITHUB_TOKEN Permissions
Contents: read
```

— `PullRequests` absent. If it is still absent after this change, the grant did
not take effect and Task 1 is wrong.

- [ ] **Step 3: Regression check — the filter still works**

```bash
gh api repos/SMK1085/paigasus-helikon/actions/jobs/$JOB/logs 2>/dev/null \
  | grep -E 'changed files|listFiles|No session-related'
```

Expected: `Fetching list of changed files for PR#N from GitHub API` and
`Invoking listFiles(...)`. Because `.github/workflows/ci.yml` is in the
`sessions` filter list (`ci.yml:195`), this PR matches its own filter and the
live Postgres/Redis suite runs — the `No session-related changes` branch must
**not** appear.

This step proves the edit did not break the workflow. It proves nothing about
the grant; that is Step 2's job. Do not report Step 3 passing as evidence the
ticket is done.

- [ ] **Step 4: Confirm `sessions-it` concluded green**

```bash
gh run view $RUN --json jobs --jq '.jobs[] | select(.name=="sessions-it") | {name, conclusion}'
```

Expected: `"conclusion": "success"`.

---

## Self-Review

**1. Spec coverage.**

| Spec section | Task |
|---|---|
| §1 The grant + comment text | Task 1, Steps 1-4 |
| §2 Audit (negative result, no code) | No task by design — it produces no change; it is recorded in the spec and goes in the PR body at Stage 5 |
| §3 CLAUDE.md correction | Task 1, Steps 5-6 |
| Verification gate 1 (token permissions) | Task 2, Step 2 |
| Verification gate 2 (regression) | Task 2, Step 3 |
| Out of scope: push-path gap, other permissions, mdBook/README | Global Constraints; no task touches them |

No gaps.

**2. Placeholder scan.** No TBD/TODO. Every step has the literal text to write or
the exact command to run with its expected output.

**3. Type consistency.** No types. The two cross-task references — the log string
`GitHub API` and the shell variable `$JOB` — are consistent between Task 1 Step 1
and Task 2 Steps 1-3.

**One deliberate deviation from the skill's template:** there are no
write-failing-test-first steps, because there is no test framework that can
observe a GitHub Actions token scope. Task 1's steps 2-4 and 6-7 are assertions
with exact expected output, which is the closest available equivalent, and Task 2
is the real verification against a captured baseline.
