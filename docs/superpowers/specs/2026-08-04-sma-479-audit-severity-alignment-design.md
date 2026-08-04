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

Land the invariant: **if a PR would go red, `main` should already be red.**

Derived from the ticket's acceptance criteria:

1. An `unsound` / `unmaintained` advisory affecting `main`'s lockfile causes a visible failing
   signal on `main`, not only an auto-filed issue.
2. The daily signal and the PR gate agree on severity — no advisory class fails one and passes the
   other.
3. `CLAUDE.md`'s CI section documents the final arrangement: both jobs' tools *and* their severity
   behaviour.

## Non-goals

* **Linear issue-filing.** Replacing `rustsec/audit-check@v2` with a script that files SMA-\*
  issues via Linear's GraphQL API was considered and explicitly declined (2026-08-04). It would
  need a new `LINEAR_API_KEY` repo secret with workspace write access stored on a public repo,
  roughly 100 lines of bash + GraphQL to own indefinitely, and its own decisions about dedup,
  which advisory classes file, and behaviour during a Linear outage. All three acceptance criteria
  are met by the strict-command change alone; issue filing is a nice-to-have on top. No follow-up
  ticket is planned.
* **Removing or replacing `rustsec/audit-check@v2`.** It keeps filing GitHub receipts, unchanged.
  Its known limitations (points 2 and 3 above) are accepted, and are made tolerable by the fact
  that the strict job now reddens the same run.
* **`sbom.yml`.** Tag-triggered, no time-varying input, no severity semantics. Untouched.
* **Any change to job names or required status checks.**

## Design

### `.github/workflows/audit.yml`

Delete the event gate on the strict job, and add a manual trigger:

```yaml
 on:
   push:
     branches: [main]
   pull_request:
   schedule:
     - cron: "0 6 * * *"   # daily, 06:00 UTC
+  workflow_dispatch:

 jobs:
   audit:
-    if: github.event_name != 'schedule'
     runs-on: ubuntu-latest
```

`scheduled-audit` is untouched. It keeps `if: github.event_name == 'schedule'`, keeps its
`issues: write` permission, and keeps filing receipts.

On a cron event both jobs now run, in parallel, with no dependency between them. GitHub Actions
has no job-level fail-fast (that is matrix-only), so a failing `audit` does not prevent
`scheduled-audit` from filing its issue. The receipt and the red run are independent and both
arrive.

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
+    - cron: "0 6 * * *"   # daily, 06:00 UTC
+  workflow_dispatch:
```

No job or severity changes are needed. `cargo deny --all-features check` already has exactly one
severity on every event.

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

### Effect on the compounding trap

Today a `schedule` row in `gh run list --workflow=audit.yml` means only "the weak job passed". A
workflow run's conclusion is `failure` if any of its jobs fails, so after this change a cron run
carries the strict job's verdict: a green `schedule` row genuinely means *`main`'s lockfile was
clean against the RustSec DB at 06:00 UTC*.

"Read `event`, not just `conclusion`" therefore stops being a semantics trap and becomes a
staleness note — a green row can be up to 24 hours old. `workflow_dispatch` is the on-demand
escape from even that.

### Documentation

* **`CLAUDE.md`** — rewrite the supply-chain paragraph (currently line 105) to record both jobs'
  tools *and* their severity behaviour, the new trigger set for both workflows, and the fact that
  `scheduled-audit` is a receipt-filer with no failure semantics on any advisory class. Satisfies
  acceptance criterion 3.
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

1. **Local — proves severity.** Comment out `RUSTSEC-2025-0012` in `.cargo/audit.toml`; run
   `cargo audit --deny warnings`. It must exit non-zero and name the advisory. Restore the entry
   and confirm exit 0. This change is never committed.
2. **On the PR — proves the un-gated job still gates PRs.** The `audit` and `deny` checks run as
   `pull_request` and must remain required and green.
3. **Post-merge — proves the new trigger paths.** `gh workflow run audit.yml` and
   `gh workflow run deny.yml`, then confirm via `gh run view <id> --json jobs` that each run
   contains the strict job.
4. **First cron after merge — final confirmation.** `gh run list --workflow=audit.yml --branch
   main --json event,conclusion,createdAt` should show a `schedule` row, and
   `gh run view <id> --json jobs` should list **both** `audit` and `scheduled-audit`.

### Known pre-merge limitation

GitHub fires `workflow_dispatch` only for workflows present **on the default branch**. Because
this PR is what adds that trigger, step 3 cannot run before merge — dispatching from the feature
branch is rejected. Pre-merge evidence is therefore steps 1 and 2 plus review of the `if:`
removal; steps 3 and 4 are post-merge confirmations. This is stated explicitly so that a green PR
is not mistaken for having exercised the schedule path.

## Risks and accepted trade-offs

* **Advisory-DB cache staleness — checked, not a problem.** The `audit` job's
  `Swatinem/rust-cache` caches `~/.cargo/advisory-db`, which could in principle serve a stale DB
  and mask a newly published advisory. It does not: `cargo audit` fetches the advisory DB on every
  run unless `--no-fetch` is passed, and we do not pass it, so the cache only makes that fetch
  incremental. `cargo deny` likewise fetches unless `--offline`. Note the two tools cache
  different paths — `~/.cargo/advisory-db` (singular) for cargo-audit, `~/.cargo/advisory-dbs`
  (plural) for cargo-deny per `deny.toml`'s `db-path`.
* **Notification routing is best-effort.** GitHub mails scheduled-run failures to the user who
  last modified the cron *syntax*. This PR does not touch `audit.yml`'s cron line, so its routing
  is unchanged; `deny.yml`'s new cron routes to its author. The red run visible in the Actions tab
  and in `gh run list` is the primary signal — email is a bonus, not the mechanism, and acceptance
  criterion 1 does not depend on it.
* **60-day dormancy.** GitHub disables cron triggers in repositories with no activity for 60 days.
  This applies to both workflows equally and to `audit.yml`'s existing cron already. The repo is
  active; noted, not engineered around.
* **`main` can now be red with no PR to fix it.** This is the intended outcome — it is the
  "visible failing signal" of acceptance criterion 1. The documented escape hatch when no upgrade
  exists is the `[advisories].ignore` list, which must be added to *both* `.cargo/audit.toml` and
  `deny.toml` (they are kept in sync by policy) with a rationale recorded in the same commit, per
  `CONTRIBUTING.md`.
* **One extra runner-minute per cron day.** Accepted; see "Why un-gating" above.

## Files touched

| File | Change |
| --- | --- |
| `.github/workflows/audit.yml` | remove `if: github.event_name != 'schedule'` from the `audit` job; add `workflow_dispatch` |
| `.github/workflows/deny.yml` | add `schedule` (daily 06:00 UTC) and `workflow_dispatch` triggers |
| `CLAUDE.md` | rewrite the supply-chain paragraph: tools *and* severity behaviour, new triggers |
| `CONTRIBUTING.md` | correct the `audit` bullet's daily-run description; add the cron to `deny` |
| `docs/superpowers/specs/2026-08-04-sma-479-audit-severity-alignment-design.md` | this document |
| `docs/superpowers/plans/2026-08-04-sma-479-audit-severity-alignment.md` | implementation plan |
