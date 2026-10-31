# Release & Stub-Ascend Reference

> **Scope:** the crate version lifecycle, the stub-ascend recipe, the two
> publish deadlocks it has caused (SMA-321, SMA-346), and the release-plz
> `git/refs` 403 with the release gap report (SMA-745).
> This runbook is **not** linked from the public mdBook — it lives standalone under
> `docs/runbooks/` to avoid linkcheck coupling. It holds the *rationale and incident
> history*; the operative rules stay in `CLAUDE.md`.

## Version lifecycle

**Per-crate version is the one exception**, with a two-state lifecycle:

1. **Stub state — `version = "0.0.0"` + `publish = false` in Cargo.toml + `release = false` block in `release-plz.toml`.** Every stub was pre-published once to crates.io at `0.0.0` during SMA-385 to claim the name and satisfy the facade's optional-dep resolver. After that pre-publish, cargo refuses to republish (the per-crate `publish = false`); release-plz ignores them entirely (the `release = false`).
2. **Released state — bumped to a real version (≥ `0.1.0`)** after the first real public-API ticket lands. The 4-step ascend recipe:
   - Bump `version = "0.0.0"` → `"0.1.0"` in the crate's `Cargo.toml`.
   - Remove `publish = false` from that `Cargo.toml`.
   - Remove the crate's `[[package]] … release = false` block from `release-plz.toml`.
   - Land as one `chore(release): SMA-### lift stage-1 gates for <crate>` commit on the feature branch alongside the implementation. release-plz handles the first crates.io publish on CI.

   The 4-step recipe applies to **stubs ascending from `0.0.0`**. The ten already-released crates (`-core`, facade, `-macros`, `-providers-openai`, `-providers-anthropic`, `-providers-bedrock`, `-sessions-sqlite`, `-runtime-tokio`, `-mcp`, `-tools`) ship through release-plz's normal flow — no manual ritual needed for their future bumps. The historical chain of `chore(release): … escape release-plz 0.0.0 trap …` commits in the git log (SMA-317/347/350/372/382) is pre-Stage-1 archaeology and won't recur.

## Caveat 1 — same-PR core API needs a core bump (SMA-321)

**Caveat — when the ascending crate uses `paigasus-helikon-core` API added in the *same* PR, bump `core` too** (a 5th step). `cargo publish --verify` builds the ascending crate's tarball against the **registry** core (the `path` is stripped at publish), so if crates.io `core` lacks the new API the publish fails with `failed to verify package tarball`, and release-plz's combined job (SMA-351) aborts before its release-PR step — a deadlock, since `core` never gets its auto bump (a squashed `feat(<ascending-crate>)` commit attributes nothing to `core`). Fix: in the same PR, also bump `paigasus-helikon-core` (patch for additive/non-breaking-behind-`#[non_exhaustive]`, e.g. `0.2.0` → `0.2.1`) and its `[workspace.dependencies]` pin + CHANGELOG. release-plz then publishes `core` first, then the ascending crate verifies against the fresh `core` (dependency-ordered publish). Diagnosed in SMA-321: PR #45's release failed against the stale `core` `0.2.0`; PR #46 (`chore(release)` bumping `core` to `0.2.1`) cleared it.

## Caveat 2 — the manual core bump defeats the facade cascade (SMA-346)

