# SMA-479 Supply-Chain Gate Severity Alignment — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the daily cron evaluate `main` at exactly the severity the PR gate uses, so an advisory that reddens every PR cannot leave `main` reporting green.

**Architecture:** No new CI machinery. The strict `audit` job loses its `if: github.event_name != 'schedule'` gate so the *same job definition* serves PRs and the cron — severity drift becomes structurally impossible rather than merely discouraged. `deny.yml` gains the cron it never had. Both workflows get their concurrency groups keyed on `github.event_name` (without which a queued cron run is cancelled by the next merge to `main`), get their actions SHA-pinned to match the rest of the repo, and `deny` gains a guard asserting the two advisory-ignore policy files have not drifted apart.

**Tech Stack:** GitHub Actions workflow YAML, Bash 3.2+ (macOS ships 3.2 — no `mapfile`, no associative arrays), `cargo-audit` 0.22.x, `cargo-deny` 0.19.x, `actionlint`.

**Spec:** `docs/superpowers/specs/2026-08-04-sma-479-audit-severity-alignment-design.md`

## Global Constraints

- **Branch:** `feature/sma-479-align-audityml-severity-scheduled-audit-reports-green-on`. Never commit to `main`.
- **Commit format:** `<type>(<scope>): SMA-479 <lowercase subject>`. Types and scopes are gated by `.versionrc` and enforced by a local `commit-msg` hook running `convco`. Valid scopes for this work: `workflows`, `ci`, `claude`, `contributing`, `docs`, `repo`, `plan`. **`release-plz` is NOT a valid scope.**
- **Commit type must be `ci(...)`, `chore(...)`, or `docs(...)` — never `feat`/`fix`.** release-plz parses commits since each per-crate tag; a `feat`/`fix` touching workspace files would attribute version bumps to crates. No crate source is touched by this plan, so no crate version changes.
- **Never `git add -A`.** `.env` and `.claude/` are untracked but **not** gitignored in this repo; `-A` would stage secrets. Stage explicit paths only, and verify with `git show --stat` after each commit.
- **Commits are signed via a 1Password SSH key.** If a commit fails with `failed to fill whole buffer`, the vault is locked — ask the user to unlock, then retry. Do not bypass signing with `--no-gpg-sign`.
- **`git push` triggers a pre-push hook** that runs `cargo fmt --all -- --check` and a full-workspace `cargo clippy --all-features --all-targets`. This takes 5+ minutes and looks like a hang. It is not a hang, and it is not an SSH problem. Either background the push or use `--no-verify` (safe here: this plan touches no Rust source, so fmt and clippy cannot be affected).
- **Action pinning:** every `uses:` line must reference a commit SHA with a human-readable `# <owner>/<action> vX.Y.Z` comment on the line above. The exact SHAs to use are given verbatim in each task, resolved 2026-08-04.
- **Do not modify** `.github/rulesets/main-protection-checks.json`, `ci.yml`, `msrv.yml`, `sbom.yml`, `deny.toml`, or `.cargo/audit.toml`. Job names must stay `audit`, `scheduled-audit`, and `deny` — they are required status-check contexts.

## File Structure

| File | Responsibility |
| --- | --- |
| `scripts/check-advisory-ignore-sync.sh` | **new** — single-purpose guard: the `[advisories].ignore` lists in `.cargo/audit.toml` and `deny.toml` are identical. No other logic. |
| `.github/workflows/audit.yml` | cargo-audit gate on all four events + the receipt-filing cron job. |
| `.github/workflows/deny.yml` | cargo-deny gate on all four events + the ignore-sync guard. |
| `CLAUDE.md` | agent-facing record of both jobs' tools *and* severity behaviour. |
| `CONTRIBUTING.md` | contributor-facing description of the supply-chain gates. |

## Prerequisite

`actionlint` is used in Tasks 2 and 3 and is not installed. Install it once before starting:

```bash
brew install actionlint
actionlint --version
```

If `brew` is unavailable, download a release binary from `https://github.com/rhysd/actionlint/releases`. Do **not** skip it — it is the only pre-merge evidence that `deny.yml`'s brand-new `on: schedule:` block parses. A malformed trigger block fails **open and silently**: PRs keep passing and the cron simply never fires.

---

### Task 1: Advisory ignore-list sync guard

