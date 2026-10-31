# SMA-745 — release-plz `POST /git/refs` 403 after a workflow-changing merge

Linear: [SMA-745](https://linear.app/smaschek/issue/SMA-745). Branch:
`feature/sma-745-release-plz-post-gitrefs-returns-403-when-a-workflow`.

## Intent

A `release-plz` run must not leave a crate published on crates.io without its
git tag and GitHub release. Sven said that the result to avoid is "a release that
one must repair by hand". A larger permission for the paigasusbot App is
acceptable to get this, but the extra risk that it brings must be contained in
the same change.

If a release still leaves a gap for a different cause, the `release-plz` run
must report the gap and be red. Nobody must find it later by accident.

## What happened

On 2026-10-30, two `release-plz.yml` runs failed with `403 Forbidden` and an
empty body on `POST /repos/SMK1085/paigasus-helikon/git/refs`:

| Run | Commit | Failed call | Effect |
| -- | -- | -- | -- |
| 41119756553 | `91a6ae65` (PR 314) | create branch `release-plz-2026-10-30T20-04-51Z` | none; the next run opened the release PR |
| 41137079475 | `edc06f74` (PR 317) | create tag `paigasus-helikon-evals-v0.1.17` | evals 0.1.17 is on crates.io with **no tag and no GitHub release** |

In both runs, a PR that changes `.github/workflows/*` (PR 315, PR 318) merged
while the run was in progress.

## Cause (tested on 2026-10-31)

GitHub refuses to create a ref at a commit that is behind workflow-file changes
on `main`, unless the token has the `workflow` permission. We tested this with
Sven's `gh` OAuth token (scopes `gist`, `read:org`, `repo`; no `workflow`):

| Target commit | Position | Result |
| -- | -- | -- |
| `edc06f74` | behind `main` by PR 318 (workflow change) | `404 Not Found`, no ref |
| `b69fbf13` | tip of `main` | ref created (deleted again immediately) |

The token, the ref name (`refs/heads/release-plz-sma745-repro`) and the call
were the same. Only the commit was different. An OAuth token gets 404 and the App
token got 403. Both are refusals of the same check.

Not tested: that an App token **with** `workflows: write` succeeds. We cannot
make an App token locally (the private key is only in Actions secrets). The
first release-plz run with a workflow merge during the run is the first real
evidence. The gap report (below) catches a failure if the fix is not enough.

Other facts checked:

- The permission covers every ref write by release-plz: the tag ref in
  `release`, and the branch create and force-update in `release-pr`. A GitHub
  release on an existing tag, a tag object, and a commit made through the API
  create no ref.
- Token expiry (1 hour) is not a second cause. The longest of the last 30
  release-plz runs took 12 minutes.
- release-plz makes **annotated** tags (for example, the ref for
  `paigasus-helikon-evals-v0.1.16` points to an object of type `tag`). It makes
  the tag object first, then the ref. In run 41137079475 the tag object
  `93f06c5ad30ce04fd74718430e28381531bb1b0a` was made (tag
  `paigasus-helikon-evals-v0.1.17`, object `edc06f74…`, tagger
  `paigasusbot[bot]`); only the ref failed.
- `release-plz.toml` sets no `git_tag_name`, so the tag format is the default
  `<package>-v<version>`.

## Goals

1. The paigasusbot installation token can create refs behind workflow changes.
2. The extra power of that token cannot reach the release secrets from a
   branch other than `main`.
3. Each `release-plz` run checks for published versions without a tag or a
   GitHub release, reports each gap with recovery commands, and is red when a
   gap exists.
4. The runbook describes this failure, the evidence, the report and the
   recovery.
5. `paigasus-helikon-evals` 0.1.17 gets its tag and its GitHub release.

## Non-goals

- Automatic repair of a gap. The report only reports.
- A change to the `concurrency` group. The runs did not overlap; it works.
- Rules about the merge order of PRs, except during the rollout of this PR.
- A check of older versions. Only the version at the run's commit is checked.

## Anti-goals

- A gap report that says "no gaps" when it could not check. A network error, an
  API error, an empty package list or an unexpected response must show as
  "could not check", never as success.
- A report step that hides the original failure. The job stays red when
  release-plz failed.
- A gap report that gives wrong recovery commands because an auth or rate-limit
  error looked like "missing".

## Constraints

- Sven added `Workflows: Read and write` to the paigasusbot App (ID 3742291) on
  2026-10-31, and confirmed that the installation on `SMK1085/paigasus-helikon`
  accepted it.
- No PR runs `release-plz.yml`. The first real test of the workflow change is
  the first push to `main` after the merge. Static guards and a self-test in the
  required `fmt` job protect what can be checked before that.
- crates.io rejects the default curl User-Agent with 403. Every crates.io
  request sets a custom User-Agent. The crates.io crawler policy asks for a
  maximum of 1 request per second.
- Release infrastructure commits use `chore(...)` or `docs(...)`, never
  `feat`/`fix`. The scope `workflows` is valid in `.versionrc` and in the
  `pr-title.yml` allowlist on `main`. PR title:
  `chore(workflows): SMA-745 <lowercase subject>`.
- This PR changes no packaged crate file, so release-plz makes no version bump
  for it.
- Actions stay SHA-pinned. This change adds no new action.

## Design

### 1. Token permission (`.github/workflows/release-plz.yml`)

Add `permission-workflows: write` to the `actions/create-github-app-token`
step, next to `permission-contents` and `permission-pull-requests`. Add a short
comment: refs at a commit behind a workflow change need it (SMA-745).

The workflow-level `permissions:` block is for `GITHUB_TOKEN` and stays as it
is.

### 2. Release secrets in a `release` environment (Sven's decision)

Risk: App 3742291 is a bypass actor on the `branch-names` ruleset. With
`contents: write` and `workflows: write`, a leaked installation token can push a
branch that contains a new `on: push` workflow. Today, that workflow can read
every repository secret, including `RELEASE_PLZ_APP_PRIVATE_KEY` and
`CARGO_REGISTRY_TOKEN`. The token is in the env of the release-plz step, so a
build script that runs during `cargo publish --verify` can probably read it.

Mitigation:

- A GitHub environment `release` with a deployment branch policy that allows
  only the `main` branch (custom branch policy, one entry: `main`). No required
  reviewers, because the release must stay automatic.
- The three secrets `RELEASE_PLZ_APP_CLIENT_ID`, `RELEASE_PLZ_APP_PRIVATE_KEY`
  and `CARGO_REGISTRY_TOKEN` move from repository secrets to environment
  secrets of `release`. They are used only in `release-plz.yml`.
- The `release-plz` job sets `environment: release`.
- The environment and its branch policy are configuration as code: extend
  `scripts/apply-repo-config.sh` to create or update the environment
  idempotently. Document it where `CONTRIBUTING.md` describes "Repo
  configuration".
- A workflow on another branch then cannot read these secrets. The job on `main`
  reads the environment secrets, which override repository secrets of the same
  name.

Secret values cannot be read back from GitHub, so Sven sets the environment
secrets by hand. Recommended: use the move to rotate. Make a new App private key
and a new crates.io token (publish-update scope), set them on the environment,
and revoke the old ones after the first good run.

### 3. Gap report on each run

A new script `scripts/check-release-gaps.sh`, and a new last step in
`release-plz.yml`:

```yaml
- name: report release gaps
  if: ${{ always() && (steps.release.outcome == 'success' || steps.release.outcome == 'failure' || steps.release.outcome == 'cancelled') }}
  timeout-minutes: 5
  env:
    GH_TOKEN: ${{ github.token }}
    RELEASE_STEP_OUTCOME: ${{ steps.release.outcome }}
  run: bash scripts/check-release-gaps.sh
```

The `release` command step gets `id: release`. The report runs when the release
step ran (success, failure or cancelled). It does not run when the release step
was skipped, because then nothing was published. (Builder: a job-level
cancellation can stop `always()` steps too; this is accepted.)

The report uses `github.token`, not the App token. The App token can be the
cause of the failure, and it can be expired. The workflow-level
`contents: write` gives read access to refs and releases, and `github.token`
is valid for the whole job.

**Package list.** The script reads the versions **before** release-plz runs, so
that `release-pr` cannot change what it reads: a step before the release step
writes `cargo metadata --no-deps --format-version 1` to a file, and the script
reads it from a path in an env var (`RELEASE_GAPS_METADATA`). If the env var is
not set (local use), the script runs `cargo metadata` itself. It keeps the
packages whose `publish` field is not `[]`. If the list is empty, or the
metadata cannot be read or parsed, the result is "could not check".

**Tag format guard.** If `release-plz.toml` contains `git_tag_name`,
`git_tag_enable` or `git_release_enable`, the result is "could not check" with a
clear message. The script assumes `<package>-v<version>` and must not guess
another format.

**Per package**, with a 1-second pause between crates.io requests and a time
limit on every call (`curl --max-time 20`, `timeout 30` around `gh`):

- crates.io: `GET https://crates.io/api/v1/crates/<name>` with a custom
  User-Agent. Require 200 and read `versions[].num`. The version is in the list
  → published. Not in the list → not published, no gap. A 404 for the crate
  itself → "could not check" (every publishable crate here is already on
  crates.io; a 404 more probably means a wrong URL or an API change). Any other
  result → "could not check".
- tag: `GET /repos/{repo}/git/ref/tags/<name>-v<version>` (singular `ref`,
  exact match, not a prefix match). 200 → present. 404 → missing. Any other
  status → "could not check".
- release: `GET /repos/{repo}/releases/tags/<name>-v<version>`. 200 and not a
  draft → present. 200 and a draft, or 404 → missing. Any other status →
  "could not check".

The script reads HTTP status codes explicitly (for example `gh api --include`
or `curl -w '%{http_code}'`). A non-zero exit of `gh` alone is never read as
"missing".

**Output.**

- For each gap: a line in `$GITHUB_STEP_SUMMARY` (when set) with the crate, the
  version, what is missing, and the recovery commands. Without
  `$GITHUB_STEP_SUMMARY` (local use), the same text goes to stdout.
- One summary `::error::` annotation for all gaps (GitHub shows a maximum of 10
  annotations per step, and multi-line messages need `%0A`).
- Each "could not check" item is listed with its reason.

**Recovery text** for a missing tag: first look for the tag object from the
failed run in the run log; if none is known, make a new annotated tag that
points at the run commit (`$GITHUB_SHA` in CI, `HEAD` locally). Then create the
ref. For a missing release: `gh release create <tag> --verify-tag
--latest=false` with the version's CHANGELOG section as notes. The text states
that the token needs the `workflow` scope.

**Exit codes and job result.**

| Result | Exit code | On a red release step | On a green release step |
| -- | -- | -- | -- |
| no gaps | 0 | job stays red (release failed) | green |
| at least one gap (with or without "could not check") | 1 | red | **red** |
| "could not check", no gap | 2 | red | `::warning::`, step exits 0 |

The script gets the release-step outcome from `RELEASE_STEP_OUTCOME`. Only the
"could not check on a green run" case changes the exit code; the script maps 2
to a warning and exit 0 there. A green run with a gap is red, because a gap is a
broken release.

Locally: `GH_TOKEN=$(gh auth token) bash scripts/check-release-gaps.sh`. Before
the evals recovery, it must report exactly one gap:
`paigasus-helikon-evals` 0.1.17, tag and release missing.

### 4. Self-test and static guards in CI

The report runs on each release-plz run, but only on `main`. Add
`scripts/check-release-gaps-selftest.sh`. It runs the script with fake `curl`
and `gh` commands on `PATH` and a fixture metadata JSON through
`RELEASE_GAPS_METADATA` (no live cargo, network or GitHub). It checks the exit
code **and** the output text (crate name, version, "could not check" reason).
Cases:

- all versions tagged and released → 0
- published, no tag → 1, crate and version in the output
- published, tag present, no release → 1
- published, release is a draft → 1
- version not in the crates.io list → 0
- crates.io returns 404 for the crate → 2
- crates.io returns 500, or no response → 2
- crates.io returns 429 → 2
- tag call returns 500 → 2; release call returns 401 → 2
- one gap plus one "could not check" → 1, both shown
- only a longer tag with the same prefix exists (`…-v0.1.17` vs
  `…-v0.1.170`) → 1
- metadata cannot be read → 2; zero publishable packages → 2
- `release-plz.toml` with `git_tag_name` → 2
- `RELEASE_STEP_OUTCOME=success` and "could not check" only → 0 with a warning

Static guards in the same self-test (no PR runs `release-plz.yml`, so these are
the only pre-merge protection for it): `release-plz.yml` contains
`permission-workflows: write`, `environment: release`, `id: release`, and a step
that runs `scripts/check-release-gaps.sh`.

Run the self-test as a step in the `fmt` job of `ci.yml`, next to
`check-cargo-profile-env-sync-selftest.sh`. `fmt` is a required check.

Add the new self-test command to the lists of local CI gates in `CLAUDE.md`
("To reproduce every CI gate locally") and `CONTRIBUTING.md`, and to the `fmt`
job description in `docs/runbooks/ci-architecture.md`.

### 5. Runbook (`docs/runbooks/release-ascend.md`)

Add a section "A red release-plz run with 403 on `git/refs` (SMA-745)":

- the cause and the test evidence (short form of "Cause" above)
- the fix (the `workflows` permission) and what to do if the installation loses
  it (the token step fails)
- the `release` environment: what it protects, and that secrets are set there,
  not as repository secrets
- how to read the gap report and its three results
- the recovery commands
- the general rule: a red release-plz run can still have published crates; read
  the gap report, do not assume that nothing shipped.

The section is in the markdown-lint gate.

### 6. Rollout (order matters)

1. Done: the App has `Workflows: Read and write`, and the installation accepted
   it.
2. Before the merge: Sven runs `sh scripts/apply-repo-config.sh` from this
   branch (creates the `release` environment with the `main`-only branch
   policy) and sets the three environment secrets (new values recommended).
3. Merge only when no `release-plz` run is queued or in progress. This PR
   changes workflow files, so a run in progress at merge time can hit the
   SMA-745 failure one more time.
4. After the merge, read the first run: the token step succeeds, the job shows
   a deployment to `release`, and the gap report runs. The evals 0.1.17 gap may
   still show if step 7 is not done yet; that is expected and correct.
5. Delete the three repository-level secrets. Revoke the old App private key and
   the old crates.io token if new ones were made.
6. Rollback: revert the workflow change. The App permission and the
   environment can stay. Before a rollback, the repository-level secrets must
   exist again.
7. evals 0.1.17 recovery (Sven runs it, see below). It does not depend on the
   merge.

### 7. Recovery of evals 0.1.17 (Sven runs it)

The auto-mode classifier blocks ref creation by the agent, and Sven's token needs
the `workflow` scope first. The builder extracts the 0.1.17 section of
`crates/paigasus-helikon-evals/CHANGELOG.md` to a file and gives Sven these
commands with the real path:

```bash
gh auth refresh -s workflow
# Check that the tag object still exists and is the right one:
gh api repos/SMK1085/paigasus-helikon/git/tags/93f06c5ad30ce04fd74718430e28381531bb1b0a \
  --jq '{tag, sha: .object.sha}'
# expect: tag paigasus-helikon-evals-v0.1.17, sha edc06f74bcf537d6c7a49ea60e696d3b45c4dfa7
gh api -X POST repos/SMK1085/paigasus-helikon/git/refs \
  -f ref=refs/tags/paigasus-helikon-evals-v0.1.17 \
  -f sha=93f06c5ad30ce04fd74718430e28381531bb1b0a
gh release create paigasus-helikon-evals-v0.1.17 --verify-tag --latest=false \
  --title paigasus-helikon-evals-v0.1.17 --notes-file <path>
gh auth refresh -r workflow
```

If the ref call returns 422 (GitHub removed the unreferenced tag object), the
fallback is a new annotated tag object at `edc06f74…` through
`POST /git/tags`, then the ref. After the recovery, the local gap report must
say "no gaps".

## Decisions and who made them

- **The fix is the permission** — Sven chose this direction ("a release that one
  must repair by hand" is the result to avoid) and added the permission.
- **Move the release secrets into a `main`-only environment, in this PR** —
  Sven, after the challenge showed the security cost.
- **Report on every run, red on a gap also when release-plz is green** — Sven
  first said "a red run of release-plz should report it", then agreed to run it
  on every run.
- **Report only, no auto-repair** — my call. An automatic repair needs the same
  permission, so it adds no protection against this cause.
- **Only the current version per crate** — my call. release-plz publishes the
  version at the run's commit.
- **The report uses `github.token`** — my call, from the challenge.
- **Versions read before release-plz runs** — my call, so that `release-pr`
  cannot change them.
- **No mdBook or crate README edits** — conscious call. This is release plumbing,
  not a user-facing change.

## Left to the builder

- Exact shell structure of the script and the fake-command mechanism.
- The exact text of the summary and the annotation.
- How `apply-repo-config.sh` creates the environment (the environments API with
  `deployment_branch_policy.custom_branch_policies = true`, then a branch policy
  for `main`), as long as re-running it converges.
