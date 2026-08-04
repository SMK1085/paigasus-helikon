# SMA-479 — Align supply-chain gate severity across triggers

**Status:** approved (2026-08-04)
**Linear:** [SMA-479](https://linear.app/smaschek/issue/SMA-479)
**Branch:** `feature/sma-479-align-audityml-severity-scheduled-audit-reports-green-on`

## Problem

`.github/workflows/audit.yml` runs two jobs, gated on `github.event_name`, using two different
tools with two different severity semantics:

| Job | Trigger | Tool | On an `unsound` / `unmaintained` advisory |
| --- | --- | --- | --- |
| `audit` | `pull_request`, `push` to `main` | `cargo audit --deny warnings` | **fails the run** |
| `scheduled-audit` | daily cron | `rustsec/audit-check@v2` | files an issue, **exits 0** |

The two-tool split is deliberate and documented in `CLAUDE.md` (the scheduled job was chosen for
its auto-issue-filing behaviour). The severity asymmetry was not.

Consequence: an advisory can sit in `main`'s lockfile indefinitely while the daily signal reports
green, and simultaneously redden every open PR. That happened with RUSTSEC-2026-0221
(`event-listener` 5.4.1, `unsound`, reachable transitively via `sqlx-core 0.9.0` and
`async-lock 3.4.2` / `redis 1.4.1`):

* `scheduled-audit` reported success daily from 2026-07-20 through 2026-08-03.
* It filed GitHub issue #161 on 2026-08-01 — the only visible signal, and easy to miss.
* All five open Dependabot PRs (#158, #162, #163, #164, #165) had a red `audit` gate and could
  not merge.
* `cargo audit --deny warnings` on a clean checkout of `main` (`61c8f5c`) reproduced the failure
  in seconds.

Fixed lockfile-only in PR #166. The diagnosis cost far more than the fix, because "main's audit is
green" is the natural first thing to check and it was actively misleading.

### Compounding trap

`gh run list --workflow=audit.yml --branch main` returned only `event: schedule` rows. There had
been no push to `main` in a while, so the stricter `audit` job had not run on `main` at all — yet
the history looked like an unbroken wall of green. Reading run *conclusions* alone is insufficient
when the `event` column changes what the conclusion means.

## Root cause (verified against the action's source, 2026-08-04)

`rustsec/audit-check@v2` routes on `github.context.eventName` in `src/main.ts`:

```ts
if (github.context.eventName == 'schedule') {
    await reporter.reportIssues(actionInput.token, advisories, warnings);
} else {
    await reporter.reportCheck(actionInput.token, advisories, warnings);
}
```

`reportCheck` ends with a severity check:

```ts
if (stats.critical > 0) {
    throw new Error('Critical vulnerabilities were found, marking check as failed');
} else {
    core.info('No critical vulnerabilities were found, not marking check as failed');
    return;
}
```

`reportIssues` has **no such check** — it iterates advisories and warnings, creates issues, and
returns. Three consequences, all worse than the ticket assumed:

1. **The daily job exits 0 for every advisory class, including critical RUSTSEC vulnerabilities**
   — not merely the informational (`unsound` / `unmaintained`) ones. The `stats.critical > 0`
   failure path exists only on the non-schedule branch.
2. **`yanked` crates produce no signal whatsoever.** `reportIssues` hits an explicit `continue`:
   `` core.warning(`Crate ${warning.package.name} was yanked, but no issue will be reported about it`) ``.
   `cargo audit --deny warnings` fails on yanks, and `deny.toml` sets `yanked = "deny"`. This is
   the class that reddened every PR and `main` during the `spin 0.9.8` yank.
3. **Recurrences are permanently silent.** `alreadyReported()` queries
   `` `${advisoryId} in:title repo:${owner}/${repo}` `` with no state filter, so a *closed* issue
   counts as reported. Issue #161 is closed; if RUSTSEC-2026-0221 returned, nothing would be
   filed.

The action's entire input surface is `token`, `ignore`, and `working-directory` (`action.yml`).
**There is no configuration that changes any of the above.** The ticket's proposed option 1a
("configure `rustsec/audit-check@v2` to fail on informational advisories") is not implementable;
the fix must be a second command, not a setting.

## Goals

The ticket states the invariant as **"if a PR would go red, `main` should already be red."** Taken
literally that is not achievable and not what this change delivers — a PR that *introduces* a bad
dependency (most Dependabot lockfile PRs) can be red while `main` is legitimately green. The
precise, deliverable form is:

> **Every successfully delivered scheduled run evaluates `main`'s committed lockfile at full PR
> severity — the same job, the same command, the same exit code as the gate a PR must pass.**

Note what this deliberately does *not* claim: a maximum staleness bound. An earlier draft said
"with a lag of at most 24 hours", which contradicts this spec's own Risks section — scheduled runs
are best-effort, and GitHub may delay them, drop them under load, or disable them entirely after 60
days of repository inactivity. No upper bound is enforceable, so the invariant is defined per
*delivered* run, and a missing run counts as unverified rather than as passing.

Derived from the ticket's acceptance criteria:

1. An `unsound` / `unmaintained` advisory affecting `main`'s lockfile causes a visible failing
   signal on `main`, not only an auto-filed issue.
2. The daily signal and the PR gate agree on severity — no advisory class fails one and passes the
   other.
3. `CLAUDE.md`'s CI section documents the final arrangement: both jobs' tools *and* their severity
   behaviour.

### AC1 has two halves, and this change delivers them unequally

It is worth separating them, because conflating them is how a spec talks itself into believing a
weaker signal is a stronger one:

* **Diagnosis** — *when someone checks whether `main` is clean, do they get the truth?* Today they
  get a lie: `scheduled-audit` is green on every advisory class. This change fixes that
  completely, and that is the failure that actually cost the time on 2026-08-03.
* **Detection** — *does anyone find out without looking?* This is weaker, and the spec must not
  pretend otherwise. The mechanisms, strongest first:
  1. **A check run on `main`'s HEAD commit.** A `schedule` run creates check runs against the
     default branch's head SHA, so a failure renders as a red ✗ beside the latest commit on the
     repo home page and in the commit list. Query it with the **Checks API**:
     `gh api repos/{owner}/{repo}/commits/main/check-runs --jq '.check_runs[] | select(.name=="audit") | {name,status,conclusion}'`.
     This — not the Actions tab — is the real AC1 surface, and verification step 6 checks it.

     **Do not use `/commits/{ref}/status` for this.** GitHub Actions publishes *check runs*; that
     legacy endpoint returns only commit statuses. Measured on this repository (PR #172 head SHA):
     `/status` returned `{"state":"success","total_count":1,"contexts":["CodeRabbit"]}` — a
     confident green containing **no audit verdict at all** — while `/check-runs` returned 22 runs
     including `audit: success` and `deny: success`. An earlier draft of this spec used the legacy
     endpoint, which would have reproduced the precise bug this document exists to fix: a green
     reading that does not mean what it appears to mean.
  2. **A `failure` row in `gh run list --workflow=audit.yml --branch main`**, which is what the
     ticket's own repro section tells the next person to read.
  3. **Email**, which is genuinely best-effort — see Risks.

  Detection remains pull-based. That is an accepted limitation of this change, recorded here
  rather than glossed.

## Non-goals

* **Linear issue-filing.** Replacing `rustsec/audit-check@v2` with a script that files SMA-\*
  issues via Linear's GraphQL API was considered and explicitly declined (2026-08-04). It would
  need a new `LINEAR_API_KEY` repo secret with workspace write access stored on a public repo,
  roughly 100 lines of bash + GraphQL to own indefinitely, and its own decisions about dedup,
  which advisory classes file, and behaviour during a Linear outage. All three acceptance criteria
  are met by the strict-command change alone; issue filing is a nice-to-have on top. No follow-up
  ticket is planned.
* **Removing or replacing `rustsec/audit-check@v2`.** It keeps filing GitHub receipts, unchanged.
  Be explicit about what that accepts: after this change the receipt mechanism remains **broken for
  both incidents on record** — it files nothing for yanked crates (the `spin 0.9.8` class) and never
  re-files an advisory whose issue has been closed (so a RUSTSEC-2026-0221 recurrence would be
  silent). This is tolerable only because the strict job now reddens the same run: the receipt is
  demoted from *the* signal to a convenience, and the design no longer depends on it being correct.
  A ~6-line `if: failure() && github.event_name == 'schedule'` step calling `gh issue create` with
  the ambient `GITHUB_TOKEN` would close both holes with no new secret and no second copy of the
  strict command — it was evaluated and declined for this ticket, keeping the diff to triggers and
  documentation. Recorded here so the decision is visible rather than implied.
* **`sbom.yml` and `msrv.yml`.** `sbom.yml` is tag-triggered with no time-varying input and no
  severity semantics. Both remain on mutable action tags after this PR; the SHA-pinning sweep below
  is deliberately scoped to the two files this change opens.
* **`ci.yml`'s equivalent drift — out of scope, and no follow-up ticket.** `ci.yml` runs
  `dtolnay/rust-toolchain@stable` with `cargo clippy … -D warnings` and has no cron, so a new Rust
  stable release that adds lints reddens every PR while `main` stays green on its last run against
  the old compiler — structurally the same pathology, and in practice the one that fires most
  often. It is excluded because the mechanism differs (toolchain version, not advisory database)
  and because a cron over `ci.yml`'s eight jobs, including a 3×2 test matrix, is a materially
  larger standing commitment than two single-job supply-chain workflows. Recorded so that the Goal
  above is not read as a workspace-wide guarantee.
* **Any change to job names or required status checks.**

## Design

### `.github/workflows/audit.yml`

Delete the event gate on the strict job, add a manual trigger, and repair the concurrency group:

```yaml
 on:
   push:
     branches: [main]
   pull_request:
   schedule:
     - cron: "0 6 * * *"   # daily, 06:00 UTC
+  workflow_dispatch:

 concurrency:
-  group: audit-${{ github.workflow }}-${{ github.ref }}
+  group: audit-${{ github.workflow }}-${{ github.ref }}-${{ github.event_name }}
   cancel-in-progress: ${{ github.event_name == 'pull_request' }}

 jobs:
   audit:
-    if: github.event_name != 'schedule'
     runs-on: ubuntu-latest
```

`scheduled-audit` is untouched. It keeps `if: github.event_name == 'schedule'`, keeps its
`issues: write` permission, and keeps filing receipts.

On a cron event both jobs now run, in parallel, with no dependency between them. GitHub Actions
has no job-level fail-fast (that is matrix-only), so a failing `audit` does not prevent
`scheduled-audit` from filing its issue.

#### The concurrency group must be keyed on the event, or the daily run can vanish

The existing group is `audit-${{ github.workflow }}-${{ github.ref }}` with `cancel-in-progress`
true only for pull requests. On `schedule`, `workflow_dispatch`, and `push`-to-`main`, `github.ref`
is all three times `refs/heads/main` — so all three share one group with `cancel-in-progress:
false`. GitHub's documented behaviour for that combination is that a run entering an occupied group
goes *pending*, and **"any previously pending job or workflow in the concurrency group will be
canceled."**

The consequence: a 06:00 cron run that arrives while a push-to-`main` run is in flight goes
pending, and the next merge to `main` cancels it outright. Today that costs nothing, because the
cron run only carries the decorative `scheduled-audit`. After this change it would silently discard
the day's only strict evaluation of `main`, and leave behind a row whose conclusion is `cancelled`
— neither `success` nor `failure`. That is the ticket's own disease (absence misread as health) in
a new costume.

Adding `-${{ github.event_name }}` to the group separates the three event streams. Push runs to
`main` remain serialised against each other, PR runs keep cancelling their own predecessors, and
cron runs no longer compete with anything. The same fix applies to `deny.yml`, whose concurrency
block has the identical shape.

### `.github/workflows/deny.yml`

`deny` is a required status check whose verdict depends on the same time-varying inputs — the
RustSec advisory DB and upstream yanks (`deny.toml` sets `yanked = "deny"` and carries an
`[advisories].ignore` list mirroring `.cargo/audit.toml`). It has **no cron at all**: the same
disease in a worse form, since there is not even a weak daily signal to be misled by.

```yaml
 on:
   push:
     branches: [main]
   pull_request:
+  schedule:
+    - cron: "17 6 * * *"  # daily, 06:17 UTC — offset from audit.yml's 06:00
+  workflow_dispatch:

 concurrency:
-  group: deny-${{ github.workflow }}-${{ github.ref }}
+  group: deny-${{ github.workflow }}-${{ github.ref }}-${{ github.event_name }}
   cancel-in-progress: ${{ github.event_name == 'pull_request' }}
```

No job or severity changes are needed. `cargo deny --all-features check` already has exactly one
severity on every event.

The cron is deliberately offset to `06:17`. Top-of-hour is GitHub's most congested scheduling slot;
scheduled runs are best-effort and can be delayed or dropped under load, and stacking two
supply-chain workflows plus Dependabot's Monday 06:00 window on the same minute maximises that
risk for no benefit.

### Why un-gating, and not a strict step inside `scheduled-audit`

Both routes make the daily run red. They differ in how they fail *later*.

Adding `- run: cargo audit --deny warnings` as a step inside `scheduled-audit` creates a **second
copy** of the strict command. Nothing then prevents a future edit — a new flag, an `--ignore`, a
version pin — from landing on one copy and not the other, silently reopening precisely the
asymmetry this ticket exists to close.

Un-gating means the daily signal and the PR gate are *the same job definition*. There is one
command, in one place, and drift is structurally impossible rather than merely discouraged. The
cost is one extra runner and a duplicated checkout + `cargo-audit` install on cron days, roughly a
minute. That is the right trade.

### SHA-pin the actions in both files

`CLAUDE.md` requires every `uses:` line to pin a commit SHA of the action's latest stable major,
with a human-readable `# action vX.Y.Z` comment above it. `ci.yml`, `docs.yml`, `bench.yml`, and
`release-plz.yml` comply. `audit.yml` and `deny.yml` do not — they carry bare `actions/checkout@v7`,
`dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`, `taiki-e/install-action@v2.85.5`, and
`rustsec/audit-check@v2`.

Two reasons this belongs in *this* PR rather than a sweep:

1. `rustsec/audit-check@v2` is **the only third-party action in the repository that runs with write
   permission** (`issues: write`). A mutable tag on it is the highest-value pin available.
2. This spec's entire Root-cause section, and the Non-goals decision resting on it, were derived by
   reading the source at whatever commit `v2` resolved to on 2026-08-04. Unpinned, that analysis
   expires silently the moment the tag moves. Pinning anchors the reasoning to the artefact it was
   performed against.

Also add `persist-credentials: false` to both checkouts, matching every other workflow in the repo.

**Two gotchas the implementation must not trip over:**

* `dtolnay/rust-toolchain` normally selects the toolchain **from its ref name** (`@stable`).
  Pinning it to a SHA destroys that signal, so the pin must be accompanied by an explicit
  `with: { toolchain: stable }`, exactly as `ci.yml` does. A bare SHA pin here would be a silent
  behaviour change, not a no-op.
* Resolve the SHAs **at implementation time**, not from this document — `CLAUDE.md` explicitly
  forbids using a plan-time pin if a newer release shipped in between. Note that `ci.yml` currently
  pins `taiki-e/install-action` *older* (v2.79.x) than the `v2.85.5` tag these two files request, and
  labels the same SHA `v2.79.0` in `ci.yml` and `v2.79.3` in `docs.yml` — one of those comments is
  already wrong. Do not propagate either; resolve fresh, and leave `ci.yml` to Dependabot's
  `github-actions` group.

### Enforce ignore-list sync between the two policy files

`.cargo/audit.toml` and `deny.toml` currently carry the same three `[advisories].ignore` entries,
kept in sync by prose policy (`CONTRIBUTING.md`) and nothing else. Adding a cron to `deny.yml` means
both lists are now evaluated daily against the same advisory database — so a one-line drift produces
`audit` green beside `deny` red on `main`, every day, until someone notices. Closing a
severity-asymmetry hole while leaving an unguarded one in the adjacent file is not a good trade.

Add `scripts/check-advisory-ignore-sync.sh`: extract the quoted advisory IDs from each file, sort,
deduplicate, and compare. Wire it as a step in the `deny` job, before `cargo deny` runs.

**What it asserts, precisely: equality of the two quoted-advisory-ID *sets*.** Not byte identity,
and not list ordering — the entries may appear in any order, with any surrounding comments or
formatting, and a repeated ID collapses to one. That is the right granularity, because the two
files legitimately differ everywhere else (`deny.toml` also carries `[licenses]`, `[bans]`, and
`[sources]` sections) and their ignore-entry rationale comments are maintained independently.

Extraction matches **quoted** IDs only — `grep -oE '"RUSTSEC-[0-9]{4}-[0-9]{4}"'`. This is
deliberate and load-bearing: both files' comments mention `RUSTSEC-2023-0071` and
`RUSTSEC-2025-0052` in unquoted prose as historical notes about entries that were *removed*. A
naive unquoted grep would match those and report permanent false sync. The known limitation is that
a quoted advisory ID inside a comment would produce a false positive; that is acceptable and noted
in the script's header.

### A trap to document in-place

`workflow_dispatch` exercises the strict job but **not** the cron path's distinguishing behaviour:
`scheduled-audit` keeps `if: github.event_name == 'schedule'`, so a manual run produces one job,
not two. The obvious "improvement" — widening that condition to
`|| github.event_name == 'workflow_dispatch'` so dispatch rehearses the full cron run — is a trap.
It would route `rustsec/audit-check` down its `reportCheck` branch, which calls the Checks API and
needs `checks: write`; the job grants only `contents: read` + `issues: write`, so it would 403.

This gets a short comment beside the `if:` in `audit.yml`, because that is where someone will trip
over it, not in a design document they will not be reading at the time.

### Effect on the compounding trap

Today a `schedule` row in `gh run list --workflow=audit.yml` means only "the weak job passed". A
workflow run's conclusion is `failure` if any of its jobs fails (absent `continue-on-error`, which
neither workflow uses), so after this change a cron run carries the strict job's verdict: a green
`schedule` row genuinely means *`main`'s lockfile was clean against the RustSec DB at 06:00 UTC*.

"Read `event`, not just `conclusion`" therefore stops being a semantics trap and becomes a
staleness note — a green row can be up to 24 hours old. `workflow_dispatch` is the on-demand
escape from even that.

**One caveat survives, and belongs in the documentation:** a row that is `cancelled`, or a day with
no row at all, is *not* a green row. Scheduled runs are best-effort — GitHub can delay or drop them
under load, and disables them entirely after 60 days of inactivity in a public repository. The
concurrency fix above removes the one cause of cancellation this change would otherwise have
introduced, but it cannot make cron delivery guaranteed.

The honest reading rule is therefore **not** "red means dirty" — that overclaims in two directions
this spec itself documents elsewhere. A run also goes red when the advisory-DB or crates.io fetch
fails transiently, and when `scheduled-audit` fails (say, the GitHub API rejecting an issue write)
while the strict `audit` job passed and the lockfile is clean. State it at job granularity:

> **Green** — the strict `audit` job passed; `main`'s lockfile is clean against the advisory DB as
> of that run. **Red** — some job failed and needs triage: read *which* job, then reproduce with
> `cargo audit --deny warnings` on a clean checkout before concluding an advisory exists.
> **Absent or `cancelled`** — unverified, exactly as before this change.

### Documentation

* **`CLAUDE.md`** — rewrite the supply-chain paragraph (currently line 105) to record both jobs'
  tools *and* their severity behaviour, the new trigger set for both workflows, and — in one
  explicit sentence — that **`scheduled-audit`'s green job status means nothing at any severity**,
  including critical vulnerabilities. AC2 is satisfied at the *run* level, not the job level: the
  run is red because `audit` failed, while `scheduled-audit` sits green beside it. Anyone reading
  job status instead of run status is back in the original trap. Must also carry the reading rule
  from "Effect on the compounding trap" (absent or cancelled ≠ green) and the concurrency-group
  rationale, so a future editor does not "simplify" the group key back. Satisfies acceptance
  criterion 3.
* **`CONTRIBUTING.md`** — the `audit` bullet (around line 247) currently reads "plus a daily
  scheduled run on `main` that auto-files a GitHub issue if a new advisory affects the locked
  deps", which omits that it cannot fail. Correct it and add the cron to the `deny` bullet.
* **mdBook — no edit, deliberate.** The three `audit` matches under `docs/book/src/` are "audit
  trail" and "audit-grade schema" in the sessions and observability pages; none concern CI.
* **Crate READMEs and the root README — no edit, deliberate.** No crate's public API, install
  story, feature map, or published status changes.
* **`.github/rulesets/main-protection-checks.json` — no edit.** Job names are unchanged, so the
  `audit` and `deny` required contexts keep resolving.

## Verification

`.cargo/audit.toml` suppresses three advisories that are genuinely present in the lockfile.
`RUSTSEC-2025-0012` (`backoff`, unmaintained, transitive via `temporalio-*` 0.5) is therefore a
live test lever for exactly the advisory class in the ticket.

Baseline, confirmed 2026-08-04 on this branch: `cargo audit --deny warnings` exits 0, and
`cargo audit --json` reports `vulnerabilities.found = false` with an empty `warnings` object —
the policy-ignored advisories are fully suppressed from JSON output, not merely downgraded.

1. **Local, static — proves the YAML is valid and the triggers parse.** Run `actionlint` over both
   changed files. This is the one piece of pre-merge evidence for `deny.yml` that actually matters:
   its change is a *brand-new* `on: schedule:` block, and a mistyped key or wrong indentation
   fails **open and silently** — PRs keep passing, and the cron simply never fires. Nobody would
   notice for months. `actionlint` also validates the `${{ }}` expressions in the reworked
   concurrency groups. It is not currently installed (`brew install actionlint`), and this PR does
   *not* add it as a CI gate — that is a separate concern; here it is a pre-merge check the
   implementer runs and pastes the output of.
2. **Local, behavioural — proves severity.** Comment out `RUSTSEC-2025-0012` in
   `.cargo/audit.toml`; run `cargo audit --deny warnings`. It must exit non-zero and name the
   advisory. Restore the entry and confirm exit 0. This edit is never committed — so **paste both
   outputs into the PR body**, or the only evidence for the central claim of this spec evaporates
   with the shell session.
3. **Local — proves the sync check works in both directions.** Run
   `scripts/check-advisory-ignore-sync.sh` against the tree as-is: it must exit 0 and report three
   IDs. Then temporarily delete one entry from `deny.toml` only: it must exit non-zero and name the
   missing ID. Restore. A sync check that has only ever been observed passing is not evidence.
4. **On the PR — proves the un-gated job still gates PRs.** The `audit` and `deny` checks run as
   `pull_request` and must remain required and green — `deny` now also running the sync step, and
   both jobs now running on SHA-pinned actions with an explicit `toolchain: stable`.
5. **Post-merge, same day — proves the new trigger paths.** `gh workflow run audit.yml` and
   `gh workflow run deny.yml`, then confirm via `gh run view <id> --json jobs` that each run
   contains the strict job.
6. **First cron after merge (next 06:00/06:17 UTC) — final confirmation.**
   `gh run list --workflow=audit.yml --branch main --json event,conclusion,createdAt` should show a
   `schedule` row; `gh run view <id> --json jobs` should list **both** `audit` and
   `scheduled-audit`; and `gh run list --workflow=deny.yml --branch main --json event` should show
   a `schedule` row that did not exist before.

**Steps 5 and 6 are owned by the PR author and due the day after merge.** A verification step with
no owner and no date is how the `deny` cron ends up never having fired with nobody finding out.
Step 6 also confirms the AC1 detection surface — via the Checks API, not the legacy status API:
`gh api repos/{owner}/{repo}/commits/main/check-runs --jq '.check_runs[] | select(.name=="audit") | {name,status,conclusion}'`
should reflect the cron run's verdict against `main`'s HEAD.

### Known pre-merge limitation

GitHub fires `workflow_dispatch` only for workflows present **on the default branch**. Because this
PR is what adds that trigger, step 5 cannot run before merge — dispatching from the feature branch
is rejected. Pre-merge evidence is therefore steps 1–4 plus review of the `if:` removal; steps 5
and 6 are post-merge confirmations. This is stated explicitly so that a green PR is not mistaken
for having exercised the schedule path.

Two ways to convert step 5 into pre-merge evidence were considered and declined:

* **A precursor PR** adding only `workflow_dispatch` to both files, merged first, so the trigger is
  live on the default branch before the semantics change lands. This works, but costs a full extra
  PR cycle to de-risk a four-line diff.
* **A scratch fork** whose default branch carries the change, dispatched there. Also works, and
  also costs more setup than `actionlint` plus reading a one-line `if:` deletion.

`actionlint` (step 1) covers the failure mode those would have caught — a malformed trigger block —
at a fraction of the cost.

### Rollback

Both changes are single-commit reverts with no state to unwind: re-add
`if: github.event_name != 'schedule'` to the `audit` job, and drop the `schedule:` block from
`deny.yml`. No published artefact, crate version, or branch-protection setting is touched.

## Risks and accepted trade-offs

* **Advisory-DB cache staleness — checked, not a problem.** The `audit` job's
  `Swatinem/rust-cache` caches `~/.cargo/advisory-db`, which could in principle serve a stale DB
  and mask a newly published advisory. It does not: `cargo audit` fetches the advisory DB on every
  run unless `-n` / `--no-fetch` is passed, and we do not pass it, so the cache only makes that
  fetch incremental. `cargo deny` likewise fetches unless suppressed — by `-d` / `--disable-fetch`
  on the `check` subcommand, or the global `--offline`; we pass neither. Note the two tools cache
  different paths — `~/.cargo/advisory-db` (singular) for cargo-audit, `~/.cargo/advisory-dbs`
  (plural) for cargo-deny per `deny.toml`'s `db-path`.
* **A red `main` does not block unrelated PRs — confirmed, not assumed.**
  `.github/rulesets/main-protection-checks.json` sets `strict_required_status_checks_policy:
  false`, so PRs are not required to be up to date with `main`, and required contexts are evaluated
  against the PR's own head SHA. A failing `audit` check run created by a cron against `main`'s HEAD
  is therefore inert with respect to merging. This matters because "red `main` with no PR to fix
  it" becomes a *steady state* during an unfixable-advisory window, and a reviewer should not have
  to derive its blast radius.
* **Fork PRs are unaffected.** The strict job needs no secrets and no write permissions;
  `scheduled-audit` never runs on `pull_request`; GitHub disables `schedule` on forks by default;
  and `workflow_dispatch` requires write access to the repository. Stated so the next reviewer does
  not have to re-derive it.
* **Notification routing is best-effort, and in this repo it is entirely untested.** GitHub mails
  scheduled-run failures to the user who last modified the cron *syntax*, subject to that user's
  Watch and Actions notification settings. This PR does not touch `audit.yml`'s cron line, so its
  routing is unchanged; `deny.yml`'s new cron routes to its author. Sharper point: `audit.yml`'s
  cron has existed since SMA-306 and **every scheduled run in the visible history has concluded
  `success`** — so no failure email has ever actually been delivered from this workflow. "Email is
  best-effort" understates it; the mechanism is unproven here. AC1 therefore rests on the check run
  against `main`'s HEAD and on `gh run list`, not on email.
* **60-day dormancy.** GitHub disables cron triggers in **public** repositories with no activity
  for 60 days (it emails first; private-repo behaviour differs). This applies to both workflows
  equally and to `audit.yml`'s existing cron already. The repo is active; noted, not engineered
  around.
* **A network flake can turn `main` red for a non-reason.** Both tools fetch over the network — the
  advisory DB, and the crates.io index. A transient failure produces a red cron run that looks
  exactly like a real advisory hit. The whole design rests on "red `main` means something," so a
  few false reds would train the maintainer to ignore it. **Triage rule: reproduce any red cron run
  with `cargo audit --deny warnings` (or `cargo deny --all-features check`) on a clean checkout of
  `main` before treating it as an advisory** — which is what the ticket's own repro section already
  prescribes as authoritative.
* **`main` can now be red with no PR to fix it.** This is the intended outcome — it is the
  "visible failing signal" of acceptance criterion 1. The documented escape hatch when no upgrade
  exists is the `[advisories].ignore` list, which must be added to *both* `.cargo/audit.toml` and
  `deny.toml` (they are kept in sync by policy) with a rationale recorded in the same commit, per
  `CONTRIBUTING.md`.
* **One extra runner-minute per cron day.** Accepted; see "Why un-gating" above.

## Files touched

| File | Change |
| --- | --- |
| `.github/workflows/audit.yml` | remove `if: github.event_name != 'schedule'` from the `audit` job; add `workflow_dispatch`; key the concurrency group on `github.event_name`; add the `workflow_dispatch`-widening trap comment; SHA-pin all four `uses:` lines with `# action vX.Y.Z` comments; add `toolchain: stable` and `persist-credentials: false` |
| `.github/workflows/deny.yml` | add `schedule` (daily 06:17 UTC) and `workflow_dispatch` triggers; key the concurrency group on `github.event_name`; add the ignore-sync step; SHA-pin all four `uses:` lines; add `toolchain: stable` and `persist-credentials: false` |
| `scripts/check-advisory-ignore-sync.sh` | **new** — asserts `.cargo/audit.toml` and `deny.toml` ignore lists match |
| `CLAUDE.md` | rewrite the supply-chain paragraph: tools *and* severity behaviour, new triggers, `scheduled-audit`'s green means nothing, absent/cancelled ≠ green, concurrency-group rationale |
| `CONTRIBUTING.md` | correct the `audit` bullet's daily-run description; add the cron to `deny`; document the ignore-sync check beside the existing "keep both files in sync" prose |
| `docs/superpowers/specs/2026-08-04-sma-479-audit-severity-alignment-design.md` | this document |
| `docs/superpowers/plans/2026-08-04-sma-479-audit-severity-alignment.md` | implementation plan |