`.cargo/audit.toml` and `deny.toml` carry byte-identical three-entry ignore lists kept in sync by prose policy and nothing else. Task 3 adds a daily cron to `deny.yml`, after which both files are evaluated daily against the same advisory database — so a one-line drift would show `audit` green beside `deny` red on `main`, every day, until noticed.

**Files:**
- Create: `scripts/check-advisory-ignore-sync.sh`
- Read-only inputs: `.cargo/audit.toml`, `deny.toml`

**Interfaces:**
- Consumes: nothing.
- Produces: an executable at repo-relative path `scripts/check-advisory-ignore-sync.sh`, invoked by Task 3 as `./scripts/check-advisory-ignore-sync.sh`. Exit 0 = lists agree; exit 1 = lists differ; exit 2 = a policy file is missing. Resolves its own paths from `BASH_SOURCE`, so it is correct from any working directory.

- [ ] **Step 1: Confirm the current state the guard must accept**

Both files must currently agree — if they do not, stop and report, because the guard would then fail on a pre-existing condition rather than on drift this plan introduces.

```bash
grep -oE '"RUSTSEC-[0-9]{4}-[0-9]{4}"' .cargo/audit.toml | tr -d '"' | sort -u
echo "---"
grep -oE '"RUSTSEC-[0-9]{4}-[0-9]{4}"' deny.toml | tr -d '"' | sort -u
```

Expected: both print exactly these three IDs, and nothing else.

```
RUSTSEC-2024-0384
RUSTSEC-2024-0436
RUSTSEC-2025-0012
```

**Why the `"` quotes in that pattern are load-bearing:** both files mention `RUSTSEC-2023-0071` and `RUSTSEC-2025-0052` in *unquoted* prose comments, as historical notes about entries that were removed. Matching unquoted IDs would pick those up and — because both files carry the same comment — report permanent false agreement. The guard would then never fire. Verify this for yourself before continuing:

```bash
grep -oE 'RUSTSEC-[0-9]{4}-[0-9]{4}' .cargo/audit.toml | sort -u | wc -l   # 5 — WRONG, includes comments
grep -oE '"RUSTSEC-[0-9]{4}-[0-9]{4}"' .cargo/audit.toml | sort -u | wc -l # 3 — correct
```

- [ ] **Step 2: Write the script**

This exact script was smoke-tested during planning against copies of the real policy files — baseline pass, drift in each direction, and invocation from a foreign working directory all behaved as documented below. If it misbehaves for you, suspect a transcription error before suspecting the logic.

Create `scripts/check-advisory-ignore-sync.sh`:

```bash
#!/usr/bin/env bash
# check-advisory-ignore-sync.sh — assert the RustSec advisory ignore lists in
# .cargo/audit.toml and deny.toml are identical.
#
# cargo-audit (the `audit` gate) and cargo-deny (the `deny` gate) each read
# their own policy file. Both are required status checks, and since SMA-479
# both are evaluated daily against the same advisory database — so a one-line
# drift between the two ignore lists surfaces as one gate green and the other
# red on `main`, every day, until someone notices.
#
# Matching is deliberately restricted to *quoted* advisory IDs. Both files
# carry unquoted RUSTSEC-* IDs inside comments, as historical notes about
# entries that were removed; matching those would report permanent false
# agreement and the guard would never fire. Known limitation: a quoted
# advisory ID inside a comment is a false positive. Accepted — see
# docs/superpowers/specs/2026-08-04-sma-479-audit-severity-alignment-design.md

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
audit_toml="${repo_root}/.cargo/audit.toml"
deny_toml="${repo_root}/deny.toml"

for f in "${audit_toml}" "${deny_toml}"; do
  if [[ ! -f "${f}" ]]; then
    echo "error: policy file not found: ${f}" >&2
    exit 2
  fi
done

extract_ids() {
  # `|| true` keeps `set -e` from aborting when a file has zero entries —
  # an empty ignore list is legitimate, and must compare equal to an empty one.
  grep -oE '"RUSTSEC-[0-9]{4}-[0-9]{4}"' "$1" | tr -d '"' | sort -u || true
}

audit_ids="$(extract_ids "${audit_toml}")"
deny_ids="$(extract_ids "${deny_toml}")"

if [[ "${audit_ids}" == "${deny_ids}" ]]; then
  count="$(printf '%s' "${audit_ids}" | grep -c . || true)"
  echo "advisory ignore lists agree (${count} entries)"
  exit 0
fi

{
  echo "error: advisory ignore lists differ between .cargo/audit.toml and deny.toml"
  echo
  only_audit="$(comm -23 <(printf '%s\n' "${audit_ids}") <(printf '%s\n' "${deny_ids}") | grep -c . || true)"
  if [[ "${only_audit}" != "0" ]]; then
    echo "  only in .cargo/audit.toml:"
    comm -23 <(printf '%s\n' "${audit_ids}") <(printf '%s\n' "${deny_ids}") | sed 's/^/    /'
  fi
  only_deny="$(comm -13 <(printf '%s\n' "${audit_ids}") <(printf '%s\n' "${deny_ids}") | grep -c . || true)"
  if [[ "${only_deny}" != "0" ]]; then
    echo "  only in deny.toml:"
    comm -13 <(printf '%s\n' "${audit_ids}") <(printf '%s\n' "${deny_ids}") | sed 's/^/    /'
  fi
  echo
  echo "Both files must carry the same [advisories].ignore entries."
  echo "See CONTRIBUTING.md -> Supply-chain security."
} >&2

exit 1
```

