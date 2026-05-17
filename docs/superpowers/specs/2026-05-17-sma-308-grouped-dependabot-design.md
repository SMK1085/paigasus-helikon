# SMA-308 — Grouped weekly Dependabot updates

**Linear issue**: [SMA-308](https://linear.app/smaschek/issue/SMA-308/grouped-weekly-dependabot-updates)
**Status**: design approved 2026-05-17
**Branch**: `feature/sma-308-grouped-weekly-dependabot-updates`
**Depends on**: SMA-306 (supply-chain security — landed; contributed the initial placeholder `dependabot.yml`)
**Related**: SMA-335 (Conventional Commits enforcement — must remain compatible with the `chore(deps):` prefix used here)

## 1. Goal & non-goals

**Goal.** Replace the placeholder `.github/dependabot.yml` from SMA-306 with the steady-state configuration that consolidates dependency updates into at most ~5 grouped PRs per ecosystem per week, with major bumps split out for visibility and lockstep-release crate families grouped together.

The single deliverable is one file:

- `.github/dependabot.yml` — two ecosystems (`cargo`, `github-actions`), weekly schedule, six groups total, `chore(deps):` commit prefix, `area:deps` label, hardcoded assignee.

**Non-goals.**

- Creating a `CODEOWNERS` file. The ticket mentions assignees "from CODEOWNERS", but no such file exists in the repo yet. Introducing CODEOWNERS pulls in branch-protection and review-routing concerns that belong to a separate ticket (likely paired with SMA-309 branch protection). For this ticket we hardcode `SMK1085` as the assignee on Dependabot PRs, and revisit when CODEOWNERS lands.
- Auto-merging Dependabot PRs. Auto-merge requires branch protection + a merge workflow + a green-CI gate, all out of scope here.
- Version-ignore rules (e.g. pinning `tokio` to a major). None of the workspace deps have known incompatibilities yet; revisit reactively if one appears.
- Modifications to existing `audit.yml`, `deny.yml`, `sbom.yml`, or `ci.yml` workflows.
- A new GitHub-side validation workflow for `dependabot.yml`. Schema validation happens once in the implementation plan's local CI loop via `check-jsonschema`; we don't need a recurring CI job for a file that only changes via PR.

**No new required-status-check IDs.** This ticket adds no workflows, so SMA-309's branch-protection list is unaffected.

## 2. File layout

```
.github/
├── dependabot.yml         # rewritten end-to-end
└── workflows/             # untouched
    ├── ci.yml
    ├── audit.yml
    ├── deny.yml
    ├── sbom.yml
    └── msrv.yml
```

No other files in the repo change.

## 3. The `dependabot.yml` shape

Two `updates:` entries, one per ecosystem. Each entry declares its `groups:` in **specific-before-general** order, because Dependabot v2 evaluates groups top-to-bottom and a dependency matched by an earlier group is excluded from later ones.

### 3.1 Cargo ecosystem

Five groups, in declaration order:

1. **`tokio-stack`** — patterns `tokio*`, `tower*`, `hyper*`, `reqwest*`. No `update-types:` filter — catches all bump sizes (incl. major). Rationale: these four crate families release in lockstep frequently enough that splitting their majors into separate PRs causes more thrash (interleaved breaking changes) than it prevents.
2. **`tracing-otel`** — patterns `tracing*`, `opentelemetry*`. No `update-types:` filter. Same reasoning: `tracing-subscriber` + `tracing-opentelemetry` + `opentelemetry_sdk` move together.
3. **`serde-stack`** — patterns `serde*`, `schemars`, `serde_with*`. No `update-types:` filter. `serde` + `serde_derive` + `serde_json` + `schemars` form a tight cluster.
4. **`rust-major`** — pattern `*`, `update-types: [major]`. Catches major bumps of everything **not** already absorbed by groups 1–3. Major PRs land separately so they get explicit review.
5. **`rust-minor-patch`** — pattern `*`, `update-types: [minor, patch]`. The catch-all for the remaining mass of routine bumps.

**Patterns that currently match nothing.** `tower*`, `hyper*`, `reqwest*` (group 1) and `serde_with*` (group 3) are not in the workspace as of 2026-05-17. Dependabot silently ignores no-match patterns; the groups activate the moment those deps land. This is deliberate forward-compat — the alternative (gating the patterns on dep presence) would force a `dependabot.yml` edit in every future ticket that pulls in one of these crates.

### 3.2 GitHub Actions ecosystem

One group:

6. **`gh-actions`** — pattern `*`. No `update-types:` filter (action bumps are uniformly cheap to review, so we don't separate majors here).

### 3.3 Shared per-ecosystem fields

| Field | Value | Notes |
|---|---|---|
| `schedule.interval` | `weekly` | Same as the SMA-306 baseline. |
| `schedule.day` | `monday` | Same. |
| `schedule.time` | `"06:00"` | Same. |
| `schedule.timezone` | `"Etc/UTC"` | **Deliberate deviation from the ticket's `Europe/Vienna`.** Holds the cron alignment with `audit.yml`'s daily 06:00 UTC schedule, which CLAUDE.md documents as an intentional architectural property. DST drift between Vienna and UTC would silently desync the two crons twice a year. |
| `open-pull-requests-limit` | `5` | Per ticket. Replaces the SMA-306 baseline's `10` (cargo) and `5` (actions). |
| `commit-message.prefix` | `chore(deps)` | Per ticket. Replaces SMA-306's `chore(ci)` for the actions ecosystem so all dep-bump commits share a prefix — simpler for release-plz changelog filtering. |
| `labels` | `["area:deps"]` | Per ticket. Same label across both ecosystems — no `area:ci` for actions. |
| `assignees` | `["SMK1085"]` | Inline list, no CODEOWNERS dependency (see §1 non-goals). |

## 4. Concrete `dependabot.yml`

```yaml
version: 2
updates:
  - package-ecosystem: cargo
    directory: "/"
    schedule:
      interval: weekly
      day: monday
      time: "06:00"
      timezone: "Etc/UTC"
    open-pull-requests-limit: 5
    commit-message:
      prefix: "chore(deps)"
    labels: ["area:deps"]
    assignees: ["SMK1085"]
    groups:
      tokio-stack:
        patterns:
          - "tokio*"
          - "tower*"
          - "hyper*"
          - "reqwest*"
      tracing-otel:
        patterns:
          - "tracing*"
          - "opentelemetry*"
      serde-stack:
        patterns:
          - "serde*"
          - "schemars"
          - "serde_with*"
      rust-major:
        patterns: ["*"]
        update-types: ["major"]
      rust-minor-patch:
        patterns: ["*"]
        update-types: ["minor", "patch"]

  - package-ecosystem: github-actions
    directory: "/"
    schedule:
      interval: weekly
      day: monday
      time: "06:00"
      timezone: "Etc/UTC"
    open-pull-requests-limit: 5
    commit-message:
      prefix: "chore(deps)"
    labels: ["area:deps"]
    assignees: ["SMK1085"]
    groups:
      gh-actions:
        patterns: ["*"]
```

## 5. Dependabot security updates

Dependabot security updates (auto-PRs for advisories from the GitHub Advisory Database) are a **repo-settings toggle**, not file config. The ticket calls for enabling them separately.

**Click-path** (Settings → Code security and analysis):

- "Dependabot alerts" → **Enable** (presumed already on from SMA-306; verify).
- "Dependabot security updates" → **Enable**.
- "Grouped security updates for Dependabot" → **Enable** (so security PRs also respect the grouping rules above).

These toggles are operator-applied, not committable. The implementation plan includes a verification step that confirms all three are on via `gh api /repos/{owner}/{repo}` (the `security_and_analysis` block).

## 6. Interaction with release-plz

release-plz reads conventional-commits trailers since the last per-crate tag to decide bumps. With `chore(deps):` prefix and `include: scope`, Dependabot's commits look like:

```
chore(deps)(deps): bump tokio from 1.40 to 1.41
chore(deps)(cargo): bump serde from 1.0.210 to 1.0.215 in the serde-stack group
```

`chore(...)` commits are non-bumping in release-plz's default config, so a routine Dependabot PR will **not** trigger a release. This is the intended behavior — releases come from `feat:`/`fix:` commits authored by humans.

The doubled `(deps)(cargo)` looks odd but is harmless: Dependabot inserts the ecosystem as `scope` when `include: scope` is set, even when `prefix` already contains parentheses. release-plz parses the leading `chore` correctly. If this aesthetic ever becomes load-bearing we can drop `include: scope` and live with `chore(deps): bump …`.

## 7. Interaction with SMA-335 (Conventional Commits enforcement)

SMA-335 will add a CI check that PR titles conform to Conventional Commits. Dependabot PR titles inherit the `commit-message.prefix`, so they'll arrive as `chore(deps)(deps): …` or `chore(deps)(cargo): …`. The PR-title linter (whatever SMA-335 picks — `commitlint`, `action-semantic-pull-request`, etc.) must accept the doubled-parens form, OR we drop `include: scope` before SMA-335 lands. The first option is preferable; revisit when SMA-335 picks a tool.

## 8. Schema and edge cases

### 8.1 `commit-message.include: scope` semantics

The dependabot schema's `commit-message.include` accepts only the literal `"scope"` (or omitted). When set, Dependabot inserts the ecosystem name as a Conventional-Commits scope **after** any literal scope in `prefix`. So `prefix: "chore(deps)"` + `include: "scope"` yields `chore(deps)(deps): …`, not `chore(deps): …`. Documented edge case, not a bug — this matches the SMA-306 baseline's existing behavior. We accept the doubled-parens form for now; see §7.

### 8.2 Group exclusion order

Dependabot evaluates groups top-to-bottom. A dep matched by an earlier group is **removed from the candidate set** for later groups, even if it would also match. Without this rule, a `tokio` major would land both in `tokio-stack` and `rust-major`, producing two PRs for the same bump. The §3.1 ordering (`tokio-stack` → `tracing-otel` → `serde-stack` → `rust-major` → `rust-minor-patch`) guarantees the desired exclusion.

### 8.3 `update-types` on a wildcard-pattern group

`rust-major` and `rust-minor-patch` both use `patterns: ["*"]` with disjoint `update-types`. This is a supported Dependabot pattern (introduced ~late 2023) and the only sanctioned way to split major from minor/patch into separate PRs.

### 8.4 What happens if Dependabot can't parse the file

Dependabot validates `dependabot.yml` on every push to the default branch and surfaces parse/schema errors as a GitHub UI banner under the repo's Insights → Dependency graph → Dependabot. There's no PR-blocking gate. The implementation plan therefore validates the file locally (via `check-jsonschema` against the schema at <https://json.schemastore.org/dependabot-2.0.json>) before opening the PR.

### 8.5 No-match patterns

`tower*`, `hyper*`, `reqwest*`, `serde_with*` don't currently match any workspace dep. Dependabot does **not** error on no-match patterns; it logs them silently and skips. Verified against the schema and the Dependabot docs. The risk is only that someone reads the file and wonders why those patterns exist — the spec and the design doc document the forward-compat reason.

## 9. Decisions log

| Decision | Choice | Why |
|---|---|---|
| Replace or augment the SMA-306 placeholder? | **Replace** | SMA-306's two-group config was explicitly a placeholder; SMA-308 is the intended steady state. |
| Timezone: `Europe/Vienna` (ticket) or `Etc/UTC` (existing)? | **`Etc/UTC`** | Holds alignment with the daily audit cron documented in CLAUDE.md. DST drift would silently desync them twice a year. |
| CODEOWNERS-derived assignees or hardcoded? | **Hardcoded `["SMK1085"]`** | No CODEOWNERS file exists; introducing one is scope creep with permissions implications. Revisit when CODEOWNERS lands. |
| Actions ecosystem commit prefix: `chore(deps)` (ticket) or `chore(ci)` (existing)? | **`chore(deps)`** | Unifies all dep-bump commits under one prefix; simpler release-plz changelog filtering. |
| Actions ecosystem label: `area:deps` (ticket) or `area:ci` (existing)? | **`area:deps`** | Same rationale — one label per concern (deps), regardless of ecosystem. |
| Open-PR cap | **5/5** | Per ticket. Lower cap is the whole point of grouping. |
| Include aspirational patterns (`tower*`, etc.) that don't yet match deps? | **Yes** | Forward-compat; no-match patterns are harmless and self-documenting. |
| Split majors into a separate PR group? | **Yes** for the catch-all `rust-major`; **No** for the three lockstep stacks | Lockstep stacks (`tokio-stack`, `tracing-otel`, `serde-stack`) need to move together — separating their majors causes interleaved breaking changes. The catch-all gets the split so single-dep majors get explicit review. |

## 10. Verification

Acceptance criteria in the ticket are post-merge observational. The implementation plan has both pre-merge and post-merge checks:

**Pre-merge** (in the PR itself):

1. Schema-validate the new `dependabot.yml` against the upstream schema using `check-jsonschema --schemafile https://json.schemastore.org/dependabot-2.0.json .github/dependabot.yml`. One-shot, run from the implementation plan; not added as a recurring CI job.
2. Confirm the diff is `dependabot.yml`-only — no incidental edits.

**Post-merge** (Monday after merge):

3. The first Monday-06:00-UTC scheduled run produces ≤5 grouped PRs per ecosystem.
4. Each PR's title and squash-merge commit use the `chore(deps):` prefix (acceptable form: `chore(deps)(deps): …` or `chore(deps)(cargo): …`; see §8.1).
5. Each PR carries the `area:deps` label and `SMK1085` as assignee.
6. `gh api /repos/SMK1085/paigasus-helikon | jq .security_and_analysis` confirms Dependabot alerts, security updates, and grouped security updates are all `enabled` (§5).

If criteria 3–5 fail on the first Monday, the implementation plan includes a small follow-up checklist (review the Dependabot logs in the Insights tab, confirm the file parsed, adjust grouping). If criterion 6 is `disabled`, the operator toggles it via Settings.

## 11. Post-implementation findings

### 11.1 `include: scope` dropped — doubled prefix was uglier than predicted

The first Dependabot run after merge produced PR #9 with the title `chore(deps)(deps): Bump the gh-actions group with 2 updates`. Two findings:

1. **§6's predicted output was wrong about the scope contents.** The doc claimed `include: scope` would inject the *ecosystem* name (`(cargo)`, `(github-actions)`), so the example showed `chore(deps)(cargo): …`. Actual Dependabot behavior is that `include: scope` always injects the literal token `deps`, regardless of ecosystem — hence `chore(deps)(deps): …` for *both* cargo and github-actions PRs. The ecosystem distinction surfaces only in the group name within the title body (e.g., "Bump the gh-actions group").
2. **The doubled prefix was net-negative.** It carried no information (`deps` twice says nothing the single `chore(deps):` doesn't already say) and added visual noise. Per §7's contingency, we removed `include: "scope"` from both ecosystems. Future Dependabot PRs use the clean `chore(deps): …` form.

§3.3 and §4 of this doc were updated to drop the `commit-message.include` row and the `include: "scope"` line, respectively. §6, §7, and §8.1 are left as the historical design-time analysis — they document *why* we initially accepted the doubled-parens form, even though the contingency in §7 was eventually triggered.

Fix landed in [PR #11](https://github.com/SMK1085/paigasus-helikon/pull/11) (commit `chore(deps): drop dependabot include: scope to fix doubled prefix`, 2026-05-17), opened immediately after SMA-308's main PR #8 merged.

### 11.2 Still pending

- Whether subsequent Monday runs match the ≤5-grouped-PRs-per-ecosystem and `area:deps` label/assignee predictions (need at least one full week).
- Whether any group pattern misses an obvious dep (likely none until tower/hyper/reqwest land in a future ticket).
- Whether CODEOWNERS is introduced separately and the inline `assignees` is migrated there.
- Whether SMA-335's eventual PR-title linter accepts `chore(deps): bump …` cleanly (should be straightforward now that the form is canonical).
