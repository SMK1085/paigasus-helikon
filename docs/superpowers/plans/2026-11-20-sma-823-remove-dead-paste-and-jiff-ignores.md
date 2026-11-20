# SMA-823 Remove Dead Ignores Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove three dead RustSec ignores and the dead jiff Dependabot ignore, and correct two stale comments.

**Architecture:** Config-only edit of three policy files. The "test" is the lockfile check plus the exact CI gate commands.

**Tech Stack:** cargo-audit, cargo-deny, Dependabot YAML.

**Spec:** `docs/superpowers/specs/2026-11-20-sma-823-remove-dead-paste-and-jiff-ignores-design.md`

## Global Constraints

- `.cargo/audit.toml` and `deny.toml` carry the same quoted `RUSTSEC-*` IDs (`scripts/check-advisory-ignore-sync.sh`).
- Historical notes carry the advisory ID unquoted.
- No change to `Cargo.toml`, `Cargo.lock`, or any crate directory.
- Commit: `chore(deps): SMA-823 ...`.

## Review Focus

- A removed crate is still reachable → the `audit`/`deny` gate goes red. Pinned by Task 1 Step 1 and Step 4.
- The two policy files drift → the sync script fails. Pinned by Task 1 Step 4.
- `dependabot.yml` indentation breaks after the block removal → Dependabot stops. Pinned by Task 1 Step 4 (YAML parse).

---

### Task 1: Remove the dead entries and correct the comments

**Files:**
- Modify: `.cargo/audit.toml` (header lines 4-6; entries RUSTSEC-2025-0012, RUSTSEC-2024-0384, RUSTSEC-2024-0436; smartstring comment)
- Modify: `deny.toml` (the same three entries; smartstring comment)
- Modify: `.github/dependabot.yml:15-21` (the `ignore:` block)

- [ ] **Step 1: Confirm the crates are absent**

Run: `for c in paste backoff instant proc-macro-error2; do printf '%s ' $c; grep -c "^name = \"$c\"$" Cargo.lock; done`
Expected: each prints `0`.

- [ ] **Step 2: Edit both policy files**

In each file, remove the three quoted IDs with their comments. Put one unquoted note per removed entry, each followed by a bare `#` line:
- backoff and instant: removed in SMA-823; neither crate is in `Cargo.lock` since the temporalio 1.0 upgrade.
- paste: removed in SMA-823; parquet 60 does not depend on paste, and paste is not in `Cargo.lock`.

Change `rhai 1.25.1 is the latest release` to name rhai 1.27.0. In `.cargo/audit.toml`, change "the deny job in ci.yml" to `.github/workflows/deny.yml`.

- [ ] **Step 3: Edit `dependabot.yml`**

Delete the `ignore:` key and its jiff entry with the comment. `groups:` stays at the same indentation.

- [ ] **Step 4: Run the gates**

Run:
```bash
bash scripts/check-advisory-ignore-sync.sh
cargo audit --deny warnings
cargo deny --all-features check 2>&1 | grep -E 'advisory-not-detected|^(advisories|bans|licenses|sources) (ok|FAILED)'
ruby -ryaml -e 'YAML.load_file(".github/dependabot.yml"); puts "yaml ok"'
```
Expected: sync script prints `advisory ignore lists agree (N entries)`; audit exits 0; deny shows `advisories ok`, `bans ok`, `licenses ok`, `sources ok`, and no `advisory-not-detected` line for the removed IDs; `yaml ok`.

- [ ] **Step 5: Commit**

```bash
git add .cargo/audit.toml deny.toml .github/dependabot.yml docs/superpowers/specs/2026-11-20-sma-823-remove-dead-paste-and-jiff-ignores-design.md docs/superpowers/plans/2026-11-20-sma-823-remove-dead-paste-and-jiff-ignores.md
git commit -m "chore(deps): SMA-823 remove dead advisory ignores and the jiff dependabot ignore"
```