- [ ] **Step 3: Make it executable and run it — expect PASS**

```bash
chmod +x scripts/check-advisory-ignore-sync.sh
./scripts/check-advisory-ignore-sync.sh; echo "exit=$?"
```

Expected output exactly:

```
advisory ignore lists agree (3 entries)
exit=0
```

- [ ] **Step 4: Prove it fails when the lists drift**

A guard only ever observed passing is not evidence. Break it deliberately, in each direction.

```bash
# Direction A: entry missing from deny.toml
sed -i.bak '/"RUSTSEC-2024-0436",/d' deny.toml
./scripts/check-advisory-ignore-sync.sh; echo "exit=$?"
```

Expected: exit=1, stderr naming `RUSTSEC-2024-0436` under `only in .cargo/audit.toml:`.

```bash
mv deny.toml.bak deny.toml
# Direction B: entry missing from .cargo/audit.toml
sed -i.bak '/"RUSTSEC-2024-0436",/d' .cargo/audit.toml
./scripts/check-advisory-ignore-sync.sh; echo "exit=$?"
```

Expected: exit=1, stderr naming `RUSTSEC-2024-0436` under `only in deny.toml:`.

- [ ] **Step 5: Restore both files and confirm a clean tree**

```bash
mv .cargo/audit.toml.bak .cargo/audit.toml
./scripts/check-advisory-ignore-sync.sh; echo "exit=$?"     # back to: agree (3 entries), exit=0
git status --short
```

`git status --short` must show **only** the new untracked script. If it shows `.cargo/audit.toml` or `deny.toml` as modified, a `.bak` restore was missed — fix with `git checkout -- .cargo/audit.toml deny.toml` before continuing. Also confirm no stray backups remain:

```bash
ls .cargo/*.bak deny.toml.bak 2>/dev/null && echo "STRAY BACKUPS — delete them" || echo "clean"
```

- [ ] **Step 6: Commit**

```bash
git add scripts/check-advisory-ignore-sync.sh
git commit -m "ci(workflows): SMA-479 add advisory ignore-list sync guard"
git show --stat --oneline HEAD
```

`git show --stat` must list exactly one file. If it lists more, the commit staged something unintended — reset and redo with explicit paths.

---

### Task 2: `audit.yml` — un-gate the strict job, fix concurrency, pin actions

**Files:**
- Modify: `.github/workflows/audit.yml` (whole file replaced — it is 47 lines)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: job names `audit` and `scheduled-audit`, unchanged — both are required status-check contexts declared in `.github/rulesets/main-protection-checks.json`. Renaming either breaks branch protection.

- [ ] **Step 1: Read the current file and confirm it matches the expected baseline**

```bash
cat .github/workflows/audit.yml
```

Confirm the `audit` job carries `if: github.event_name != 'schedule'` and the concurrency group is `audit-${{ github.workflow }}-${{ github.ref }}`. If either differs, stop and report — the file has changed since planning.

- [ ] **Step 2: Replace the file contents**

Write `.github/workflows/audit.yml` exactly as follows. The SHAs were resolved 2026-08-04; re-verify them with the commands in Step 3 before committing.

