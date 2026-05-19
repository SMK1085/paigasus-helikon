# SMA-309 Branch Protection + CODEOWNERS Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lock down `main` and stop stray branch names by landing three GitHub Repository Rulesets, a `CODEOWNERS` file, and a `gh repo edit` setup — all driven from checked-in JSON + a POSIX `sh` apply script.

**Architecture:** Rulesets-only (not classic branch protection). Split into three rulesets so admin self-bypass applies only to PR reviews — status checks remain fully enforced. Configuration committed under `.github/rulesets/`; an idempotent script in `scripts/` resolves bot App IDs via the public `/apps/{slug}` endpoint and POST/PUTs each ruleset.

**Tech Stack:** GitHub REST Rulesets API, `gh` CLI, POSIX `sh`, `jq`. No Rust changes.

**Spec:** `docs/superpowers/specs/2026-05-17-sma-309-branch-protection-codeowners-design.md`

**Branch:** `feature/sma-309-branch-protection-codeowners` (already created and on the spec commit `3d03f0b`).

**Pre-resolved constants:**
- GitHub repo: `SMK1085/paigasus-helikon`
- Dependabot App ID: `29110`
- release-plz App ID: `205377`
- Admin RepositoryRole `actor_id`: `5`
- Default-branch macro: `~DEFAULT_BRANCH`

---

## File Structure

### New files

| Path | Purpose | Owner |
|---|---|---|
| `.github/CODEOWNERS` | Review-routing single-owner stub. | `SMK1085` |
| `.github/rulesets/main-protection-checks.json` | Status-checks + linear-history + force-push + deletion ruleset on `main`. No bypass. | Apply script |
| `.github/rulesets/main-protection-reviews.json` | `pull_request` rule with admin-role bypass. | Apply script |
| `.github/rulesets/branch-names.json` | Branch-name regex on all branches except `main`. Bots bypass. | Apply script |
| `scripts/apply-repo-config.sh` | Idempotent applier: resolves App IDs, POST/PUTs rulesets, `gh repo edit` merge settings. | Maintainer invocation |

### Modified files

| Path | Change |
|---|---|
| `CONTRIBUTING.md` | Replace the "once it lands" hedge in the branch-naming paragraph; append a new "Repo configuration" section. |
| `CLAUDE.md` | Replace the CI-section sentence about required-status-check IDs with the corrected bare-job-name list. |

---

## Task 1: Add `.github/CODEOWNERS`

**Files:**
- Create: `.github/CODEOWNERS`

- [ ] **Step 1: Write the file**

Path: `.github/CODEOWNERS`

```
# Ownership for review routing. With one owner, this functions mostly as
# auto-review-request. Path-specific stanzas can be added once a second
# human maintainer joins.

*  @SMK1085
```

- [ ] **Step 2: Verify GitHub will parse the file**

Run:

```bash
# Local syntax check — basic format expected by GitHub's parser.
# A valid line is: <pattern> <whitespace> <owner1> [<owner2>...]
# Comments start with #. Blank lines OK.
grep -vE '^(#|$)' .github/CODEOWNERS
```

Expected output (one line):

```
*  @SMK1085
```

- [ ] **Step 3: Stage and commit**

Run:

```bash
git add .github/CODEOWNERS
git commit -m "$(cat <<'EOF'
chore(repo): SMA-309 add CODEOWNERS

Single-owner stub: * @SMK1085. Path-specific stanzas are deliberately
omitted on a solo repo (no second human to route to).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds, hook passes (`convco check`).

---

## Task 2: Add `.github/rulesets/main-protection-checks.json`

**Files:**
- Create: `.github/rulesets/main-protection-checks.json`

- [ ] **Step 1: Make the directory**

Run:

```bash
mkdir -p .github/rulesets
```

- [ ] **Step 2: Write the file**

Path: `.github/rulesets/main-protection-checks.json`

```json
{
  "name": "main-protection-checks",
  "target": "branch",
  "enforcement": "active",
  "conditions": {
    "ref_name": {
      "include": ["~DEFAULT_BRANCH"],
      "exclude": []
    }
  },
  "bypass_actors": [],
  "rules": [
    {
      "type": "required_status_checks",
      "parameters": {
        "required_status_checks": [
          { "context": "fmt" },
          { "context": "clippy" },
          { "context": "test (ubuntu-latest, stable)" },
          { "context": "docs" },
          { "context": "doc-coverage" },
          { "context": "commits" },
          { "context": "pr-title" },
          { "context": "audit" },
          { "context": "deny" }
        ],
        "strict_required_status_checks_policy": false,
        "do_not_enforce_on_create": false
      }
    },
    { "type": "required_linear_history" },
    { "type": "non_fast_forward" },
    { "type": "deletion" }
  ]
}
```

- [ ] **Step 3: Verify JSON validity**

Run:

```bash
jq empty .github/rulesets/main-protection-checks.json
echo "exit: $?"
```

Expected:

```
exit: 0
```

(No output from `jq empty` means the file parses cleanly.)

- [ ] **Step 4: Verify all required contexts match real CI job names**

Run (sanity check against the latest `main` commit's actual check-runs):

```bash
SHA=$(git rev-parse origin/main)
gh api "repos/SMK1085/paigasus-helikon/commits/$SHA/check-runs" \
  --jq '[.check_runs[].name] | unique | sort'
```

Expected: includes `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `audit`, `deny`. (`commits` and `pr-title` only run on PRs, so they may not appear here — that's fine; they're verified once the PR opens.)

- [ ] **Step 5: Stage and commit**

Run:

```bash
git add .github/rulesets/main-protection-checks.json
git commit -m "$(cat <<'EOF'
chore(repo): SMA-309 add main-protection-checks ruleset

Required status checks + linear history + force-push + deletion
protection on the default branch. No bypass actors — fully enforced
on admins. Context strings are bare job names (no `ci /` prefix),
verified against actual check-run output.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 3: Add `.github/rulesets/main-protection-reviews.json`

**Files:**
- Create: `.github/rulesets/main-protection-reviews.json`

- [ ] **Step 1: Write the file**

Path: `.github/rulesets/main-protection-reviews.json`

```json
{
  "name": "main-protection-reviews",
  "target": "branch",
  "enforcement": "active",
  "conditions": {
    "ref_name": {
      "include": ["~DEFAULT_BRANCH"],
      "exclude": []
    }
  },
  "bypass_actors": [
    { "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always" }
  ],
  "rules": [
    {
      "type": "pull_request",
      "parameters": {
        "required_approving_review_count": 1,
        "dismiss_stale_reviews_on_push": true,
        "require_code_owner_review": true,
        "require_last_push_approval": false,
        "required_review_thread_resolution": true,
        "allowed_merge_methods": ["squash"]
      }
    }
  ]
}
```

- [ ] **Step 2: Verify JSON validity**

Run:

```bash
jq empty .github/rulesets/main-protection-reviews.json
echo "exit: $?"
```

Expected: `exit: 0`.

- [ ] **Step 3: Stage and commit**

Run:

```bash
git add .github/rulesets/main-protection-reviews.json
git commit -m "$(cat <<'EOF'
chore(repo): SMA-309 add main-protection-reviews ruleset

PR-review rule on the default branch: 1 approval, dismiss stale on push,
CODEOWNERS review required, thread resolution required, squash-only
merge method. Admin RepositoryRole (actor_id 5) is the sole bypass
actor, enabling solo-maintainer self-merge once CI is green. Approvals
re-engage automatically for non-admin contributors when a second human
joins.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 4: Add `.github/rulesets/branch-names.json`

**Files:**
- Create: `.github/rulesets/branch-names.json`

- [ ] **Step 1: Write the file**

Path: `.github/rulesets/branch-names.json`

```json
{
  "name": "branch-names",
  "target": "branch",
  "enforcement": "active",
  "conditions": {
    "ref_name": {
      "include": ["~ALL"],
      "exclude": ["refs/heads/main"]
    }
  },
  "bypass_actors": [
    { "actor_id": "DEPENDABOT_APP_ID",  "actor_type": "Integration", "bypass_mode": "always" },
    { "actor_id": "RELEASE_PLZ_APP_ID", "actor_type": "Integration", "bypass_mode": "always" }
  ],
  "rules": [
    {
      "type": "branch_name_pattern",
      "parameters": {
        "operator": "regex",
        "pattern": "^(feature|hotfix)/[a-z0-9._-]+$",
        "negate": false,
        "name": "Branch name must be feature/<slug> or hotfix/<slug>"
      }
    }
  ]
}
```