**Second-order caveat — the manual core bump silently defeats `dependencies_update`, so the facade drifts.** `release-plz.toml` sets `dependencies_update = true`, which is *supposed* to cascade: when a sibling's version changes, release-plz bumps the facade's `[workspace.dependencies]` pin and gives the facade a patch bump. But that cascade only runs when **release-plz itself performs the sibling bump**. The same-PR manual bump above means the sibling version is already at target when the PR merges, so release-plz just publishes it and never runs the dependent-bump step — the facade (`paigasus-helikon`) is left untouched and stops tracking. Consequence: the facade stays at its old version with stale published dep reqs (e.g. after SMA-346, facade `0.2.0` still requested `paigasus-helikon-runtime-tokio = ^0.0.0`, so the new runner-boundary surface was unreachable through the facade's `runtime-tokio` feature). **Fix: in any PR that uses the same-PR manual bump, ALSO bump the facade** (patch: `version` in `crates/paigasus-helikon/Cargo.toml` + its `[workspace.dependencies]` self-pin + CHANGELOG) so it republishes with current sibling reqs. Diagnosed after SMA-346: PRs #48/#49 shipped `core 0.2.3` + `runtime-tokio 0.1.1` but left facade `0.2.0`; PR #50 (`chore(release)` bumping facade to `0.2.1`) cleared it. NB: feature branches must match the `branch-names` ruleset (`feature/**` or `hotfix/**`); a `chore/**` branch is rejected at push with `GH013 … creations being restricted`.

## A red release-plz run with 403 on `git/refs` (SMA-745)

**Symptom.** A `release-plz.yml` run fails with `403 Forbidden` and an empty body on `POST /repos/<owner>/<repo>/git/refs`. The failed call can create a release branch or a tag. On 2026-10-30, two runs failed this way. One run published `paigasus-helikon-evals` 0.1.17 to crates.io and then failed to create the tag. The crate had no tag and no GitHub release.

**Cause.** GitHub refuses to create a ref at a commit that is behind workflow-file changes on `main`. The token needs the `workflow` permission for this. In both runs, a PR that changes `.github/workflows/*` merged while the run was in progress. We tested this on 2026-10-31 with a token that had no `workflow` scope. The same call, ref name and token gave different results:

| Target commit | Position | Result |
| -- | -- | -- |
| A commit behind a workflow change on `main` | behind `main` | `404 Not Found`, no ref |
| The tip of `main` | tip of `main` | ref created |

We measured only the 404 with the OAuth token. The App token got 403 in the incident runs. We think that both are refusals of the same check. We did **not** test that an App token with `workflows: write` succeeds, because the App private key exists only in Actions secrets. The first `release-plz` run that overlaps a workflow merge is the first real evidence. The gap report (below) catches a failure if the fix is not enough.

**Fix.** The `actions/create-github-app-token` step in `release-plz.yml` requests `permission-workflows: write`. The paigasusbot App (ID 3742291) has `Workflows: Read and write`. If the installation loses this permission, the token step fails. Then restore the permission in the App settings and accept it on the installation.

**The `release` environment.** The extra power of the token has a risk. A leaked token can push a branch that has a new `on: push` workflow. Such a workflow can read repository secrets. The `release-plz` job therefore sets `environment: release`. This environment allows only the `main` branch. It has no required reviewers, so the release stays automatic. `scripts/apply-repo-config.sh` creates the environment. Set `RELEASE_PLZ_APP_CLIENT_ID`, `RELEASE_PLZ_APP_PRIVATE_KEY` and `CARGO_REGISTRY_TOKEN` as **environment** secrets of `release`, by hand. Do not set them as repository secrets. A repository-level copy would be readable from any branch.

**The gap report.** Each `release-plz` run ends with the step "report release gaps". It runs `scripts/check-release-gaps.sh`. For each publishable crate, it checks the version at the run commit. It reports a gap when crates.io lists the version but the tag `<package>-v<version>` or the non-draft GitHub release is missing. The script only reports. It does not repair. The step has three results:

| Result | Exit code | Meaning |
| -- | -- | -- |
| No gaps | 0 | Every published version has a tag and a release. |
| Gap | 1 | At least one version is published without a tag or a release. The step is red, also when release-plz was green. |
| Could not check | 2 | An error or an unexpected response stopped a check. On a green release step, this is a `::warning::` and the step exits 0. On a red release step, the step stays red. |

"Could not check" is never "no gaps". A network error, an API error, an empty package list and a `git_tag_name`, `git_tag_enable` or `git_release_enable` key in `release-plz.toml` all give "could not check". The report lists the reason for each item. To run the report locally, use `GH_TOKEN=$(gh auth token) bash scripts/check-release-gaps.sh`.

**Recovery.** The step summary shows the commands for each gap. In general form, with `<tag>` as `<package>-v<version>`:

If the tag already exists and only the release is missing, skip steps 2 to 4.

1. Give your `gh` token the `workflow` scope: `gh auth refresh -s workflow`.
2. Look in the failed run log for the annotated tag object that release-plz made. Check it: `gh api repos/<owner>/<repo>/git/tags/<tag-sha> --jq '{tag, sha: .object.sha}'`. The tag name and the object SHA must match the release commit.
3. Create the ref: `gh api -X POST repos/<owner>/<repo>/git/refs -f ref=refs/tags/<tag> -f sha=<tag-sha>`.
4. If the call fails because the tag object does not exist, make a new annotated tag object at the release commit with `POST /git/tags`. Then repeat step 3 with its SHA. The release commit is the commit that bumped the version. The gap can come from an earlier run, so the commit of the current run can be wrong.
5. GitHub returns 404 for a draft release. Look for a draft with this tag in `gh release list`. If one exists, publish it with `gh release edit <tag> --draft=false --latest=false`. Without `--latest=false`, GitHub marks the published release as "Latest". Do not make a second release.
6. Else create the release: `gh release create <tag> --verify-tag --latest=false --title <tag> --notes-file <file>`. Put the CHANGELOG section of that version in the file.
7. Remove the extra scope: `gh auth refresh -r workflow`.
8. Run the gap report again. It must say "no gaps".

**Rule.** A red `release-plz` run can still have published crates. Do not assume that nothing shipped. Read the gap report first. Then check crates.io, the tags and the releases.

**Rollout order for the SMA-745 change.** Run `sh scripts/apply-repo-config.sh` and set the three environment secrets before the merge. If the `release` environment does not exist when the workflow first runs, GitHub creates it without a branch policy. Thus the script must run before the merge. Merge only when no `release-plz` run is queued or in progress. After the first run on `main`, run `gh secret list --env release`. Make sure that it shows `RELEASE_PLZ_APP_CLIENT_ID`, `RELEASE_PLZ_APP_PRIVATE_KEY` and `CARGO_REGISTRY_TOKEN`. Then delete the three repository-level secrets. For a rollback, revert the workflow change. Restore the repository-level secrets before you do this.

## Implementation status snapshot

**Implementation status** (as of 2026-07-06): every crate in the workspace carries a real implementation and publishes to crates.io — the original ten (`paigasus-helikon-core`, `paigasus-helikon`, `paigasus-helikon-macros`, `paigasus-helikon-providers-openai`, `paigasus-helikon-providers-anthropic`, `paigasus-helikon-providers-bedrock` (SMA-329), `paigasus-helikon-sessions-sqlite`, `paigasus-helikon-runtime-tokio` (ascended from stub in SMA-346), `paigasus-helikon-mcp` (SMA-327), and `paigasus-helikon-tools` (SMA-328)) plus `paigasus-helikon-providers-gemini` (SMA-449), `paigasus-helikon-sessions-postgres`/`-sessions-redis` (SMA-330), `paigasus-helikon-runtime-axum` (SMA-331), and the last four ascends — `paigasus-helikon-runtime-temporal`, `paigasus-helikon-runtime-agentcore` (both SMA-332), `paigasus-helikon-evals`, and `paigasus-helikon-cli` (both SMA-333). The workspace was first published to crates.io in SMA-385; zero name-claim stubs remain. `paigasus-helikon-cli` publishes as a **binary** crate (`0.1.0`) — its lib target is internal (`missing_docs` opted out) and carries no stability guarantee, publishing only so `cargo install paigasus-helikon-cli` resolves. `paigasus-helikon-sessions-testkit` is the sole non-published crate: an internal `Session` conformance test harness kept at `0.0.0` with `publish = false` by design, not a stub awaiting an ascend.