```yaml
name: audit

on:
  push:
    branches: [main]
  pull_request:
  schedule:
    - cron: "0 6 * * *"   # daily, 06:00 UTC
  workflow_dispatch:

# The group is keyed on github.event_name as well as github.ref because
# `schedule`, `workflow_dispatch`, and `push` to main all resolve github.ref to
# refs/heads/main. Sharing one group with cancel-in-progress: false means a
# queued cron run goes *pending*, and GitHub cancels a pending run when the next
# run enters the group — so the next merge to main would silently discard the
# day's only strict evaluation of main, leaving a `cancelled` row that is
# neither success nor failure. Do not "simplify" this key back (SMA-479).
concurrency:
  group: audit-${{ github.workflow }}-${{ github.ref }}-${{ github.event_name }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

permissions:
  contents: read

env:
  CARGO_TERM_COLOR: always

jobs:
  # The strict gate. Deliberately runs on EVERY event, including the daily cron:
  # the daily signal and the PR gate are the same job definition, so their
  # severity cannot drift apart. Do not re-add an event filter here (SMA-479).
  audit:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      # actions/checkout v7.0.1
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
        with:
          persist-credentials: false
      # dtolnay/rust-toolchain master (no tagged releases)
      - uses: dtolnay/rust-toolchain@2c7215f132e9ebf062739d9130488b56d53c060c
        with:
          toolchain: stable
      # Swatinem/rust-cache v2.9.1
      - uses: Swatinem/rust-cache@c19371144df3bb44fab255c43d04cbc2ab54d1c4
        with:
          cache-directories: "~/.cargo/advisory-db"
      # taiki-e/install-action v2.85.7
      - uses: taiki-e/install-action@67729d5c413db75907f0ad1e39bb04b9c868ff60
        with:
          tool: cargo-audit
      # cargo-audit fetches the advisory DB on every run unless -n/--no-fetch,
      # so the cache above only makes that fetch incremental — never stale.
      - run: cargo audit --deny warnings

  # Receipt-filer ONLY. rustsec/audit-check routes `schedule` events to its
  # reportIssues() path, which files GitHub issues and returns without ever
  # failing — at ANY severity, including critical vulnerabilities. This job
  # going green means nothing; the `audit` job above is the verdict.
  #
  # Do NOT widen this condition to also run on workflow_dispatch. A non-schedule
  # event routes the action to reportCheck(), which calls the Checks API and
  # requires `checks: write`; this job grants only `issues: write`, so it 403s.
  scheduled-audit:
    if: github.event_name == 'schedule'
    runs-on: ubuntu-latest
    permissions:
      contents: read
      issues: write
    steps:
      # actions/checkout v7.0.1
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
        with:
          persist-credentials: false
      # rustsec/audit-check v2.0.0
      - uses: rustsec/audit-check@69366f33c96575abad1ee0dba8212993eecbe998
        with:
          token: ${{ secrets.GITHUB_TOKEN }}
```

**Note the `toolchain: stable` input.** `dtolnay/rust-toolchain` normally reads the toolchain from its *ref name* (`@stable`). Pinning to a SHA destroys that signal, so the input is mandatory — without it this is a silent behaviour change, not a no-op. `ci.yml` does the same thing at every call site.

- [ ] **Step 3: Re-verify the pinned SHAs are still current**

```bash
for r in actions/checkout Swatinem/rust-cache taiki-e/install-action rustsec/audit-check; do
  tag=$(gh api repos/$r/releases/latest --jq '.tag_name')
  ref=$(gh api repos/$r/git/ref/tags/$tag --jq '.object.type + " " + .object.sha')
  typ=${ref%% *}; sha=${ref##* }
  if [ "$typ" = "tag" ]; then sha=$(gh api repos/$r/git/tags/$sha --jq '.object.sha'); fi
  echo "$r  $tag  $sha"
done
gh api repos/dtolnay/rust-toolchain/commits/master --jq '.sha'
```

Expected (as of 2026-08-04):

```
actions/checkout  v7.0.1  3d3c42e5aac5ba805825da76410c181273ba90b1
Swatinem/rust-cache  v2.9.1  c19371144df3bb44fab255c43d04cbc2ab54d1c4
taiki-e/install-action  v2.85.7  67729d5c413db75907f0ad1e39bb04b9c868ff60
rustsec/audit-check  v2.0.0  69366f33c96575abad1ee0dba8212993eecbe998
2c7215f132e9ebf062739d9130488b56d53c060c
```

