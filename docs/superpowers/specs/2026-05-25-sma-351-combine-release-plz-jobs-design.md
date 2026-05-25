# SMA-351 — Combine release-plz jobs to eliminate App-token revoke/mint race — design

- **Linear:** [SMA-351](https://linear.app/smaschek/issue/SMA-351/combine-release-plz-jobs-to-eliminate-app-token-revokemint-race)
- **Branch:** `feature/sma-351-combine-release-plz-jobs-to-eliminate-app-token-revokemint`
- **Status:** design (awaiting implementation plan)
- **Author:** Sven Maschek
- **Date:** 2026-05-25
- **Related:** [SMA-349](https://linear.app/smaschek/issue/SMA-349) (where the SHA-pin + `persist-credentials: false` convention for `ci.yml` was established), [SMA-350](https://linear.app/smaschek/issue/SMA-350) (introduced the current two-job structure with `client-id` auth)

## 1. Goal & non-goals

**Goal.** Eliminate the transient `HTTP 401` failure that hit the SMA-349-merge release-plz run by collapsing the two-job structure into a single job that mints exactly one GitHub App installation token and revokes it exactly once at job end. While in the file, apply the SHA-pin + `persist-credentials: false` hygiene that landed for `ci.yml` in SMA-349.

The failure pattern, recorded in the Linear ticket, was:

| Time | Event |
| --- | --- |
| 07:45:31 | `release-plz-release` mints token A |
| 07:45:34 | token A used successfully for `viewer` query |
| 07:45:35 | token A revoked by Post step |
| 07:45:44 | `release-plz-pr` mints token B |
| 07:45:47 | token B used → **401** |

A manual re-run nine minutes later succeeded with no workflow change, confirming a timing-window issue at GitHub's installation-token endpoint rather than a config bug. The empirical hypothesis: a brief window after an explicit `DELETE /installation/token` during which freshly-minted tokens for the same installation are not honored. Collapsing the two jobs removes the mid-flight revoke entirely.

**Non-goals.**

- Not changing `release-plz.toml` or any per-crate release config.
- Not changing the paigasusbot App's permissions, installation, or secrets (`RELEASE_PLZ_APP_CLIENT_ID`, `RELEASE_PLZ_APP_PRIVATE_KEY`, `CARGO_REGISTRY_TOKEN` all stay).
- Not touching other workflows. Only `release-plz.yml` exhibits the dual-job pattern; `ci.yml` doesn't use App tokens.
- Not adding `Swatinem/rust-cache` to speed up `cargo publish`. Discussed during brainstorming and deliberately deferred — keeps the diff focused on the race fix + SHA pinning. (Follow-up ticket if release-plz publish latency becomes noticeable.)
- Not adding `workflow_dispatch`. The release-plz workflow is intentionally `push: [main]`-only by SMA-307's design.
- Not bumping `actions/create-github-app-token` or `release-plz/action` to a new major. Both are still on v3.x and v0.5.x respectively (no v4 or v1 has shipped as of 2026-05-25).

## 2. File layout

```text
.github/workflows/release-plz.yml                                        (modified — two jobs collapse to one;
                                                                          all `uses:` lines SHA-pinned;
                                                                          checkout gains `persist-credentials: false`)
docs/superpowers/specs/2026-05-25-sma-351-combine-release-plz-jobs-design.md  (new — this spec)
docs/superpowers/plans/2026-05-25-sma-351-combine-release-plz-jobs.md         (new — implementation plan, written next)
```

No changes to any crate's `src/`, to `Cargo.toml` (root or member), to `release-plz.toml`, to other workflows (`ci.yml`, `msrv.yml`, `audit.yml`, `deny.yml`, `sbom.yml`, `pr-title.yml`), to `deny.toml`, to `.github/rulesets/main-protection-checks.json`, to `CLAUDE.md`, or to `CONTRIBUTING.md`. No new tests.

## 3. Decisions and rationale

| Decision | Choice | Rationale |
| --- | --- | --- |
| Architecture | **Single job, two sequential `release-plz/action` steps, sharing one App token mint.** Token is revoked exactly once by the single mint's Post step at job end. | Directly addresses the root cause (mid-flight revoke followed by re-mint). Alternatives considered and rejected: (a) keep two jobs and drop the explicit revoke — doesn't help, you'd still mint two tokens and the second could still race; (b) `sleep 30` in the second job — hacky, relies on an undocumented timing window; (c) use the default `GITHUB_TOKEN` for `release-pr` — PRs opened by `GITHUB_TOKEN` don't trigger downstream workflows per GitHub's anti-loop policy, so the release PR's required status checks would never run; (d) revert to a PAT — undoes SMA-350's whole motivation. |
| Job key / `name:` | **`release-plz`** (matches the Linear ticket's sketch). | No required-status-check on `main` references either of the old job names (`release-plz-release`, `release-plz-pr`); release-plz runs on push-to-main only, not on PRs, so it's never a PR-gating context. Verified against `.github/rulesets/main-protection-checks.json`. Renaming is safe; no ruleset edit needed. |
| Step ordering | **`release` first, then `release-pr`** — same order the existing `needs:` dependency enforced. | `release` may push tags (when a release PR is merged); `release-pr` then recomputes the rolling PR relative to those tags. Reversing would propose a PR against stale state for the duration of one workflow run. |
| Re-checkout between steps | **None.** | `release-plz release` only publishes crates and pushes tags; it never moves `main`'s branch HEAD (the rolling release PR lands its bumps on a `release-plz-…` branch, not main). The working tree the runner has after `release` is exactly what `release-pr` needs. |
| `CARGO_REGISTRY_TOKEN` scope | **Step-scoped on the `release` step only**, not job-level env. | `release-pr` doesn't publish; least-privilege says the token shouldn't be in its environment. Matches the Linear sketch. |
| SHA-pinning convention | **Above-the-fold `# <action> vX.Y.Z` comment per `uses:` line**, identical to `ci.yml`'s convention from SMA-349. | Single canonical hygiene pattern across all workflows; Dependabot's `github-actions` group can track patch/minor updates via the SHA + comment together. |
| `persist-credentials: false` on checkout | **Yes.** | Matches every `actions/checkout` invocation in `ci.yml`. The default `persist-credentials: true` writes the workflow's `GITHUB_TOKEN` into `.git/config` for the rest of the job — unnecessary here because all git operations are driven by `release-plz/action` using the App token, not the workflow token. |
| Top-level `concurrency:`, `permissions:`, `env:` blocks | **Unchanged.** | The single-job structure has the same scheduling and permissions surface as the two-job structure. `cancel-in-progress: false` still protects against mid-tag-push cancellation. |
| Adding caching | **No.** | Decided in brainstorming. Out of the ticket's explicit scope; release-plz runs are infrequent (one per merge to main), so the cache hit rate is low and the diff stays minimal. |
| Action SHAs | Resolved at spec-write time against `gh api repos/<owner>/<repo>/releases/latest` per the CLAUDE.md "always implement against the latest stable major" rule. | See §4. No new majors have shipped; staying on `actions/create-github-app-token@v3` and `release-plz/action@v0.5` is correct. |
| Commit shape | **One commit on the feature branch**, squash-merged to a single commit on `main`. Type/scope `ci(workflows)`. | `ci(workflows): SMA-351 combine release-plz jobs to share one App token mint` (lowercase verb after the SMA-### prefix — required by `pr-title.yml`'s subject regex). `workflows` is in the `.versionrc` scope allowlist. |

## 4. Target structure

### 4.1 Resolved action SHAs

| Action | Version | Commit SHA | Source |
| --- | --- | --- | --- |
| `actions/checkout` | v6.0.2 | `de0fac2e4500dabe0009e67214ff5f5447ce83dd` | Reused from `ci.yml` (set in SMA-349) |
| `dtolnay/rust-toolchain` | master (no tagged releases) | `3c5f7ea28cd621ae0bf5283f0e981fb97b8a7af9` | Reused from `ci.yml` |
| `actions/create-github-app-token` | v3.2.0 | `bcd2ba49218906704ab6c1aa796996da409d3eb1` | `gh api repos/actions/create-github-app-token/git/ref/tags/v3.2.0` — lightweight tag, points directly at the commit |
| `release-plz/action` | v0.5.129 | `064f4d1e36c843611ddf013be726beaa4ad804db` | `gh api repos/release-plz/action/git/ref/tags/v0.5.129` returns an annotated tag at `4a08fbe6...`; dereferenced via `gh api repos/release-plz/action/git/tags/4a08fbe6...` |

### 4.2 Full target file

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

### 4.3 Behavior parity vs. the current two-job version

| Concern | Current (two jobs) | New (one job) | Same? |
| --- | --- | --- | --- |
| `release-pr` skipped if `release` fails | Yes, via `needs: release-plz-release` | Yes, default step-level skip-on-failure (a failed step short-circuits subsequent steps in the same job) | ✅ |
| `CARGO_REGISTRY_TOKEN` only visible to `release` | Job-scoped env on `release-plz-release` | Step-scoped `env:` on the `release` step only | ✅ |
| Token revoked when work is done | Twice (one Post step per job) | Once, by the single mint's Post step at job end | Improved |
| Re-checkout needed after `release` pushes a tag? | Two-job version did a redundant fresh checkout in job 2 | No re-checkout needed: `release-plz release` doesn't move `main`'s HEAD | Improved (less wasted work) |
| Concurrency / cancellation behavior | One group, `cancel-in-progress: false` | Unchanged | ✅ |
| Permissions surface | `contents: write`, `pull-requests: write` at workflow level | Unchanged | ✅ |

## 5. Verification path

The acceptance criterion ("a merge to `main` after this lands triggers exactly one release-plz workflow run that completes green") is **not testable on a feature branch** because the workflow is `push: [main]`-only. There is no `workflow_dispatch` and adding one is explicitly a non-goal.

Pre-merge verification (PR-time, local):

1. `cat .github/workflows/release-plz.yml` — YAML is syntactically valid (no tabs, consistent indent, all `uses:` SHAs are 40 hex chars).
2. `gh workflow view release-plz` (after pushing the branch) — GitHub parses the workflow file without error.
3. All four SHAs resolve via `gh api repos/<owner>/<repo>/commits/<sha>` — confirms we didn't fat-finger a SHA.
4. CI's `commits` job (convco) accepts the `ci(workflows): SMA-351 ...` commit subject — `workflows` is in the scope allowlist per memory.
5. CI's `pr-title` job accepts the squashed-PR title: subject starts with a lowercase verb after the `SMA-351 ` prefix.

Post-merge verification (on `main`):

6. Watch the first push-to-main after merge trigger exactly one `release-plz` workflow run (not two), and that run completes green.
7. Inspect the run's logs for: exactly one "Token revoked" line in the Post step (vs. the previous two), and no `HTTP 401` from any `release-plz/action` step.

If step 6 or 7 fails, the rollback is a one-commit revert that restores the prior two-job file — no secrets need rotating, no App settings need touching.

## 6. Risks

| Risk | Likelihood | Mitigation |
| --- | --- | --- |
| `release-plz release` succeeds-with-side-effects, then `release-pr` fails after | Low | The two operations are independent (`release` publishes crates + pushes tags; `release-pr` only opens/updates a PR). A `release-pr` failure leaves the published crates and pushed tags intact — same as today. Re-run the workflow manually if needed. |
| Annotated-tag dereferencing footgun | Low (mitigated) | `release-plz/action@v0.5.129` is an annotated tag (`type: "tag"`); using the tag's own SHA instead of its dereferenced commit SHA would mean GitHub Actions refuses to run the step. The spec lists the dereferenced commit SHA (`064f4d1e...`), not the tag SHA (`4a08fbe6...`). The implementation plan will re-confirm before writing. |
| `cargo publish` slower without cache | Negligible | Release-plz runs ~weekly. The added latency per run is in seconds, not minutes. If it ever matters, file a follow-up. |
| Race fix doesn't fix the underlying token-endpoint bug | N/A | We're not trying to fix GitHub's endpoint. We're removing the code path that triggers it. |

## 7. Out-of-scope follow-ups

- If `release-plz/action` ever publishes a v1, bump in a separate ticket (this is the standard "new major" dance — Dependabot can't auto-bump across majors).
- If publish latency becomes noticeable, add `Swatinem/rust-cache` (consciously deferred during brainstorming, 2026-05-25).
- If a future release introduces additional `release-plz/action` subcommands that should run in the same workflow, add them as further steps in this same single job — do not split back into multiple jobs.