The string-valued `"actor_id"` placeholders are intentional. The committed JSON is structurally valid; the apply script (Task 5) substitutes the whole quoted token (`"DEPENDABOT_APP_ID"` → `29110`, `"RELEASE_PLZ_APP_ID"` → `205377`) to produce a numerically-typed `actor_id` in the request body.

- [ ] **Step 2: Verify JSON validity**

Run:

```bash
jq empty .github/rulesets/branch-names.json
echo "exit: $?"
```

Expected: `exit: 0`.

- [ ] **Step 3: Verify the regex against the spec's allowed / rejected examples**

Run (sanity-check the pattern in Python — POSIX `grep -E` differs slightly from PCRE):

```bash
python3 - <<'PY'
import re
PATTERN = r"^(feature|hotfix)/[a-z0-9._-]+$"
allowed = ["feature/sma-312-core-traits", "hotfix/sma-451-token-leak", "feature/sma-309-branch-protection-codeowners"]
rejected = ["wip", "my-stuff", "Feature/Foo", "feature/", "feature/foo bar"]
for b in allowed:
    assert re.match(PATTERN, b), f"should match: {b}"
for b in rejected:
    assert not re.match(PATTERN, b), f"should NOT match: {b}"
print("regex passes all spec examples")
PY
```

Expected:

```
regex passes all spec examples
```

- [ ] **Step 4: Stage and commit**

Run:

```bash
git add .github/rulesets/branch-names.json
git commit -m "$(cat <<'EOF'
chore(repo): SMA-309 add branch-names ruleset

Regex restriction `^(feature|hotfix)/[a-z0-9._-]+$` on all branches
except `main`. Bypass: dependabot + release-plz GitHub Apps (by stable
public App ID). Committed-form JSON uses quoted string placeholders for
the Integration `actor_id`s; the apply script substitutes them to bare
numerics at runtime.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 5: Add `scripts/apply-repo-config.sh`

**Files:**
- Create: `scripts/apply-repo-config.sh`

- [ ] **Step 1: Write the script**

Path: `scripts/apply-repo-config.sh`

```bash
#!/bin/sh
# Apply repo-level configuration (rulesets + merge settings) idempotently.
#
# Usage:
#   gh auth login              # one-time; needs `repo` scope minimum
#   bash scripts/apply-repo-config.sh
#
# What this script does:
#   1. Preflight: ensure `gh` is authenticated and `jq` is installed.
#   2. Resolve dependabot + release-plz GitHub App IDs via the public
#      /apps/{slug} endpoint (no installation check — App IDs are global
#      constants and the ruleset accepts them regardless of install
#      status).
#   3. For each .github/rulesets/*.json, substitute App ID placeholders
#      and POST (create) or PUT (update) via the rulesets API.
#   4. Apply merge-method and squash-format settings via `gh repo edit`.
#
# Idempotent: re-running converges to the same state.

set -eu

REPO="SMK1085/paigasus-helikon"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RULESET_DIR="$SCRIPT_DIR/../.github/rulesets"

# ---------- 1. Preflight ----------

if ! gh auth status >/dev/null 2>&1; then
    echo "ERROR: 'gh' is not authenticated. Run 'gh auth login' first." >&2
    exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: 'jq' is required but not installed." >&2
    echo "       macOS:  brew install jq" >&2
    echo "       Linux:  apt-get install jq  (or your distro equivalent)" >&2
    exit 1
fi

# ---------- 2. Resolve App IDs ----------

DEPENDABOT_APP_ID="$(gh api /apps/dependabot --jq .id)"
if [ -z "$DEPENDABOT_APP_ID" ]; then
    echo "ERROR: Could not resolve dependabot App ID via /apps/dependabot." >&2
    exit 1
fi

RELEASE_PLZ_APP_ID="$(gh api /apps/release-plz --jq .id)"
if [ -z "$RELEASE_PLZ_APP_ID" ]; then
    echo "ERROR: Could not resolve release-plz App ID via /apps/release-plz." >&2
    exit 1
fi

echo "Resolved App IDs: dependabot=$DEPENDABOT_APP_ID release-plz=$RELEASE_PLZ_APP_ID"