If any SHA differs, a newer release shipped since planning: use the **fresh** SHA and update the `# action vX.Y.Z` comment to match. `CLAUDE.md` explicitly forbids using a stale plan-time pin.

- [ ] **Step 4: Lint the file**

```bash
actionlint .github/workflows/audit.yml
```

Expected: no output, exit 0. Any error here is a real YAML or expression bug — fix it before proceeding.

- [ ] **Step 5: Verify the gate is gone and nothing else regressed**

```bash
grep -c "github.event_name != 'schedule'" .github/workflows/audit.yml   # must be 0
grep -c "event_name }}" .github/workflows/audit.yml                     # must be 1 (concurrency key)
grep -c "if: github.event_name == 'schedule'" .github/workflows/audit.yml # must be 1 (scheduled-audit)
grep -E "uses:.*@(v[0-9]|stable|main|master)$" .github/workflows/audit.yml && echo "UNPINNED TAG FOUND" || echo "all pinned"
grep -c "persist-credentials: false" .github/workflows/audit.yml         # must be 2
```

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/audit.yml
git commit -m "ci(workflows): SMA-479 run the strict audit job on the daily cron"
git show --stat --oneline HEAD
```

---

### Task 3: `deny.yml` — add the cron, wire the sync guard, pin actions

**Files:**
- Modify: `.github/workflows/deny.yml` (whole file replaced — it is 30 lines)

**Interfaces:**
- Consumes: `scripts/check-advisory-ignore-sync.sh` from Task 1, invoked as `./scripts/check-advisory-ignore-sync.sh`. **Task 1 must be committed before this task runs**, or CI fails on a missing file.
- Produces: job name `deny`, unchanged — a required status-check context.

- [ ] **Step 1: Confirm Task 1 landed**

```bash
test -x scripts/check-advisory-ignore-sync.sh && echo "present and executable" || echo "MISSING — do Task 1 first"
git log --oneline -1 -- scripts/check-advisory-ignore-sync.sh
```

The script must be **committed**, not merely present: CI checks out the commit, not your working tree.

- [ ] **Step 2: Replace the file contents**

Write `.github/workflows/deny.yml` exactly as follows:

```yaml
name: deny

on:
  push:
    branches: [main]
  pull_request:
  schedule:
    - cron: "17 6 * * *"  # daily, 06:17 UTC — offset from audit.yml's 06:00
  workflow_dispatch:

