# SMA-308 Grouped Weekly Dependabot Updates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the SMA-306 placeholder `.github/dependabot.yml` with the SMA-308 steady-state grouped configuration (two ecosystems, six groups, `chore(deps):` prefix, `area:deps` label, hardcoded assignee), schema-validated locally before opening the PR.

**Architecture:** Single-file change. The new YAML is fully defined in the design doc (§4); the work here is to (1) prove the local schema check works against the current file, (2) write the new file, (3) confirm it still validates, (4) commit, push, open PR, (5) document post-merge observation.

**Tech Stack:** GitHub Dependabot v2 (`.github/dependabot.yml`), `check-jsonschema` via `uvx` (already installed at `/opt/homebrew/bin/uvx`), `gh` CLI for PR + post-merge verification.

**Reference:** [Design doc](../specs/2026-05-17-sma-308-grouped-dependabot-design.md). When in doubt about a config detail, the design doc is authoritative.

**Branch state at start:** `feature/sma-308-grouped-weekly-dependabot-updates` is already checked out and the design doc commit `9d54b65 docs(specs): SMA-308 grouped weekly Dependabot updates design` is at HEAD.

---

## Task 1: Preflight — verify schema validator works against current file

**Files:**
- Inspect: `.github/dependabot.yml` (current SMA-306 placeholder; do not modify in this task)

This task proves the validator tool runs on this machine and that the upstream schema URL is reachable, so a failure in Task 2 can be attributed to the new YAML rather than the toolchain.

- [ ] **Step 1: Confirm `uvx` is on PATH**

Run: `uvx --version`
Expected: prints a version like `uvx 0.11.7 (...)`. If "command not found", install via `brew install uv` (macOS) or follow <https://docs.astral.sh/uv/getting-started/installation/>.

- [ ] **Step 2: Validate the existing `dependabot.yml` against the upstream schema**

Run:
```bash
uvx check-jsonschema \
  --schemafile https://json.schemastore.org/dependabot-2.0.json \
  .github/dependabot.yml
```

Expected:
```
ok -- validation done
```

(Exact wording may vary by `check-jsonschema` version; exit code `0` is what matters.)

If this step fails, **stop and investigate** — the upstream schema URL may have moved (check <https://www.schemastore.org/json/> for `dependabot-2.0.json`) or `uvx` may be unable to fetch packages. Do not proceed to Task 2 until this passes.

- [ ] **Step 3: Capture the validator's exit code as the baseline**

Run: `echo $?`
Expected: `0`

The Task 3 re-validation must also exit `0`.

No commit in this task — read-only verification.

---

## Task 2: Rewrite `.github/dependabot.yml`

**Files:**
- Modify: `.github/dependabot.yml` (full rewrite — replaces the SMA-306 placeholder)

- [ ] **Step 1: Replace the file contents in full**

Overwrite `.github/dependabot.yml` with exactly this content (copied verbatim from the design doc §4 — preserve the blank line between the two `updates:` entries):

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
      include: "scope"
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
      include: "scope"
    labels: ["area:deps"]
    assignees: ["SMK1085"]
    groups:
      gh-actions:
        patterns: ["*"]
```

- [ ] **Step 2: Re-run the schema validator against the new file**

Run:
```bash
uvx check-jsonschema \
  --schemafile https://json.schemastore.org/dependabot-2.0.json \
  .github/dependabot.yml
```

Expected: exit `0`, same "ok" message as Task 1 Step 2.

If validation fails: read the error, compare the file against the §4 YAML byte-for-byte (indentation, quoting), fix, re-run. Common gotchas:
- `update-types` must be a YAML list, not a single string.
- Group names use hyphens (`rust-minor-patch`), keys are kebab-case.
- `version: 2` at top level is required (not `version: "2"`).

- [ ] **Step 3: Confirm the diff is `dependabot.yml`-only**

Run: `git status --short`
Expected:
```
 M .github/dependabot.yml
```

No untracked files, no other modifications. If anything else is dirty, investigate before committing.

- [ ] **Step 4: Inspect the diff visually**

Run: `git diff .github/dependabot.yml`

Expected: full replacement of the two SMA-306 group blocks (`cargo-minor-and-patch`, `actions-minor-and-patch`) with the six groups from §4. The `commit-message.prefix` for the actions ecosystem changes from `chore(ci)` to `chore(deps)`. The actions `labels` changes from `area:ci` to `area:deps`. Both ecosystems gain `assignees: ["SMK1085"]`. Cargo `open-pull-requests-limit` drops from `10` to `5`.

- [ ] **Step 5: Commit**

```bash
git add .github/dependabot.yml
git commit -m "$(cat <<'EOF'
chore(deps): SMA-308 group weekly Dependabot updates

Replace the SMA-306 placeholder dependabot.yml with the steady-state
grouped configuration: two ecosystems, six groups, majors split from
the catch-all, tokio/tracing/serde stacks kept lockstep, chore(deps):
prefix unified across both ecosystems, area:deps label, SMK1085 as
hardcoded assignee. Schema-validated locally via check-jsonschema.

See docs/superpowers/specs/2026-05-17-sma-308-grouped-dependabot-design.md
EOF
)"
```

Expected: commit succeeds with no hook failures.

Per CLAUDE.md, the `chore(...)` type is correct here — release-plz treats `chore` as non-bumping, which is what we want for a config-only change.

---

## Task 3: Push branch and open PR

**Files:** none modified

- [ ] **Step 1: Push the branch**

Run: `git push -u origin feature/sma-308-grouped-weekly-dependabot-updates`
Expected: push succeeds; the remote prints a "create PR" URL.

- [ ] **Step 2: Open the PR via `gh`**

Run:
```bash
gh pr create \
  --base main \
  --title "chore(deps): SMA-308 group weekly Dependabot updates" \
  --body "$(cat <<'EOF'