# ---------- 3. Apply rulesets ----------

EXISTING_RULESETS_JSON="$(gh api "repos/$REPO/rulesets")"

RULESET_COUNT=0
for ruleset_file in "$RULESET_DIR"/*.json; do
    RULESET_COUNT=$((RULESET_COUNT + 1))
    name="$(jq -r '.name' < "$ruleset_file")"
    tmp_file="$(mktemp)"
    # Substitute the literal quoted placeholders with bare numerics.
    # The quoted-token approach keeps the committed JSON parseable
    # while still producing a numerically-typed actor_id post-substitution.
    sed \
        -e "s/\"DEPENDABOT_APP_ID\"/$DEPENDABOT_APP_ID/g" \
        -e "s/\"RELEASE_PLZ_APP_ID\"/$RELEASE_PLZ_APP_ID/g" \
        "$ruleset_file" > "$tmp_file"

    existing_id="$(printf '%s' "$EXISTING_RULESETS_JSON" \
        | jq -r --arg name "$name" '.[] | select(.name == $name) | .id' \
        | head -1)"

    if [ -z "$existing_id" ]; then
        gh api -X POST "repos/$REPO/rulesets" --input "$tmp_file" >/dev/null
        printf '  %-30s created\n' "$name"
    else
        gh api -X PUT "repos/$REPO/rulesets/$existing_id" --input "$tmp_file" >/dev/null
        printf '  %-30s updated\n' "$name"
    fi
    rm -f "$tmp_file"
done

# ---------- 4. Apply merge settings ----------

gh repo edit "$REPO" \
    --enable-merge-commit=false \
    --enable-rebase-merge=false \
    --enable-squash-merge=true \
    --delete-branch-on-merge=true \
    --squash-merge-commit-title=PR_TITLE \
    --squash-merge-commit-message=BLANK

echo "Applied $RULESET_COUNT rulesets, repo settings updated."
```

- [ ] **Step 2: Make the script executable**

Run:

```bash
chmod +x scripts/apply-repo-config.sh
```

- [ ] **Step 3: Run shellcheck if available, otherwise skip with a note**

Run:

```bash
if command -v shellcheck >/dev/null 2>&1; then
    shellcheck scripts/apply-repo-config.sh
    echo "shellcheck: clean"
else
    echo "shellcheck not installed — skipping (optional; install with 'brew install shellcheck')"
fi
```

Expected: either `shellcheck: clean` (no warnings printed) or the "not installed" message. **If shellcheck flags warnings, fix them inline before moving on** — common issues:
- Quote variable expansions: `"$VAR"` not `$VAR`.
- Use `printf` not `echo -n` for portability.
- Avoid `$(...)` inside `[ ... ]`; use `[ -n "$(...)" ]`.

- [ ] **Step 4: Smoke-test the preflight checks**

Verify the script fails gracefully on missing `jq` (simulate by overriding `PATH`):

```bash
# Should exit 1 with the jq-not-installed message.
( PATH="/usr/bin:/bin" bash scripts/apply-repo-config.sh ) 2>&1 | head -5
echo "---"
echo "exit: $?"
```

Expected: an `ERROR: 'jq' is required but not installed.` line (or similar). Exit code may show as `0` due to the subshell + pipe — the diagnostic is in the stderr message.

(We deliberately do NOT smoke-test the apply path here — that would mutate the live repo. The real run is Task 9.)

- [ ] **Step 5: Stage and commit**

Run:

```bash
git add scripts/apply-repo-config.sh
git commit -m "$(cat <<'EOF'
chore(repo): SMA-309 add apply-repo-config.sh

Idempotent applier for the SMA-309 ruleset + repo-settings policy.
Resolves dependabot / release-plz App IDs via the public
/apps/{slug} endpoint, POST/PUTs each ruleset by name, and sets
merge-method + squash-commit-format via `gh repo edit`. No drift-check
flag (declined as out of scope); divergence is detected by the next
maintainer running the script.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 6: Update `CONTRIBUTING.md`

**Files:**
- Modify: `CONTRIBUTING.md`

- [ ] **Step 1: Tighten the existing branch-naming paragraph**

Edit line 7 of `CONTRIBUTING.md`.

- Before:
  ```
  All non-bot branches must match this regex (enforced via the SMA-309 repository ruleset once it lands):
  ```
- After:
  ```
  All non-bot branches must match this regex (enforced via the `branch-names` repository ruleset; see `.github/rulesets/branch-names.json`):
  ```

- [ ] **Step 2: Append a new "Repo configuration" section**

Insert *after* the "Supply-chain security" section (which currently ends with the Dependabot paragraph at line 240) and *before* the "Releases" section (which starts at line 242 with `## Releases`).

Content to insert:

````markdown
## Repo configuration

Branch protection, branch-name enforcement, CODEOWNERS, and merge-method
settings are checked in as JSON + a POSIX `sh` apply script:

| File | Purpose |
|---|---|
| `.github/CODEOWNERS` | Review routing — currently `* @SMK1085`. |
| `.github/rulesets/main-protection-checks.json` | Required status checks, linear history, no force-push, no deletion. Enforced on admins (no bypass). |
| `.github/rulesets/main-protection-reviews.json` | 1 approval, dismiss stale, CODEOWNERS review, thread resolution. Admin role bypass — solo-maintainer self-merge is intentional and will auto-engage for non-admins once a second human joins. |
| `.github/rulesets/branch-names.json` | `^(feature\|hotfix)/[a-z0-9._-]+$` on all branches except `main`. Bypass: dependabot + release-plz integrations. |
| `scripts/apply-repo-config.sh` | Idempotent applier. Resolves bot App IDs at apply time and POST/PUTs each ruleset; sets merge methods + squash-commit format via `gh repo edit`. |

To re-apply (or replay on a fork) after `gh auth login`:

```bash
bash scripts/apply-repo-config.sh
```

There is no drift-check CI job — divergence is detected by the next maintainer
running the script, which is acceptable for the current cadence. A
follow-up can add one if needed.
````

(The `|` inside the table cell uses HTML-style escaping `\|` because raw `|` would close the cell.)

- [ ] **Step 3: Render-check by spot-checking line numbers**

Run:

```bash
grep -n "branch-names" CONTRIBUTING.md
grep -n "^## Repo configuration" CONTRIBUTING.md
grep -n "^## Releases" CONTRIBUTING.md
```

Expected: `branch-names` appears at two lines (the old paragraph + the new table). `## Repo configuration` appears once. `## Repo configuration` line number is less than `## Releases` line number.

- [ ] **Step 4: Stage and commit**

Run:

```bash
git add CONTRIBUTING.md
git commit -m "$(cat <<'EOF'
docs(contributing): SMA-309 document the repo-config policy and apply flow

Tightens the branch-naming paragraph (drops the "once it lands" hedge
now that the ruleset is real) and appends a "Repo configuration"
section enumerating the checked-in JSON files and the apply script.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 7: Correct `CLAUDE.md`

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Locate the wrong sentence**

Run:

```bash
grep -n "The required-status-check IDs SMA-309" CLAUDE.md
```

Expected: one match (the sentence we're replacing).

- [ ] **Step 2: Replace the sentence**

Edit `CLAUDE.md`. Find this sentence in the "CI" section:

```
The required-status-check IDs SMA-309 will gate merge on are: `ci / fmt`, `ci / clippy`, `ci / test (ubuntu-latest, stable)`, `ci / docs`, `ci / doc-coverage`, `ci / commits`, and `pr-title / pr-title` (the last two from SMA-335). Other matrix variants run as signals.
```

Replace it with:

```
The required-status-check contexts gated on `main` are (bare job names, as posted by the GitHub Actions app): `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`. The canonical declaration is `.github/rulesets/main-protection-checks.json` (see CONTRIBUTING.md → "Repo configuration"). Other matrix variants (`test (macos-latest, …)`, `test (windows-latest, …)`, `test (…, 1.75)`) run as signals only.
```

**Second edit in the same section.** A few lines below, CLAUDE.md has a second sentence that mentions the supply-chain check IDs (currently at line 79). Find this sentence:

```
Required status checks added in SMA-306: `audit / audit`, `deny / deny`.
```

Replace it with:

```
Required status checks added in SMA-306: `audit`, `deny` (declared in `.github/rulesets/main-protection-checks.json` alongside the CI gates).
```

Both edits should be applied in this same commit.

- [ ] **Step 3: Verify no `ci /` prefix remains in CLAUDE.md when referring to check IDs**

Run:

```bash
grep -nE 'ci / (fmt|clippy|test|docs|doc-coverage|commits)|pr-title / pr-title|audit / audit|deny / deny' CLAUDE.md
```

Expected: **no matches** (empty output, exit 1). If any match remains, fix it.

- [ ] **Step 4: Stage and commit**

Run:

```bash
git add CLAUDE.md
git commit -m "$(cat <<'EOF'
docs(claude): SMA-309 correct required-status-check naming

The actual check-run contexts posted by GitHub Actions are bare job
names (`fmt`, `clippy`, ...), not the `ci / fmt` form CLAUDE.md
documented. Empirically verified against PR #12's check-runs. Updated
the CI-section sentence to match reality and point at the canonical
ruleset JSON.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

## Task 8: Push branch, open PR, wait for CI

**Files:** none (git/GitHub actions only)

- [ ] **Step 1: Push the branch**

Run:

```bash
git push -u origin feature/sma-309-branch-protection-codeowners
```

Expected: push succeeds. The branch is created on `origin`.

Note: the `branch-names` ruleset is NOT yet active — it lands when Task 9 runs the apply script. So this push goes through unrestricted regardless of whether the branch name matches the regex (which it does anyway: `feature/sma-309-…`).

- [ ] **Step 2: Open the PR**

Run:

```bash
gh pr create \
  --base main \
  --head feature/sma-309-branch-protection-codeowners \
  --title "feat(repo): SMA-309 branch protection + CODEOWNERS via rulesets" \
  --body "$(cat <<'EOF'
## Summary

- Adds `.github/CODEOWNERS` (single owner: `@SMK1085`).
- Adds three Repository Rulesets as checked-in JSON:
  - `main-protection-checks` — required status checks, linear history, no force-push, no deletion. No bypass.
  - `main-protection-reviews` — 1 approval + CODEOWNERS review + dismiss stale + thread resolution. Admin bypass for solo-maintainer self-merge.
  - `branch-names` — `^(feature|hotfix)/[a-z0-9._-]+$` on all branches except `main`. Dependabot + release-plz bypass.
- Adds `scripts/apply-repo-config.sh` (idempotent applier).
- Updates `CONTRIBUTING.md` ("Repo configuration" section) and corrects `CLAUDE.md` (required-status-check naming).

Spec: `docs/superpowers/specs/2026-05-17-sma-309-branch-protection-codeowners-design.md`

## Test plan

- [ ] CI passes (fmt, clippy, test, docs, doc-coverage, commits, pr-title, audit, deny).
- [ ] After running `bash scripts/apply-repo-config.sh`, `gh api repos/SMK1085/paigasus-helikon/rulesets` returns 3 entries.
- [ ] Pushing a branch named `wip` to `origin` is rejected with a branch-name ruleset violation.
- [ ] Pushing a branch named `feature/test-bypass-sma-309` succeeds.
- [ ] Forcing a fmt failure on this PR greys out the merge button.
- [ ] Merging via squash produces a commit titled exactly the PR title.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: PR is created. `gh` prints the PR URL. Record it as `$PR_URL` for later steps.

- [ ] **Step 3: Wait for CI to finish**

Run:

```bash
gh pr checks --watch
```

Expected: all checks (`fmt`, `clippy`, the `test` matrix, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`, `verify` from msrv) report green. If anything fails, stop here and fix the underlying problem.

Note: until the apply script runs (Task 9), no checks are *required* — they all run as advisory. We still want them green before flipping the switch so the apply doesn't trap this PR in an unmergeable state.

---

## Task 9: Apply the configuration to GitHub

**Files:** none (live API mutation)

- [ ] **Step 1: Confirm `gh` is authenticated as a repo admin**

Run:

```bash
gh auth status
gh api repos/SMK1085/paigasus-helikon --jq '.permissions'
```

Expected: `gh auth status` shows logged in to `SMK1085`. The permissions object includes `"admin": true`.

- [ ] **Step 2: Run the apply script**

Run:

```bash
bash scripts/apply-repo-config.sh
```

Expected output (something like):

```
Resolved App IDs: dependabot=29110 release-plz=205377
  main-protection-checks         created
  main-protection-reviews        created
  branch-names                   created
✓ Updated repository "SMK1085/paigasus-helikon"
Applied 3 rulesets, repo settings updated.
```

If any line says `updated` instead of `created`, that indicates a leftover ruleset by that name — investigate before continuing.

- [ ] **Step 3: Verify the three rulesets exist via the API**

Run:

```bash
gh api repos/SMK1085/paigasus-helikon/rulesets --jq '.[] | {id, name, enforcement, target}'
```

Expected: three entries, each with `enforcement: "active"`, `target: "branch"`, names `main-protection-checks`, `main-protection-reviews`, `branch-names`.

- [ ] **Step 4: Verify each ruleset's contents round-trip correctly**

For each ruleset, pull its full body and spot-check key fields. Run:

```bash
for name in main-protection-checks main-protection-reviews branch-names; do
    id=$(gh api repos/SMK1085/paigasus-helikon/rulesets --jq ".[] | select(.name == \"$name\") | .id")
    echo "=== $name (id=$id) ==="
    gh api "repos/SMK1085/paigasus-helikon/rulesets/$id" --jq '{name, bypass_actors, rules: [.rules[] | {type}]}'
done
```

Expected:
- `main-protection-checks`: `bypass_actors: []`; rule types include `required_status_checks`, `required_linear_history`, `non_fast_forward`, `deletion`.
- `main-protection-reviews`: `bypass_actors` contains one entry with `actor_type: "RepositoryRole"`, `actor_id: 5`; rule types include `pull_request`.
- `branch-names`: `bypass_actors` contains two entries with `actor_type: "Integration"`, `actor_id: 29110` and `actor_id: 205377`; rule types include `branch_name_pattern`.

- [ ] **Step 5: Verify the merge-method settings**

Run:

```bash
gh api repos/SMK1085/paigasus-helikon --jq '{
  merge_commit_allowed,
  rebase_merge_allowed,
  squash_merge_allowed,
  delete_branch_on_merge,
  squash_merge_commit_title,
  squash_merge_commit_message
}'
```

Expected:

```json
{
  "merge_commit_allowed": false,
  "rebase_merge_allowed": false,
  "squash_merge_allowed": true,
  "delete_branch_on_merge": true,
  "squash_merge_commit_title": "PR_TITLE",
  "squash_merge_commit_message": "BLANK"
}
```

---

## Task 10: Verify acceptance criteria against the live repo

**Files:** none (live verification only)

- [ ] **Step 1: Verify that `git push origin wip-stuff` is rejected**

Run (from any local clone):

```bash
git push origin HEAD:refs/heads/wip-stuff 2>&1 | tee /tmp/sma-309-rejection.log
echo "exit: $?"
```

Expected: push **fails** with a remote rejection mentioning the branch-name ruleset (something like `Branch name does not match required pattern`). Exit code non-zero.

- [ ] **Step 2: Verify that a conformant scratch branch can be pushed**

Run:

```bash
git push origin HEAD:refs/heads/feature/sma-309-test-bypass
echo "exit: $?"
```

Expected: push succeeds, exit `0`.

- [ ] **Step 3: Clean up the scratch branch**

Run:

```bash
git push origin --delete feature/sma-309-test-bypass
```

Expected: branch deleted on origin.

- [ ] **Step 4: Verify the SMA-309 PR's merge button reflects the required-checks status**

Run:

```bash
gh pr view --json mergeable,mergeStateStatus,statusCheckRollup --jq '{mergeable, mergeStateStatus, failed: [.statusCheckRollup[] | select(.conclusion == "FAILURE") | .name]}'
```

Expected: `mergeable: "MERGEABLE"`, `mergeStateStatus: "CLEAN"` (or `"HAS_HOOKS"` — both indicate the merge can proceed), `failed: []`.

If `mergeable` is `CONFLICTING` or `mergeStateStatus` is `BLOCKED`, the required-check list doesn't match what's posted on the PR — investigate which check is missing or failing.

- [ ] **Step 5 (optional, only if you want to prove the negative): Verify that breaking `fmt` blocks merge**

Run from the feature branch worktree:

```bash
# Introduce a stray tab in any .rs file under crates/
echo "	" >> crates/paigasus-helikon-core/src/lib.rs
git commit -am "test(repo): SMA-309 deliberately break fmt"
git push
gh pr checks --watch
```

Expected: `fmt` reports FAILURE. Then check:

```bash
gh pr view --json mergeStateStatus --jq .mergeStateStatus
```

Expected: `"BLOCKED"`.

Revert before continuing:

```bash
git reset --hard HEAD~1
git push --force-with-lease
gh pr checks --watch
```

Expected: `fmt` back to green; `mergeStateStatus` back to `CLEAN`.

(This step is optional because it's PR-mutating; skip it if you want to keep the PR history clean. The other verification steps cover the essential acceptance criteria.)

- [ ] **Step 6: Verify the PR-title check is required**

Run:

```bash
gh pr view --json statusCheckRollup --jq '[.statusCheckRollup[] | {name, conclusion}] | map(select(.name == "pr-title"))'
```

Expected: one entry with `conclusion: "SUCCESS"` (the PR title conforms to Conventional Commits).

---

## Task 11: Merge the PR and confirm follow-through

**Files:** none

- [ ] **Step 1: Squash-merge the PR**

Run:

```bash
gh pr merge --squash --delete-branch
```

Expected: PR merges (no approval required because of admin bypass on `main-protection-reviews`). The branch is deleted on origin. The squash commit on `main` has the PR title exactly as its subject (per the `squash_merge_commit_title=PR_TITLE` setting).

- [ ] **Step 2: Verify the squashed commit on `main`**

Run:

```bash
git fetch origin main
git log -1 --pretty=format:'%s%n%n%b' origin/main
```

Expected: subject line is exactly `feat(repo): SMA-309 branch protection + CODEOWNERS via rulesets`. Body is empty (or contains only the `Co-Authored-By` trailer if any).

- [ ] **Step 3: Verify Linear auto-closed SMA-309**

Run:

```bash
gh api graphql -f query='{ user(login: "SMK1085") { login } }' >/dev/null  # warmup, optional
# Then check via Linear MCP — done by the agent or the user.
```

The Linear MCP `get_issue SMA-309` should now show `status: "Done"` and a non-null `completedAt`. Per the project's memory, Linear auto-closes on PR merge; no manual transition is required.

- [ ] **Step 4: Final sanity sweep**

Run:

```bash
echo "=== rulesets ==="
gh api repos/SMK1085/paigasus-helikon/rulesets --jq '.[] | .name'
echo "=== merge settings ==="
gh api repos/SMK1085/paigasus-helikon --jq '{merge_commit_allowed, rebase_merge_allowed, squash_merge_allowed, squash_merge_commit_title, squash_merge_commit_message}'
echo "=== CODEOWNERS on main ==="
git show origin/main:.github/CODEOWNERS
echo "=== last commit ==="
git log -1 origin/main --oneline
```

Expected:
- Three rulesets listed.
- Merge settings show squash-only with `PR_TITLE` / `BLANK`.
- CODEOWNERS contents include `*  @SMK1085`.
- Last commit on `main` is the squashed SMA-309 commit.

---

## Notes for the implementer

- **Commit hook:** `convco check` runs locally on every commit (installed by `cargo-husky`). Every commit message in this plan is pre-validated against `convco`'s rules; if a hook fails, the message is wrong — re-check the type/scope.
- **PR title is the source of truth:** because of the squash-commit-title=`PR_TITLE` setting, the squashed commit on `main` will be exactly the PR title. The plan's Task 8 sets `feat(repo): SMA-309 branch protection + CODEOWNERS via rulesets`. If you change that, change it everywhere consistently (PR title, this plan, the spec's "Goal" rephrasing if needed).
- **Bot bypass IDs are global:** dependabot=29110 and release-plz=205377 are stable across all GitHub repos. They don't change on App reinstall.
- **Re-runnability:** every step in Tasks 1-7 is idempotent (file write + commit; conventional `git add` + commit). The apply script in Task 9 is idempotent at the resource level. If a run is interrupted, re-running picks up cleanly.
- **Memory note for the agent:** SMA-309 status will be auto-closed by Linear on PR merge — do not call `mcp__claude_ai_Linear__save_issue` to transition state manually. (See `feedback_linear_auto_closes_on_merge` memory.)
