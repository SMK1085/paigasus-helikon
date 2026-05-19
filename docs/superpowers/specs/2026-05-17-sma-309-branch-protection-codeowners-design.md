# SMA-309 — Branch protection + CODEOWNERS — design

- **Linear:** [SMA-309](https://linear.app/smaschek/issue/SMA-309/branch-protection-codeowners)
- **Branch:** `feature/sma-309-branch-protection-codeowners`
- **Status:** design (awaiting implementation plan)
- **Author:** Sven Maschek
- **Date:** 2026-05-17

## 1. Goal

Lock down `main` so all changes flow through PRs that pass CI, and stop stray branch names from being created in the first place. Make the configuration reproducible (checked-in JSON, idempotent apply script) so a fork or restore can recover the exact policy without web-UI clicking.

The spec captures three coupled artifacts: GitHub **Repository Rulesets** (modern replacement for classic branch protection), a `CODEOWNERS` file, and repository-level merge-method settings.

## 2. Decisions and rationale

Four foundational decisions, made during brainstorming, determine the shape of everything else:

| Decision | Choice | Rationale |
|---|---|---|
| Mechanism: classic branch protection vs Repository Rulesets | **Rulesets only** | Rulesets support per-actor bypass natively (classic doesn't for approvals), can express branch-name restriction in the same model, and are GitHub's actively-developed surface. One model, fewer moving parts. |
| Provisioning model | **Checked-in JSON + POSIX `sh` apply script** | Matches the project's reproducibility pattern (every CI gate, deny rule, workflow already lives in-repo). Replayable on a fork; auditable in diffs. Drift-check CI job was explicitly declined as out of scope. |
| Solo-maintainer self-approval | **Self-bypass via admin role** on the reviews ruleset only | Sole human contributor can't realistically wait for a second reviewer. Admin bypass on `main-protection-reviews` lets the maintainer self-merge once CI is green. The `main-protection-checks` ruleset has **no** bypass so status checks are still fully enforced — admin can't merge red. When a second human joins, approvals auto-engage for non-admins. |
| Squash-commit format | **Title = `PR_TITLE`, body = `BLANK`** | The CI-validated PR title becomes the squashed commit subject verbatim, so release-plz's Conventional-Commits parser sees the right type/scope. PR discussion stays on the PR. |

A natural consequence of the rulesets-only + targeted-bypass decisions is that the ticket's single "branch protection on main" idea splits into **two** rulesets on `main`, because GitHub Rulesets apply bypass at the ruleset level — not per-rule. A combined ruleset would mean admin bypass skips status checks too, which is unacceptable. Splitting puts status checks and review rules into separate rulesets with different bypass lists.

## 3. Files added / modified

### Added

| Path | Purpose |
|---|---|
| `.github/CODEOWNERS` | Review routing: `* @SMK1085`. |
| `.github/rulesets/main-protection-checks.json` | Required status checks, linear history, no force-push, no deletion on `main`. No bypass. |
| `.github/rulesets/main-protection-reviews.json` | 1 approval, dismiss stale, require CODEOWNERS review, require thread resolution on `main`. Admin role bypass. |
| `.github/rulesets/branch-names.json` | Regex restriction on all branches except `main`. Dependabot + release-plz bypass. |
| `scripts/apply-repo-config.sh` | Idempotent POSIX `sh` applier. Resolves bot App IDs at apply time; POST/PUTs rulesets; sets merge methods via `gh repo edit`. |

### Modified

| Path | Change |
|---|---|
| `CONTRIBUTING.md` | Update the branch-naming paragraph to drop the "once it lands" hedge; append a new "Repo configuration" top-level section describing the artifacts and how to re-apply. |
| `CLAUDE.md` | Replace the "required-status-check IDs" sentence in the "CI" section. Current text uses `ci / fmt`-style names; actual check-run contexts posted by GitHub Actions are bare job names (e.g. `fmt`). Empirically verified against PR #12's check-runs. |

## 4. Ruleset specifications

All three rulesets are scoped to the repository (`source_type: "Repository"`), enforcement is `"active"`, and the JSON conforms to GitHub's [Create a repository ruleset](https://docs.github.com/en/rest/repos/rules) request body.

### 4.1 `main-protection-checks.json`

```jsonc
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

Notes:
- `~DEFAULT_BRANCH` survives a default-branch rename without editing the file.
- Context strings are bare job names (no `ci /` prefix). This is what GitHub Actions posts as the check-run context; verified empirically against the latest `main` commit (PR #12).
- `strict_required_status_checks_policy: false` deliberately — turning it on forces every PR to be up-to-date with `main` before merging, adding rebase churn without meaningful safety gain on a low-traffic repo.
- Only the `(ubuntu-latest, stable)` matrix row of `test` is required. Other rows (`macos-latest`, `windows-latest`, `1.75` MSRV) run as signals per CLAUDE.md's existing convention.

### 4.2 `main-protection-reviews.json`

```jsonc
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

Notes:
- `actor_id: 5` is the GitHub-assigned ID for the **Admin** RepositoryRole. The maintainer holds this role on the repo and gets bypass.
- `allowed_merge_methods: ["squash"]` belt-and-suspenders the merge-type repo settings applied by `gh repo edit`.
- `require_last_push_approval: false` deliberately — turning it on forces re-approval after any push (doc tweak included) and adds friction without proportional safety gain for the current team size. Easy to flip later.

### 4.3 `branch-names.json`

```jsonc
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
    { "actor_id": 3742291,             "actor_type": "Integration", "bypass_mode": "always" }
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

Notes:
- `exclude: ["refs/heads/main"]` keeps `main` out of the regex check (it doesn't match `^(feature|hotfix)/…` and we don't want the ruleset competing with `main` protection).
- The `"DEPENDABOT_APP_ID"` string placeholder is intentional. The committed JSON is a valid JSON document with a string value in that slot; the apply script (§6) substitutes the whole quoted token with a bare numeric ID (29110 at the time of writing), producing a valid request body. Dependabot is a public GitHub App, so its ID is resolvable at apply time via the unauthenticated `/apps/dependabot` endpoint.
- The second bypass actor's `actor_id` is **hardcoded** to `3742291`. This is the maintainer's private GitHub App `paigasusbot` — the workflow's `RELEASE_PLZ_APP_ID` + `RELEASE_PLZ_APP_PRIVATE_KEY` secrets hold its credentials, and release-plz acts under this App's identity (not the public `release-plz` App). Private Apps return 404 from the public `/apps/{slug}` endpoint, so runtime resolution isn't possible without elevated auth. The App ID is verifiable from the bot user's avatar URL (`https://avatars.githubusercontent.com/in/3742291?v=4` — the path segment after `/in/` is the App ID).
- Fork users who want this ruleset to apply to their own infrastructure must edit `branch-names.json` to substitute their own App's ID (or remove the bypass entry entirely if they don't run release-plz).
- The regex uses unescaped `/` (GitHub's regex dialect does not treat `/` as a metacharacter). The Linear ticket's `\/` is JavaScript-flavored escaping; both forms work, but the unescaped form is clearer in raw JSON.

## 5. CODEOWNERS

```
# Ownership for review routing. With one owner, this functions mostly as
# auto-review-request. Path-specific stanzas can be added once a second
# human maintainer joins.

*  @SMK1085
```

That's the whole file. Path-scoped stanzas (e.g. `.github/`, `Cargo.toml`) on a solo repo would route reviews to the same person — empty scaffolding deliberately omitted.

## 6. `scripts/apply-repo-config.sh`

POSIX `sh`, idempotent (re-runs produce the same state), runnable from any maintainer machine with `gh auth login` already completed.

Behavior, in order:

1. **Preflight.** `gh auth status` → fail fast if unauthenticated. `command -v jq` → fail with install hint if missing.
2. **Resolve Integration IDs.** Call the public `GET /apps/dependabot` endpoint once and read `.id` (29110). Only dependabot needs runtime resolution; the second bypass actor (paigasusbot, ID 3742291) is hardcoded in `branch-names.json` because it is a private App and the public `/apps/{slug}` endpoint returns 404 for private Apps. Earlier drafts of this spec assumed the public `release-plz` App (ID 205377) would be the right bypass actor and added it via the same resolution mechanism — that was wrong on two counts: (a) the maintainer's repo authenticates release-plz via their own private App, not the public one; (b) the GitHub Rulesets API DOES validate that an `Integration` bypass actor is installed on the ruleset source, rejecting POST requests that reference uninstalled Apps with `Actor X must be part of the ruleset source or owner organization`.
3. **Apply each ruleset.** For each `.github/rulesets/*.json`:
   1. `sed` substitutes the literal token `"DEPENDABOT_APP_ID"` (including the surrounding double-quotes) with the resolved numeric App ID, producing a temp file with a valid request body. (Committed JSON keeps the string placeholder so the files remain portable across forks.) Files that don't reference the placeholder are passed through unchanged.
   2. Query `gh api /repos/SMK1085/paigasus-helikon/rulesets` for a ruleset with the same `name`.
   3. If absent: `POST /repos/SMK1085/paigasus-helikon/rulesets` with the temp JSON. Print `created`.
   4. If present: `PUT /repos/SMK1085/paigasus-helikon/rulesets/<id>` with the temp JSON. Print `updated`. (No client-side diff — PUT is idempotent at the resource level, so re-running with unchanged JSON converges to the same state.)
4. **Apply merge settings.** One `gh repo edit SMK1085/paigasus-helikon` call with:
   ```
   --enable-merge-commit=false
   --enable-rebase-merge=false
   --enable-squash-merge=true
   --delete-branch-on-merge=true
   --squash-merge-commit-title=PR_TITLE
   --squash-merge-commit-message=BLANK
   ```
5. **Summary.** Print `Applied 3 rulesets, repo settings updated.` and exit `0`.

No `--check` / `--dry-run` flag is added. Drift detection is out of scope; the maintainer running the script on next config edit is the drift signal.

## 7. CONTRIBUTING.md edits

### Edit 1 — branch-naming paragraph (line 7)

- **Before:** `(enforced via the SMA-309 repository ruleset once it lands)`
- **After:** ``(enforced via the `branch-names` repository ruleset; see `.github/rulesets/branch-names.json`)``

### Edit 2 — new top-level section

Insert after "Supply-chain security" and before "Releases":

```markdown
## Repo configuration

Branch protection, branch-name enforcement, CODEOWNERS, and merge-method
settings are checked in as JSON + a POSIX `sh` apply script:

| File | Purpose |
|---|---|
| `.github/CODEOWNERS` | Review routing — currently `* @SMK1085`. |
| `.github/rulesets/main-protection-checks.json` | Required status checks, linear history, no force-push, no deletion. Enforced on admins (no bypass). |
| `.github/rulesets/main-protection-reviews.json` | 1 approval, dismiss stale, CODEOWNERS review, thread resolution. Admin role bypass — solo-maintainer self-merge is intentional and will auto-engage for non-admins once a second human joins. |
| `.github/rulesets/branch-names.json` | `^(feature|hotfix)/[a-z0-9._-]+$` on all branches except `main`. Bypass: dependabot + release-plz integrations. |
| `scripts/apply-repo-config.sh` | Idempotent applier. Resolves bot App IDs at apply time and POST/PUTs each ruleset; sets merge methods + squash-commit format via `gh repo edit`. |

To re-apply (or replay on a fork) after `gh auth login`:

\`\`\`bash
bash scripts/apply-repo-config.sh
\`\`\`

There is no drift-check CI job — divergence is detected by the next maintainer
running the script, which is acceptable for the current cadence. A
follow-up can add one if needed.
```

## 8. CLAUDE.md correction

In the "CI" section, replace the sentence beginning *"The required-status-check IDs SMA-309 will gate merge on are…"* with:

> The required-status-check contexts gated on `main` are (bare job names, as posted by the GitHub Actions app): `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`. The canonical declaration is `.github/rulesets/main-protection-checks.json` (see CONTRIBUTING.md → "Repo configuration"). Other matrix variants (`test (macos-latest, …)`, `test (windows-latest, …)`, `test (…, 1.75)`) run as signals only.

This is the only CLAUDE.md change in this PR. The same edit absorbs the `audit / audit` / `deny / deny` naming (those are also bare job names).

## 9. Verification plan

Each acceptance criterion in the ticket maps to a concrete check after `scripts/apply-repo-config.sh` is run against the repo:

| Acceptance criterion | How verified |
|---|---|
| A PR with failing clippy cannot be merged. | Open throwaway PR with a deliberate clippy violation, e.g. add `assert!(true);` inside any test (always triggers `clippy::assertions_on_constants`, which `-D warnings` upgrades to an error). Merge button is disabled; failure cites the `clippy` required check. |
| A PR that drops doc coverage below 80% cannot be merged. | `doc-coverage` exits non-zero on under-threshold workspace; it is in the required-check list. Verified incidentally on the first conforming PR. |
| A PR with a non-Conventional-Commits title cannot be merged. | `pr-title` is in the required-check list. Verified on this PR itself — the PR title for SMA-309 must conform. |
| `git push origin wip-stuff` is rejected. | `git push origin HEAD:refs/heads/wip-stuff` from a scratch branch produces a remote rejection with "Branch name violates ruleset". |
| `feature/sma-312-core-traits` succeeds. | Push to `feature/sma-312-test-bypass` from the same scratch branch succeeds. Delete afterward. |
| Dependabot and release-plz can still create their own PR branches. | Both apps are bypass actors on `branch-names`. Verified by the next scheduled Dependabot Monday run, or manually via `gh api -X POST /repos/.../dispatches`. |

Two implicit checks worth verifying explicitly:

| Implicit check | How verified |
|---|---|
| Admin can self-merge once CI is green. | This PR itself. After CI goes green, "Merge pull request" is available without an approval. |
| Admin **cannot** self-merge with red CI. | Push a commit to this PR's branch that introduces a formatting violation (e.g. a stray trailing-whitespace line in any `.rs` file under `crates/`). Confirm the merge button greys out and cites the `fmt` check. Revert before merging for real. |

## 10. Deviations from the ticket

- **CODEOWNERS handle.** Ticket says `* @smaschek`. Implementation uses `* @SMK1085` (the maintainer's actual GitHub username; `smaschek` is the local/email handle).
- **Single ruleset → three rulesets.** Ticket describes "branch protection on main" as a single configuration. Implementation splits into `main-protection-checks` and `main-protection-reviews` so admin bypass can apply to reviews without skipping status checks. See §2.
- **Required-status-check naming.** Ticket lists `ci / fmt`, `cargo-deny`, etc. Implementation uses the actual check-run contexts: `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`. (CLAUDE.md has the same error, fixed in this PR — see §8.)
- **Release-plz bypass on review ruleset.** Ticket says "override for release-plz bot" on the approval requirement. Implementation omits release-plz from `main-protection-reviews` bypass — the maintainer reviews and merges the release PR by hand (per existing CONTRIBUTING.md §Releases), and admin bypass covers that. Release-plz only needs `branch-names` bypass, which it has.
- **release-plz App identity (discovered at apply time).** Earlier drafts of this spec assumed release-plz on this repo acted under the public `release-plz` GitHub App (ID 205377), so the apply script resolved that ID via `/apps/release-plz`. Both halves were wrong: (a) the maintainer's `release-plz.yml` workflow uses their **own private App** `paigasusbot` (ID 3742291), and (b) the Rulesets API rejects POSTs whose `Integration` bypass actors are not installed on the ruleset source. Resolution: hardcode `3742291` in `branch-names.json` (private Apps return 404 from `/apps/{slug}` so runtime resolution isn't possible) and drop the release-plz resolution from the apply script. Dependabot's bypass entry continues to use the placeholder + runtime resolution (29110, public App).

## 11. Risks

- **paigasusbot App re-creation changes its App ID.** If the maintainer deletes and re-registers the `paigasusbot` GitHub App, its new App ID will differ from `3742291` and the hardcoded bypass actor in `branch-names.json` will be silently inert (release-plz pushes to its branch prefix will be rejected by the branch-name rule). Mitigation: a re-create is a deliberate maintainer action and they will update the JSON in the same change. Dependabot's bypass continues to be resolved dynamically by the apply script and is unaffected by reinstalls.
- **`pr-title` check race.** `pr-title.yml` runs on `pull_request_target`. If the workflow has never run on a PR (e.g. immediate merge attempt before sync), the required check is missing — GitHub treats missing required checks as blocking, which is the desired behavior. No mitigation needed; documented for future debugging.
- **Self-bypass blast radius.** Admin bypass on `main-protection-reviews` is total for that ruleset (skips approvals, CODEOWNERS review, **and** thread resolution). The `checks` ruleset has no bypass, so the safety floor — green CI — is preserved.

## 12. Out of scope / follow-ups

- **Drift-check CI job.** Declined for this PR; can be added later as a `.github/workflows/repo-config.yml` calling `scripts/apply-repo-config.sh --check` on PRs touching `.github/rulesets/**`.
- **Path-scoped CODEOWNERS rules.** No second human owner exists to route to; revisit when one joins.
- **Permitting `chore/`, `docs/`, `refactor/` branch prefixes.** Ticket's follow-up section. One-line regex edit when wanted.
- **Auto-merge.** Not in ticket. `gh repo edit --enable-auto-merge=true` is a follow-up if queueing merges becomes useful.