## Summary

Replaces the SMA-306 placeholder `.github/dependabot.yml` with the SMA-308 steady-state grouped configuration.

- Two ecosystems: `cargo` and `github-actions`.
- Six groups total: `tokio-stack`, `tracing-otel`, `serde-stack`, `rust-major`, `rust-minor-patch` (cargo) + `gh-actions` (actions).
- Majors split out from the catch-all into their own PRs for explicit review.
- Lockstep crate families (`tokio*`/`tower*`/`hyper*`/`reqwest*`, `tracing*`/`opentelemetry*`, `serde*`/`schemars`/`serde_with*`) kept together regardless of bump size.
- `chore(deps):` commit prefix unified across both ecosystems (was `chore(ci)` for actions).
- `area:deps` label on both ecosystems (was `area:ci` for actions).
- Open-PR cap: 5 per ecosystem (was 10/5).
- Assignee: `SMK1085` inline (no CODEOWNERS dependency — see design doc §1 non-goals).
- Timezone: `Etc/UTC` retained (deliberate deviation from ticket's `Europe/Vienna` — see design doc §9 decisions log).

Design: [`docs/superpowers/specs/2026-05-17-sma-308-grouped-dependabot-design.md`](docs/superpowers/specs/2026-05-17-sma-308-grouped-dependabot-design.md)
Plan: [`docs/superpowers/plans/2026-05-17-sma-308-grouped-dependabot.md`](docs/superpowers/plans/2026-05-17-sma-308-grouped-dependabot.md)
Linear: [SMA-308](https://linear.app/smaschek/issue/SMA-308/grouped-weekly-dependabot-updates)

## Test plan

- [x] Local schema validation: `uvx check-jsonschema --schemafile https://json.schemastore.org/dependabot-2.0.json .github/dependabot.yml` exits 0.
- [ ] CI: `fmt`, `clippy`, `test`, `docs`, `doc-coverage`, `audit`, `deny` all green (this PR touches no Rust code or workflows, but the gates still run).
- [ ] Post-merge (next Monday 06:00 UTC): ≤5 grouped PRs per ecosystem; each PR uses `chore(deps):` prefix, carries `area:deps` label, and assigns `SMK1085`. Tracked separately — not a blocker for merging this PR.
- [ ] Post-merge: confirm via `gh api /repos/SMK1085/paigasus-helikon | jq .security_and_analysis` that Dependabot alerts, security updates, and grouped security updates are all enabled. Toggle via Settings → Code security and analysis if any are off.

Per CLAUDE.md, do not auto-close SMA-308 from this PR — status moves manually after review.
EOF
)"
```

Expected: PR is created; `gh` prints the PR URL.

- [ ] **Step 3: Capture the PR URL**

Run: `gh pr view --json url --jq .url`
Expected: a URL like `https://github.com/SMK1085/paigasus-helikon/pull/<N>`. Note the PR number for post-merge follow-up.

- [ ] **Step 4: Wait for CI to start**

Run: `gh pr checks --watch` (Ctrl-C once all checks are queued; or let it run to completion).

Expected: the five required-status-check IDs from CLAUDE.md eventually report success — `ci / fmt`, `ci / clippy`, `ci / test (ubuntu-latest, stable)`, `ci / docs`, `ci / doc-coverage`. The `audit / audit` and `deny / deny` checks from SMA-306 also run. `sbom / sbom` does **not** run (tag-only trigger).

None of these checks consume `dependabot.yml`, so they should pass identically to a no-op PR. If any fail, the failure is unrelated to this change — investigate, but don't assume the dependabot.yml caused it.

---

## Task 4: Post-merge verification (after the PR lands)

This task runs **after** the PR is merged to `main`. It exists so the implementer doesn't forget the observational acceptance criteria from the ticket.

**Files:** none

- [ ] **Step 1: Verify Dependabot parsed the new file**

After the merge commit hits `main`, browse to <https://github.com/SMK1085/paigasus-helikon/network/updates>. There should be one "configuration" entry per ecosystem (cargo, github-actions) with no parse errors. If GitHub displays a red banner "Dependabot encountered an error parsing your config", read the error, open a follow-up PR to fix it.

- [ ] **Step 2: Confirm security-and-analysis settings**

Run:
```bash
gh api /repos/SMK1085/paigasus-helikon | jq .security_and_analysis
```

Expected (each of these `status` fields should be `"enabled"`):
- `dependabot_alerts` (or absence of the key — it's enabled by default for public repos)
- `dependabot_security_updates`
- `secret_scanning` and `secret_scanning_push_protection` (carried over from SMA-306)

If any of `dependabot_alerts` or `dependabot_security_updates` is `"disabled"`, toggle on via Settings → Code security and analysis.

For "Grouped security updates" (newer toggle, may not be in the API yet), verify visually at Settings → Code security and analysis → "Grouped security updates for Dependabot" → **on**.

- [ ] **Step 3: Mark SMA-308 as done in Linear**

Per CLAUDE.md, the PR merge does **not** auto-close the Linear issue. Move SMA-308 manually from "In Review" to "Done" once Steps 1–2 confirm the configuration is live and parseable.

- [ ] **Step 4: Observe the first Monday's run (deferred)**

This is a post-deployment observation, not a blocking step. On the first Monday-06:00-UTC after merge:

- Browse to <https://github.com/SMK1085/paigasus-helikon/pulls?q=is%3Apr+author%3Aapp%2Fdependabot>.
- Confirm: ≤5 PRs per ecosystem this week, each titled `chore(deps)(deps): …` or `chore(deps)(cargo): …`, each labeled `area:deps`, each assigned to `SMK1085`.

If criteria fail, open a follow-up ticket (not a hotfix on this PR): the most likely cause is a group-pattern mismatch, fixable in a one-line edit.

- [ ] **Step 5: Update the design doc's §11 with post-implementation findings**

Once the first Monday's run is observed (and any reactive fixes have landed), open a small follow-up PR that fills in §11 of `docs/superpowers/specs/2026-05-17-sma-308-grouped-dependabot-design.md` with concrete findings — same pattern as `b072358 docs(specs): SMA-307 add post-implementation findings (§11)`. Use commit type `docs(specs):`.

---

## Self-review notes

- **Spec coverage check:** Each §1–§10 requirement of the design doc is implemented by a task above: §4 YAML → Task 2 Step 1; §10 pre-merge schema check → Task 1 + Task 2 Step 2; §5 security-updates toggle verification → Task 4 Step 2; §10 post-merge observation → Task 4 Step 4. §11 placeholder is closed out by Task 4 Step 5.
- **Placeholder scan:** All step bodies contain concrete commands, exact filenames, or exact YAML. No "TBD", "TODO", "implement later", or "similar to above". §11 of the design doc is itself a deliberate forward-placeholder, closed by Task 4 Step 5.
- **Consistency check:** Branch name, commit-message types (`docs(specs)`, `chore(deps)`), label names (`area:deps`), assignee (`SMK1085`), and group names match between this plan, the spec, and the existing `dependabot.yml` baseline.
