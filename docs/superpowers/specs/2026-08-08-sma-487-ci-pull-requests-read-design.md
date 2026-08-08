# SMA-487 — grant `pull-requests: read` in `ci.yml`

**Status:** approved
**Date:** 2026-08-08
**Linear:** [SMA-487](https://linear.app/smaschek/issue/SMA-487/ci-grant-pull-requests-read-to-sessions-its-paths-filter-in-ciyml)
**Branch:** `feature/sma-487-ci-grant-pull-requests-read-to-sessions-its-paths-filter-in`

## Problem

`.github/workflows/ci.yml` declares only `contents: read` at workflow level. Its
`sessions-it` job runs `dorny/paths-filter`, which on a `pull_request` resolves
the changed-file list through `pulls.listFiles` — the API call that
`pull-requests: read` grants. Once a workflow declares *any* `permissions:`
block, every scope not listed is `none`, so the token `ci.yml` hands its jobs
carries no `pull-requests` scope at all.

SMA-457 added that grant to `integration.yml`, whose two `paths-filter` steps
make the same call. `ci.yml` was never updated.

**The API call is confirmed, not inferred.** From the `sessions-it` job log of
run `31216942714` (PR #181):

```
##[group]GITHUB_TOKEN Permissions
Contents: read
##[group]Fetching list of changed files for PR#181 from GitHub API
Invoking listFiles(pull_number: 181, per_page: 100)
Detected 12 changed files
```

That excerpt also captures the *before* state this change is measured against:
the permissions block lists `Contents: read` and nothing else.

## Justification: what this does and does not fix

`sessions-it` is not failing and never has failed for this reason. The repo is
**public**, so `pulls.listFiles` succeeds without the scope, and the filter has
worked under `contents: read` alone since SMA-330. CodeRabbit's original framing
on PR #181 — that both filter steps *can* fail — was too broad, and CodeRabbit
agreed.

The change rests on the same argument `integration.yml:47-49` already uses: it
is what the action documents as required, and it makes the `pull_request` path
correct independent of repository visibility. `sessions-it` is a **required**
check, so the consequence of that latent coupling is worse here than in
`integration.yml`, whose jobs are signal-only.

**It does not make `ci.yml` private-repo-safe, and the spec should not claim it
does.** On `push`, `paths-filter` does not diff locally. `ci.yml:183-185`
checks out at `fetch-depth: 1`, so the before-SHA is absent and the action runs
`git fetch --depth=1 --no-tags origin <before-sha>` (confirmed in run
`31221956714`). Because that checkout sets `persist-credentials: false`, the
fetch is unauthenticated — on a private repo it would fail, and no token scope
fixes it. That is a checkout-credentials problem, not a permissions problem, and
it is out of scope here.

So the honest claim is narrow: this closes the `pull_request` path's dependency
on repository visibility. It is a correctness fix, not a private-repo migration.

## Design

### 1. The grant (`.github/workflows/ci.yml`)

Add `pull-requests: read` to the existing top-level `permissions:` block:

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

**The imperative leads.** An earlier draft opened with "not load-bearing today"
and buried the rebuttal in sentence three — which hands a permission-minimising
reader (human or agent) the deletion argument in the first clause and asks them
to keep reading for the reason not to. Since the ticket's entire purpose is that
this comment survive such a reader, the order is inverted: requirement and
prohibition first, then the honest explanation of *why it looks removable*.

Seven comment lines, matching `integration.yml:44-50`'s precedent. The rationale
lives here and only here; CLAUDE.md points at it rather than restating it.

**Workflow-level, not job-level.** A job-level `permissions:` block *replaces*
the workflow-level one rather than merging — proved in-repo by `docs.yml:45-47`,
where `book-deploy` declares `pages: write` + `id-token: write` and thereby has
no `contents` scope at all despite `docs.yml:8-9`. Scoping to `sessions-it`
would mean repeating `contents: read` inside the job, and it diverges from
`integration.yml`, the model being copied.

The real cost is not the seven jobs that inherit an unused scope; it is that
workflow-level is the wrong default *if someone later needs a write scope for
one job*, because they would grant it to all eight — including `test`, which
compiles and runs PR-authored `build.rs` and test code. That risk is nil for
this change specifically: the scope is read-only, over data already
world-readable on a public repo, and `ci.yml:183-185` sets
`persist-credentials: false` so no token reaches the working tree. If a future
ticket needs a *write* scope in `ci.yml`, it should go job-level.

Fork PRs and Dependabot need no special handling: `ci.yml` triggers on
`pull_request`, not `pull_request_target`, so those runs get a read-only token
either way. Dependabot PRs already exercise this filter today — `Cargo.lock` is
in the `sessions` list at `ci.yml:196` — which is further evidence the grant is
not load-bearing. `release-plz.yml` is untouched and uses an App token, not
`GITHUB_TOKEN`.

### 2. Audit of the other workflows — negative result

The ticket asks whether any other workflow reads PR metadata and has the same
gap. All ten checked; **`ci.yml` is the only one.**

| Workflow | PR-metadata read | Verdict |
|---|---|---|
| `integration.yml` | two `paths-filter` steps | already granted (SMA-457) |
| `pr-title.yml` | `action-semantic-pull-request` reads the title via API | already granted (`:16`) |
| `ci.yml` (`commits` job) | `github.event.pull_request.base.sha` | **webhook payload, not API** — no scope needed |
| `pr-title.yml` (concurrency key) | `github.event.pull_request.number` | payload expression — no scope needed |
| `arduino/setup-protoc`, `taiki-e/install-action` (5 workflows) | release lookups against `protocolbuffers/protobuf` etc. | **cross-repo** — this workflow's `permissions:` block scopes `GITHUB_TOKEN` against *this* repo only and grants nothing there; the calls work because those repos are public, and `repo-token` only raises the anonymous rate limit |
| `audit.yml` (`scheduled-audit`) | `rustsec/audit-check` files issues | `issues: write`, already declared |
| `release-plz.yml` | release-plz reads/writes PRs | **App token, not `GITHUB_TOKEN`** (`:51`, `:58` pass `steps.app-token.outputs.token`); the workflow-level `pull-requests: write` at `:13` is not what authorises it |
| `sbom.yml` | `softprops/action-gh-release` | `contents: write` (`:13`), already declared |
| `docs.yml` | Pages deploy | job-level `pages: write` + `id-token: write` |
| `bench.yml`, `deny.yml`, `msrv.yml` | none | `contents: read` is correct |

No code change follows. It is recorded here and in the PR body so the scope
bullet is visibly discharged rather than silently dropped.

Two distinctions a future reader is likely to get wrong, and the reason the rows
above spell out mechanisms rather than just verdicts: reading
`github.event.pull_request.*` costs nothing because the payload is already on
the runner — only an API call needs a scope; and a `permissions:` block scopes
the token against *this* repository only, so it is never what makes a cross-repo
call succeed.

**Mechanical enforcement considered and rejected.** The repo has two stronger
patterns for "these must agree and nothing would notice if they drift":
`keep-in-sync-with:` markers (`pr-title.yml:29`, `:42`) and a hard script gate
(`scripts/check-advisory-ignore-sync.sh`, run by `deny.yml`). A ~5-line script
asserting that every workflow containing `dorny/paths-filter` also declares
`pull-requests: read` would enforce this invariant properly. It is rejected as
disproportionate: the invariant spans two workflows and one action, a new script
plus job wiring is a larger permanent surface than the thing it guards, and the
ticket scopes this to a one-line grant. Revisit if a third `paths-filter`
consumer appears.

### 3. Doc-consistency fix (`CLAUDE.md:111`)

The sentence currently ends:

> Concurrency cancels in-flight PR runs but lets `main` pushes complete; both
> workflows declare `permissions: contents: read`.

This change falsifies it. It is already loose — "both workflows" reads as
`ci.yml` + `msrv.yml` from when the section covered only those two, and the
section now also documents `integration.yml` (which has carried
`pull-requests: read` since SMA-457) and `pr-title.yml` (which has never had
`contents: read`). Replace with:

> Concurrency cancels in-flight PR runs but lets `main` pushes complete.
> `ci.yml` declares `contents: read` plus `pull-requests: read` — the latter for
> `sessions-it`'s `dorny/paths-filter`, which calls `pulls.listFiles` on PR
> events (SMA-487); the workflow comment carries the rationale and the reason it
> must not be minimised away.

Deliberately **not** a per-workflow permissions inventory. An inventory would be
a new drift surface in the file the repo has already had to run two one-time
catch-ups against (SMA-423, SMA-424), and would need updating on every future
permissions edit. This states one fact and points at the canonical rationale.

This touches no workflow, so it does not brush the ticket's "do not broaden any
other permission" boundary.

## Out of scope

- Broadening any other permission in any workflow. The repo's minimal-permission
  posture is deliberate — see `pr-title.yml`'s comments on keeping PR-controlled
  code off the runner under `pull_request_target`.
- The `push`-path fetch-credentials gap described above. Different mechanism
  (checkout credentials, not token scopes), and fixing it would mean relaxing
  `persist-credentials: false` — a security regression to solve a problem that
  only exists in a hypothetical.
- mdBook (`docs/book/`) and crate `README.md`. Conscious call per CLAUDE.md's
  rule, not a silent skip: pure-internal CI plumbing, no public API, no
  crate-roster change. `CONTRIBUTING.md` was also checked — its only permissions
  mention is the release-plz PAT (`:352`), and the required-contexts table
  (`:242`) says nothing about workflow permissions. No drift.

## Verification

**This change is not falsifiable by CI on a public repo, and the verification
plan must not pretend otherwise.** `sessions-it` is green with or without the
grant, because the call it guards already succeeds. An earlier draft argued that
because `.github/workflows/ci.yml` is in the `sessions` filter (`ci.yml:195`),
the job really runs and the change is therefore "observed" — true premise,
worthless inference. The observation is identical under both hypotheses. This
repo has a documented allergy to exactly that shape of reasoning
(`HELIKON_REQUIRE_SANDBOX`, `HELIKON_REQUIRE_TEMPORAL`), and the spec should not
reintroduce it in prose.

The verification is therefore one real check plus one regression check:

1. **The grant takes effect — the actual verification.** The `sessions-it` job
   log opens with a `GITHUB_TOKEN Permissions` group. Today it reads
   `Contents: read` and nothing more (quoted above). After this change it must
   additionally list `PullRequests: read`. This is a genuine before/after
   observable with a captured baseline. It is a one-shot manual log read; no
   gate enforces it on future runs, which is a known and accepted limitation —
   see the rejected mechanical enforcement above.
2. **Regression check.** `sessions-it` still runs and its filter step still
   succeeds. Because `.github/workflows/ci.yml` is in the `sessions` filter, this
   PR matches its own filter and the full live Postgres/Redis suite executes
   rather than taking the `echo "No session-related changes"` branch. This proves
   the edit did not break the workflow. It proves nothing about the grant.

**What would falsify the change:** nothing in this PR can. Adding a read scope
to a token that already had implicit public read access cannot make
`pulls.listFiles` start failing. The only realistic failure mode is a YAML
syntax error, which GitHub catches at dispatch — if `ci.yml` runs at all, the
file parses. Rollback is reverting one line and its comment; nothing depends on
it.

## Conventions

- Commits: the design doc lands as `docs(spec): SMA-487 …`; the workflow change
  and the `CLAUDE.md` correction land together as `ci(workflows): SMA-487 …`.
  They are one atomic unit — the CLAUDE.md sentence is wrong *only* because of
  the workflow edit. `ci`/`docs` types and `workflows`/`spec`/`claude` scopes are
  all in `.versionrc`'s allowlist; convco validates the scope token, not the file
  paths, so `ci(workflows)` on a commit touching `CLAUDE.md` is legal and
  deliberate.
- PR title: `ci(workflows): SMA-487 grant pull-requests read for sessions-it's paths-filter`
  — clears both `pr-title.yml` rules (`ci` is in `types`, `workflows` in
  `scopes`, subject lowercase after the ticket ID).
- Touches only `.github/`, `CLAUDE.md`, and `docs/`, so release-plz attributes no
  version bump to any crate.
