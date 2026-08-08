# SMA-487 — grant `pull-requests: read` in `ci.yml`

**Status:** approved
**Date:** 2026-08-08
**Linear:** [SMA-487](https://linear.app/smaschek/issue/SMA-487/ci-grant-pull-requests-read-to-sessions-its-paths-filter-in-ciyml)
**Branch:** `feature/sma-487-ci-grant-pull-requests-read-to-sessions-its-paths-filter-in`

## Problem

`.github/workflows/ci.yml` declares only `contents: read` at workflow level. Its
`sessions-it` job runs `dorny/paths-filter`, which on a `pull_request` event
resolves the changed-file list through the GitHub API call `pulls.listFiles` —
the call that `pull-requests: read` grants. Once a workflow declares *any*
`permissions:` block, every scope not listed is `none`, so the token `ci.yml`
hands its jobs has no `pull-requests` scope at all.

SMA-457 added that grant to `integration.yml`, whose two `paths-filter` steps
make the same call. `ci.yml` was never updated.

## This is a portability fix, not a live bug

`sessions-it` is not failing and has never failed for this reason. The
repository is **public**, and PR metadata on a public repo is readable without
the `pull-requests` scope, so the filter step has worked under `contents: read`
alone since SMA-330. CodeRabbit's original framing on PR #181 — that both filter
steps *can* fail — was too broad, and CodeRabbit agreed.

The reason to make the change anyway is that the workflow is currently correct
only *by accident of repository visibility*. `sessions-it` is a **required**
status check. If the repository were ever made private, its filter step would
start failing, and a required check that cannot pass blocks every PR. A
one-line, read-only grant makes the workflow correct on its own terms.

The change is therefore judged on whether it removes that latent coupling, not
on whether CI turns from red to green.

## Design

### 1. The grant (`.github/workflows/ci.yml`)

Add `pull-requests: read` to the existing top-level `permissions:` block,
directly beneath `contents: read`, carrying an explanatory comment:

```yaml
permissions:
  contents: read
  # For dorny/paths-filter in sessions-it, which on a pull_request resolves
  # changed files through `pulls.listFiles` (job log: "Fetching list of changed
  # files for PR#N from GitHub API"). On push it diffs locally and needs only
  # contents: read.
  #
  # Not load-bearing today — the repo is public, so PR metadata reads fine
  # without it and the filter has worked under contents: read alone since
  # SMA-330 — and that is exactly what makes it easy to delete by mistake.
  # Don't: sessions-it is a *required* check, so were the repo ever made
  # private the filter step would fail and block every PR. This keeps the
  # workflow correct on its own terms instead of by accident of repository
  # visibility.
  pull-requests: read
```

**Workflow-level, not job-level.** A job-level `permissions:` block *replaces*
the workflow-level one rather than merging with it, so scoping the grant to
`sessions-it` would mean repeating `contents: read` inside the job — a second
place to maintain, and a footgun the next time someone adds a scope at the top
and expects `sessions-it` to inherit it. Workflow-level also matches
`integration.yml`, the model this change is copying. The cost is that eight jobs
that never call the API inherit a read-only scope over data that is already
world-readable on a public repo; that blast radius is nil.

**The comment is adapted from `integration.yml`'s, not copied.** Two
`ci.yml`-specific facts have to survive in it, and neither is present in the
original:

- `integration.yml`'s jobs are signal-only. "If this breaks, every PR is
  blocked" is true only where the consumer is a required check.
- `ci.yml` runs on both `push` and `pull_request`. `paths-filter` only calls the
  API on the latter; on `push` it diffs locally against the before-SHA. Saying
  so pre-empts the reader who checks a push run's log, sees no API call, and
  concludes the scope is dead weight.

The comment exists because the ticket's own reasoning is self-undermining
otherwise: an accurate note that the line is not load-bearing is exactly what a
future permission-minimising reader needs in order to delete it. It has to state
the consequence, not just the fact.

### 2. Audit of the other workflows — negative result

The ticket asks whether any other workflow reads PR metadata and has the same
gap. Checked all ten workflows; **`ci.yml` is the only one.**

| Workflow | PR-metadata read | Verdict |
|---|---|---|
| `integration.yml` | two `paths-filter` steps | already granted (SMA-457) |
| `pr-title.yml` | `action-semantic-pull-request` reads the title via API | already granted |
| `ci.yml` (`commits` job) | `github.event.pull_request.base.sha` | **webhook payload, not API** — no scope needed |
| `pr-title.yml` (concurrency key) | `github.event.pull_request.number` | payload expression — no scope needed |
| `ci.yml`, `msrv.yml`, `integration.yml`, `release-plz.yml` | `arduino/setup-protoc` `repo-token` | release-asset download, covered by `contents: read` |
| `deny.yml`, `sbom.yml`, `msrv.yml`, `docs.yml` | `taiki-e/install-action` | release-asset download, covered by `contents: read` |
| `audit.yml` (`scheduled-audit`) | `rustsec/audit-check` files issues | `issues: write`, already declared |
| `release-plz.yml` | release-plz reads/writes PRs | `pull-requests: write`, already declared |
| `sbom.yml` | `softprops/action-gh-release` | `contents: write`, already declared |
| `docs.yml` | Pages deploy | job-level `pages: write` + `id-token: write` |
| `bench.yml` | none | `contents: read` is correct |

This produces no code change. It is recorded here and in the PR body so the
scope bullet is visibly discharged rather than silently dropped.

Note the distinction the table turns on, since it is the one a future reader is
most likely to get wrong: reading `github.event.pull_request.*` costs nothing,
because the webhook payload is already on the runner. Only an API call needs a
scope.

### 3. Doc-consistency fix (`CLAUDE.md`)

`CLAUDE.md:111` currently ends:

> Concurrency cancels in-flight PR runs but lets `main` pushes complete; both
> workflows declare `permissions: contents: read`.

This change falsifies it for `ci.yml`. The sentence is already loose: "both
workflows" reads as `ci.yml` + `msrv.yml` from when the CI section covered only
those two, and the section now also documents `integration.yml` (which has
carried `pull-requests: read` since SMA-457) and `pr-title.yml` (which has never
had `contents: read`).

Replace it with a sentence that states the actual permissions and why `ci.yml`
carries the extra scope, so the next reader finds the rationale in the guidance
file as well as the workflow.

This touches no workflow and so does not brush the ticket's "do not broaden any
other permission" boundary.

## Out of scope

- Broadening any other permission in any workflow. The repo's minimal-permission
  posture is deliberate — see `pr-title.yml`'s comments on keeping PR-controlled
  code off the runner under `pull_request_target`.
- Changing `scheduled-audit`'s `if:` guard. CLAUDE.md explicitly warns that a
  non-`schedule` event routes `rustsec/audit-check` to `reportCheck()`, which
  needs a `checks: write` the job does not grant and 403s.
- mdBook (`docs/book/`) and crate `README.md` updates. Conscious call, not a
  silent skip: this is pure-internal CI plumbing with no user-facing surface, no
  public API change, and no crate-roster change.

## Verification

There is no `actionlint` in this repo, so verification is by CI observation:

1. **The workflow parses.** GitHub rejects a malformed workflow at dispatch; if
   `ci.yml` runs at all, the YAML is valid.
2. **`sessions-it` runs and its filter step succeeds.** This is a real gate, not
   a hypothetical one: `.github/workflows/ci.yml` is itself listed in the
   `sessions` filter, so this PR matches its own filter, `steps.filter.outputs.sessions`
   is `true`, and the full live Postgres/Redis suite executes.
3. **The grant is actually in effect.** The run's "GITHUB_TOKEN Permissions" log
   block should list `PullRequests: read`.

Gate 2 is what distinguishes this from an unobserved change. Had the filter not
matched, `sessions-it` would have taken the `echo "No session-related changes"`
branch and reported green without ever invoking `paths-filter` — a pass that
proves nothing.

### What would falsify the change

If `sessions-it` fails at the `paths-filter` step on this PR, the grant is
wrong or misplaced. Rollback is reverting one line plus its comment; nothing
depends on it.

## Conventions

- Commits: the design doc lands as `docs(spec): SMA-487 …`; the workflow change
  and the `CLAUDE.md` correction land together as `ci(workflows): SMA-487 …`.
  They are one atomic unit — the sentence in `CLAUDE.md` is wrong *only*
  because of the workflow edit, so splitting them would leave either commit
  self-inconsistent. Both `workflows` and `spec` are in `.versionrc`'s
  `scopeRegex`; `claude` is too, if the correction ever needs to stand alone.
- PR title: `ci(workflows): SMA-487 grant pull-requests read for sessions-it's paths-filter`
  — full Conventional Commits prefix, lowercase subject after the ticket ID.
- Touches only `.github/`, `CLAUDE.md`, and `docs/`, so release-plz attributes no
  version bump to any crate.