# Keyed on github.event_name for the same reason as audit.yml: schedule,
# workflow_dispatch, and push to main all share refs/heads/main, and a shared
# group with cancel-in-progress: false lets a merge cancel a pending cron run
# (SMA-479).
concurrency:
  group: deny-${{ github.workflow }}-${{ github.ref }}-${{ github.event_name }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

permissions:
  contents: read

env:
  CARGO_TERM_COLOR: always

jobs:
  deny:
    runs-on: ubuntu-latest
    steps:
      # actions/checkout v7.0.1
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
        with:
          persist-credentials: false
      # Fails fast, before the expensive toolchain + install steps. deny.toml
      # and .cargo/audit.toml must carry identical [advisories].ignore lists;
      # both are now evaluated daily against the same DB, so drift shows up as
      # one gate green and the other red on main (SMA-479).
      - name: Check advisory ignore lists are in sync
        run: ./scripts/check-advisory-ignore-sync.sh
      # dtolnay/rust-toolchain master (no tagged releases)
      - uses: dtolnay/rust-toolchain@2c7215f132e9ebf062739d9130488b56d53c060c
        with:
          toolchain: stable
      # Swatinem/rust-cache v2.9.1
      - uses: Swatinem/rust-cache@c19371144df3bb44fab255c43d04cbc2ab54d1c4
        with:
          cache-directories: "~/.cargo/advisory-dbs"
      # taiki-e/install-action v2.85.7
      - uses: taiki-e/install-action@67729d5c413db75907f0ad1e39bb04b9c868ff60
        with:
          tool: cargo-deny
      # cargo-deny fetches the advisory DB unless -d/--disable-fetch or the
      # global --offline is passed; we pass neither, so the cache above only
      # makes that fetch incremental.
      - run: cargo deny --all-features check
```

**Note the cache path differs from `audit.yml`.** cargo-deny uses `~/.cargo/advisory-dbs` (plural, per `deny.toml`'s `db-path`); cargo-audit uses `~/.cargo/advisory-db` (singular). Each tool caches its own — do not "unify" them.

- [ ] **Step 3: Lint the file**

```bash
actionlint .github/workflows/deny.yml
```

Expected: no output, exit 0.

This is the single most important step in the task. `deny.yml`'s `on: schedule:` block is brand new; if it is mistyped or mis-indented, GitHub silently never fires it while every PR continues to pass. There is no other pre-merge signal for this.

- [ ] **Step 4: Verify the trigger block parses as intended**

Independently of actionlint, confirm the structure with a YAML parser. Note the quotes around `on:` — YAML 1.1 parses a bare `on` key as the boolean `true`, so it must be indexed as `True` in Python.

```bash
python3 -c "
import yaml
d = yaml.safe_load(open('.github/workflows/deny.yml'))
on = d.get(True) or d.get('on')
print('triggers:', sorted(on.keys()))
print('cron:', on['schedule'])
"
```

Expected exactly:

```
triggers: ['pull_request', 'push', 'schedule', 'workflow_dispatch']
cron: [{'cron': '17 6 * * *'}]
```

If `schedule` or `workflow_dispatch` is absent from that list, the block is wrong regardless of what actionlint said.

- [ ] **Step 5: Verify pinning and the sync step**

```bash
grep -E "uses:.*@(v[0-9]|stable|main|master)$" .github/workflows/deny.yml && echo "UNPINNED TAG FOUND" || echo "all pinned"
grep -c "check-advisory-ignore-sync.sh" .github/workflows/deny.yml   # must be 1
grep -c "persist-credentials: false" .github/workflows/deny.yml      # must be 1
```

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/deny.yml
git commit -m "ci(workflows): SMA-479 add a daily cron and ignore-sync guard to deny"
git show --stat --oneline HEAD
```

---

### Task 4: Documentation

Acceptance criterion 3 of the ticket. Both files currently describe the daily run in a way that omits the fact that it cannot fail.

**Files:**
- Modify: `CLAUDE.md` — the supply-chain paragraph is **line 105**, a single long line with blank lines at 104 and 106. Replacing it is therefore an unambiguous one-line-in, four-paragraphs-out edit.
- Modify: `CONTRIBUTING.md` — the `audit` bullet is lines **247-249**, the `deny` bullet lines **250-252**. Leave line 245 (`Three workflows complement CI…`) and the `sbom` bullet at 253+ untouched.

**Interfaces:**
- Consumes: the final shape of both workflows from Tasks 2 and 3. Run this task last so the prose describes what actually shipped.
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Locate the exact `CLAUDE.md` paragraph**

```bash
grep -n "Supply-chain workflows" CLAUDE.md
```

It begins `Supply-chain workflows (\`.github/workflows/audit.yml\`, \`deny.yml\`, \`sbom.yml\`) are separate from \`ci.yml\`…` and ends `…these are the only places in the repo where a wrapper action is preferred over direct tool invocation.`

- [ ] **Step 2: Replace that whole paragraph in `CLAUDE.md`**

```markdown
Supply-chain workflows (`.github/workflows/audit.yml`, `deny.yml`, `sbom.yml`) are separate from `ci.yml` because they have independent triggers and failure semantics. Required status checks added in SMA-306: `audit`, `deny` (declared in `.github/rulesets/main-protection-checks.json` alongside the CI gates). Both `audit.yml` and `deny.yml` run on **push to `main`, PRs, a daily cron, and `workflow_dispatch`** — the cron and the manual trigger were aligned in SMA-479 so that `main` is re-evaluated daily at exactly PR severity.

**The two jobs in `audit.yml` have deliberately different roles, and only one of them is a verdict.** The `audit` job runs `cargo audit --deny warnings` on *every* event — it is the same job definition that gates PRs, un-gated in SMA-479 precisely so the daily and PR severities cannot drift apart. Do not re-add an event filter to it, and do not copy its command into a second step somewhere: one command, in one place, is the whole point. The `scheduled-audit` job runs `rustsec/audit-check` for its auto-issue-filing behaviour (the only place in the repo where a wrapper action is preferred over direct tool invocation) — and **its green status means nothing at any severity**. The action routes `schedule` events to a `reportIssues()` code path that files issues and returns without ever failing, including for critical vulnerabilities; it also files nothing at all for yanked crates, and never re-files an advisory whose issue has been closed. Read the *run* conclusion, never `scheduled-audit`'s job status. Correspondingly, do **not** widen its `if:` to include `workflow_dispatch` — a non-schedule event routes the action to `reportCheck()`, which needs `checks: write` that the job does not grant, and 403s.

Both workflows key their concurrency group on `github.event_name` as well as `github.ref`. This is load-bearing, not decoration: `schedule`, `workflow_dispatch`, and `push` to `main` all resolve `github.ref` to `refs/heads/main`, so a shared group with `cancel-in-progress: false` lets a queued cron run sit *pending* until the next merge cancels it — silently discarding the day's only strict evaluation of `main`. Do not simplify the key back.

Reading the daily signal: **green means clean, red means dirty, absent or `cancelled` means unverified.** Scheduled runs are best-effort — GitHub can delay or drop them under load and disables them entirely after 60 days of repository inactivity — so a missing row is not a passing row. A red cron run should be reproduced with `cargo audit --deny warnings` on a clean checkout before being believed, since both tools fetch over the network and a transient failure looks identical to an advisory hit. The `deny` job additionally runs `scripts/check-advisory-ignore-sync.sh`, which asserts that the `[advisories].ignore` lists in `.cargo/audit.toml` and `deny.toml` have not drifted apart — they are policy-mirrored, and both are now evaluated daily against the same database.
```

- [ ] **Step 3: Replace the `CONTRIBUTING.md` `audit` and `deny` bullets**

Find them:

```bash
grep -n "cargo audit --deny warnings\` against" CONTRIBUTING.md
```

Replace the two bullets — from `- \`audit\` — \`cargo audit…\`` through `…lives in \`deny.toml\` at the workspace root.` — with:

```markdown
- `audit` — `cargo audit --deny warnings` against the [RustSec Advisory DB](https://rustsec.org/).
  Runs on every PR, every push to `main`, a daily cron at 06:00 UTC, and on
  demand via `workflow_dispatch`. The daily run uses the same job and the same
  command as the PR gate, so `main` cannot report green on an advisory that
  would redden a PR (SMA-479). A second job, `scheduled-audit`, runs
  `rustsec/audit-check` on the cron purely to auto-file a GitHub issue — it
  never fails, at any severity, so read the run's conclusion rather than that
  job's status.
- `deny` — `cargo deny --all-features check` enforces the license allowlist,
  ban list, source registry restrictions, and a second advisory pass. Policy
  lives in `deny.toml` at the workspace root. Same trigger set as `audit`, with
  the cron offset to 06:17 UTC. The job first runs
  `scripts/check-advisory-ignore-sync.sh`, which fails if the
  `[advisories].ignore` lists in `deny.toml` and `.cargo/audit.toml` have
  diverged — keep the two in sync, as the note below requires.
```

- [ ] **Step 4: Verify the docs describe what actually shipped**

Cross-check the prose against the files rather than trusting it. Every one of these must agree:

```bash
grep -c "workflow_dispatch" .github/workflows/audit.yml .github/workflows/deny.yml   # 1 each
grep -n "cron:" .github/workflows/audit.yml .github/workflows/deny.yml               # 06:00 and 06:17
grep -c "SMA-479" CLAUDE.md CONTRIBUTING.md                                          # >=1 each
```

- [ ] **Step 5: Confirm no other doc surface needs updating**

This is a deliberate check, not a skip — the repo requires the mdBook and crate READMEs be kept current in the same PR as any user-facing change.

```bash
grep -rn "audit" docs/book/src/ | grep -iv "audit trail\|audit-grade\|audit-only"
```

Expected: no output. The book's only `audit` matches are "audit trail" / "audit-grade schema" in the sessions and observability pages, which concern the session event log, not CI. No crate's public API, install story, feature map, or published status changes, so no crate README or the root README needs an edit either. Record both as conscious no-ops in the PR body.

- [ ] **Step 6: Commit**

```bash
git add CLAUDE.md CONTRIBUTING.md
git commit -m "docs(claude): SMA-479 document supply-chain gate severity and triggers"
git show --stat --oneline HEAD
```

---

## Pre-PR verification

Run all of this before opening the PR, and **paste the outputs of steps 2 and 3 into the PR body** — the edits they rely on are never committed, so this is the only durable evidence for the central claim of the change.

- [ ] **1. Both workflows lint clean**

```bash
actionlint .github/workflows/audit.yml .github/workflows/deny.yml
```

- [ ] **2. The strict command actually fails on the ticket's advisory class**

`RUSTSEC-2025-0012` (`backoff`, unmaintained) is genuinely in the lockfile and suppressed only by policy — so un-ignoring it produces a real `unmaintained` advisory, the exact class from the ticket.

```bash
cargo audit --deny warnings; echo "baseline exit=$?"          # expect 0

sed -i.bak '/"RUSTSEC-2025-0012",/d' .cargo/audit.toml
cargo audit --deny warnings; echo "un-ignored exit=$?"        # expect NON-ZERO, naming backoff
mv .cargo/audit.toml.bak .cargo/audit.toml

cargo audit --deny warnings; echo "restored exit=$?"          # expect 0 again
git status --short                                            # must be clean
```

- [ ] **3. The sync guard fails on drift**

```bash
./scripts/check-advisory-ignore-sync.sh; echo "exit=$?"       # expect: agree (3 entries), exit=0
sed -i.bak '/"RUSTSEC-2024-0436",/d' deny.toml
./scripts/check-advisory-ignore-sync.sh; echo "exit=$?"       # expect exit=1 naming the ID
mv deny.toml.bak deny.toml
git status --short                                            # must be clean
```

- [ ] **4. The real gates still pass locally**

```bash
cargo deny --all-features check
```

`cargo fmt` and `cargo clippy` are not needed — this change touches no Rust source. The pre-push hook will run them anyway.

- [ ] **5. No stray files, and the commits are what you think they are**

```bash
git status --short                                # expect empty
git log --oneline main..HEAD
ls .cargo/*.bak deny.toml.bak 2>/dev/null && echo "STRAY BACKUPS" || echo "clean"
```

## Post-merge verification — owned by the PR author, due the day after merge

`workflow_dispatch` only fires for workflows present on the **default branch**, so these cannot run before merge. A verification step with no owner and no date is how a cron ends up never having fired with nobody finding out.

- [ ] **Same day as merge — the dispatch path works**

```bash
gh workflow run audit.yml && gh workflow run deny.yml
sleep 90

audit_run=$(gh run list --workflow=audit.yml --event workflow_dispatch --limit 1 --json databaseId --jq '.[0].databaseId')
gh run view "$audit_run" --json event,conclusion,jobs --jq '.event, .conclusion, [.jobs[].name]'
# expect: workflow_dispatch, success, ["audit"]
#   -> exactly ONE job: scheduled-audit is correctly skipped on a non-schedule event

deny_run=$(gh run list --workflow=deny.yml --event workflow_dispatch --limit 1 --json databaseId --jq '.[0].databaseId')
gh run view "$deny_run" --json event,conclusion,jobs --jq '.event, .conclusion, [.jobs[].name]'
# expect: workflow_dispatch, success, ["deny"]
```

- [ ] **Next morning — the cron path works and both jobs run**

```bash
cron_run=$(gh run list --workflow=audit.yml --event schedule --limit 1 --json databaseId --jq '.[0].databaseId')
gh run view "$cron_run" --json event,conclusion,jobs --jq '.event, .conclusion, [.jobs[].name]'
# expect: schedule, success, ["audit","scheduled-audit"]  -> BOTH jobs, which is the whole change

gh run list --workflow=deny.yml --event schedule --limit 1 --json event,conclusion,createdAt
# expect: a schedule row, which never existed before this PR
```

- [ ] **Confirm the AC1 detection surface**

```bash
gh api /repos/SMK1085/paigasus-helikon/commits/main/status --jq '.state, (.statuses[].context)'
```

The cron run's verdict must be reflected against `main`'s HEAD — this, not the Actions tab, is what renders a red ✗ beside the latest commit and is the mechanism acceptance criterion 1 actually rests on.

## Rollback

Both changes are single-commit reverts with no state to unwind: re-add `if: github.event_name != 'schedule'` to the `audit` job, and drop the `schedule:` block from `deny.yml`. No published artefact, crate version, or branch-protection setting is touched.
