# SMA-763 Remove the RUSTSEC-2026-0459 ignore Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the RUSTSEC-2026-0459 ignore from both advisory policy files and raise the workspace `sqlx` floor to `0.9.1`.

**Architecture:** A manifest-only change. `Cargo.lock` on `main` already locks `sqlx` 0.9.1 and `libsqlite3-sys` 0.40.0, so the build graph does not change. The floor raise changes the `sqlx` requirement in the published manifests of evals, sessions-sqlite and sessions-postgres.

**Tech Stack:** Cargo workspace dependencies, cargo-audit, cargo-deny.

**Spec:** `docs/superpowers/specs/2026-11-20-sma-763-remove-rustsec-2026-0459-ignore-design.md`

## Global Constraints

- Commit type and scope: `chore(deps)`. Message prefix: `chore(deps): SMA-763 ...`.
- `Cargo.lock` must not change: `git diff --exit-code Cargo.lock`.
- No hand bump of any crate `version`. No CHANGELOG edit.
- Keep the `sqlx` `default-features = false` and `features = [...]` keys exactly as they are.
- A RUSTSEC ID in a comment must be unquoted. `scripts/check-advisory-ignore-sync.sh` counts quoted IDs as entries.
- Do not edit `crates/paigasus-helikon-sessions-postgres/Cargo.toml`.
- Stage files by explicit path. Never `git add -A` (`.env` is not gitignored).

## Review Focus

1. A quoted `"RUSTSEC-2026-0459"` left in a comment: the sync script then passes, but the ignore list is not what it seems. Pinned by the `grep` check in Task 1 Step 5.
2. Only one of the two policy files edited: one gate stays different from the other. Pinned by `check-advisory-ignore-sync.sh` in Task 1 Step 5.
3. `cargo` rewrites `Cargo.lock` after the pin change: the PR then carries an unreviewed dependency change. Pinned by `git diff --exit-code Cargo.lock` in Task 1 Step 5.
4. The published manifest still says `0.9`: the floor raise has no effect for users. Pinned by the `cargo package` check in Task 1 Steps 1 and 5.
5. evals' feature-gated `sqlx` code not compiled: a break hides behind `trace-sqlite`. Pinned by `cargo test --workspace --all-features` in Task 1 Step 6.

---

### Task 1: Remove the ignore and raise the sqlx floor

**Files:**
- Modify: `.cargo/audit.toml:35-40` (the RUSTSEC-2026-0459 comment block and entry)
- Modify: `deny.toml:54-59` (the same block)
- Modify: `Cargo.toml:80` (the `sqlx` workspace pin)

**Interfaces:**
- Consumes: nothing.
- Produces: the published manifests of the three `sqlx` crates require `sqlx` `0.9.1`.

- [ ] **Step 1: Record the failing state**

Run:
```bash
cargo package -p paigasus-helikon-sessions-sqlite --no-verify --allow-dirty
tar -xzOf target/package/paigasus-helikon-sessions-sqlite-*.crate '*/Cargo.toml' | grep -A1 '^\[dependencies.sqlx\]'
```
Expected: `version = "0.9"`. This is the value that the change must move to `"0.9.1"`.

- [ ] **Step 2: Remove the ignore block from both policy files**

In `.cargo/audit.toml` and `deny.toml`, delete the six-line block that starts `# libsqlite3-sys 0.37.0 bundles C SQLite` and ends with the `"RUSTSEC-2026-0459",` entry. In its place, put this note (indent to match each file: 4 spaces in `audit.toml`, 2 in `deny.toml`):

```
# Historical note: the libsqlite3-sys (RUSTSEC-2026-0459) entry was removed
# in SMA-763 after sqlx 0.9.1 allowed libsqlite3-sys >= 0.39.
```

- [ ] **Step 3: Raise the sqlx floor in `Cargo.toml:80`**

Change `version = "0.9"` to `version = "0.9.1"`. Above the line, add:

```
# Floor 0.9.1: sqlx-sqlite 0.9.0 capped libsqlite3-sys at <0.38.0, so it can
# only resolve the RUSTSEC-2026-0459 version. Do not relax to "0.9" (SMA-763).
```

- [ ] **Step 4: Run fmt**

Run: `cargo fmt --all -- --check`
Expected: exit 0.

- [ ] **Step 5: Run the supply-chain and manifest checks**

Run each command and check its result:
```bash
git diff --exit-code Cargo.lock                      # exit 0
grep -n '"RUSTSEC-2026-0459"' .cargo/audit.toml deny.toml   # no output, exit 1
bash scripts/check-advisory-ignore-sync.sh           # exit 0
cargo audit --deny warnings                          # exit 0
cargo deny --all-features check                      # exit 0
cargo package -p paigasus-helikon-sessions-sqlite --no-verify --allow-dirty
tar -xzOf target/package/paigasus-helikon-sessions-sqlite-*.crate '*/Cargo.toml' | grep -A1 '^\[dependencies.sqlx\]'   # version = "0.9.1"
```

- [ ] **Step 6: Run clippy and the tests**

Run:
```bash
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
```
Expected: both exit 0. If many bedrock tests fail with a NATIVE_ROOTS error on macOS, that is the known checkout-path issue, not this change. Report it, and run `cargo test -p paigasus-helikon-sessions-sqlite -p paigasus-helikon-sessions-postgres -p paigasus-helikon-evals --all-features` to confirm the affected crates.

- [ ] **Step 7: Commit**

```bash
git add .cargo/audit.toml deny.toml Cargo.toml \
  docs/superpowers/specs/2026-11-20-sma-763-remove-rustsec-2026-0459-ignore-design.md \
  docs/superpowers/plans/2026-11-20-sma-763-remove-rustsec-2026-0459-ignore.md
git commit -m "chore(deps): SMA-763 remove the rustsec-2026-0459 ignore and raise sqlx to 0.9.1"
git show --stat HEAD   # exactly these five files
```
