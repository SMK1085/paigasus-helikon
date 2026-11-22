# SMA-822 sqlx `sqlite` out of the workspace base — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Only `sessions-sqlite` and `evals`/`trace-sqlite` enable the sqlx `sqlite` feature, and a CI step keeps it that way.

**Architecture:** A guard script resolves four dependency graphs with `cargo tree -i libsqlite3-sys` (two must have no SQLite, two must have it). CI runs it in `build-no-default-features`. Then the manifests change so the guard passes.

**Tech Stack:** Cargo manifests, bash, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-11-22-sqlx-sqlite-feature-out-of-workspace-base-design.md`

## Global Constraints

- Every `cargo tree` call uses `--color never --target all -e normal,build -i libsqlite3-sys`.
- Workspace inheritance stays: `sqlx = { workspace = true, ... }` in each crate.
- `Cargo.lock` must not change: `git diff --exit-code "$(git merge-base origin/main HEAD)" -- Cargo.lock`.
- No Rust code changes. No version bumps by hand.
- Commit scopes must be in `.versionrc` (`sessions`, `ci`, `deps`, `workspace`, `plan`, `spec`). No `sessions-postgres` scope.
- Work in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.worktrees/sma-822`. Never run git commands that move HEAD or switch branches.
- Work synchronously in the foreground. Do not background long cargo runs.

## Review Focus

1. A typo in the package name or a cargo output change makes the negative check pass without a check → the positive controls must fail in that case (Task 1, step 2 shows the controls run; Task 2 mutation 3 proves a control can fail).
2. cargo exits 101 with "did not match any packages" for an absent package (cargo 1.99) → the negative check treats that as a pass, not as an error (Task 1, step 2).
3. Workspace feature unification hides a lost `sqlite` feature → per-package `cargo build -p` and the positive controls catch it (Task 2).
4. `CARGO_TERM_COLOR: always` in CI adds ANSI codes → `--color never` on every call (Task 1).
5. The published manifest keeps `sqlite` → the `cargo package` manifest check (Task 2, step 6).

---

### Task 1: Guard script and CI step

**Files:**
- Create: `scripts/check-sqlite-scope.sh`
- Modify: `.github/workflows/ci.yml` (job `build-no-default-features`, after the step `Assert vendored-protox is active for the Temporal protos`, near line 214)
- Modify: `docs/runbooks/ci-architecture.md:11` (the list of `build-no-default-features` steps)

**Interfaces:**
- Produces: `bash scripts/check-sqlite-scope.sh` — exit 0 when all four checks pass, exit 1 otherwise. It prints one `::error::` line per failed check, and the captured stdout and stderr of that check.

- [ ] **Step 1: Write `scripts/check-sqlite-scope.sh`**

Header comment: what it checks, SMA-822, and why the absent case is read from stderr (cargo 1.99 exits 101 with `did not match any packages`; older cargo exits 0 with `nothing to print`).

Use `set -uo pipefail`, not `set -e`. Capture stdout, stderr (to a `mktemp` file), and the exit code of each `cargo tree` call separately. Two functions:

- `assert_absent <label> <cargo tree package args...>` passes only if stdout is empty AND either (exit ≠ 0 and stderr contains `did not match any packages`) or (exit = 0 and stderr contains `nothing to print`).
- `assert_present <label> <cargo tree package args...>` passes only if exit = 0 and stdout contains `libsqlite3-sys v`.

Calls:

```bash
assert_absent  "sessions-postgres"          -p paigasus-helikon-sessions-postgres
assert_absent  "facade[sessions-postgres]"  -p paigasus-helikon --no-default-features --features sessions-postgres
assert_present "sessions-sqlite"            -p paigasus-helikon-sessions-sqlite
assert_present "evals[trace-sqlite]"        -p paigasus-helikon-evals --features trace-sqlite
```

Error text for `assert_absent`: SQLite reached `<label>`; keep `sqlite` out of the root `sqlx` features and enable it only in the crate that uses it (SMA-822). Error text for `assert_present`: `<label>` lost the sqlx `sqlite` feature, or the check cannot find `libsqlite3-sys`; add `features = ["sqlite"]` to its `sqlx` entry. Make the file executable.

- [ ] **Step 2: Run it on the unchanged manifests — it must fail**

Run: `bash scripts/check-sqlite-scope.sh; echo "exit=$?"`
Expected: two `::error::` lines (sessions-postgres, facade[sessions-postgres]), both positive controls pass, `exit=1`.

Also confirm the absent branch: temporarily run `assert_absent` against `-p paigasus-helikon-core` (for example with a one-off `bash -c` that sources the functions, or by a temporary edit you revert). Expected: pass. Revert any temporary edit.

- [ ] **Step 3: Add the CI step**

In `build-no-default-features`, after the `vendored-protox` step:

```yaml
      # SMA-822: only sessions-sqlite and evals/trace-sqlite may enable the sqlx
      # `sqlite` feature. Resolves the graph only (no build).
      - name: Assert SQLite only where it is used
        run: bash scripts/check-sqlite-scope.sh
```

- [ ] **Step 4: Update the runbook**

At `docs/runbooks/ci-architecture.md:11`, add to the list of steps: (SMA-822) a `cargo tree` assertion, `scripts/check-sqlite-scope.sh`, that `libsqlite3-sys` is absent from `sessions-postgres` and from the facade with only `sessions-postgres`, and present in `sessions-sqlite` and `evals --features trace-sqlite`.

- [ ] **Step 5: Lint and commit**

Run: `npx markdownlint-cli2 docs/runbooks/ci-architecture.md` → no errors. `bash -n scripts/check-sqlite-scope.sh` → no output.

```bash
git add scripts/check-sqlite-scope.sh .github/workflows/ci.yml docs/runbooks/ci-architecture.md
git commit -m "ci(ci): SMA-822 assert sqlite reaches only the crates that use it"
```

(The guard is red on this commit by design; Task 2 makes it green.)

---

### Task 2: Move the `sqlite` feature into the crates that use it

**Files:**
- Modify: `Cargo.toml:84` (root `sqlx` pin, plus a comment above it)
- Modify: `crates/paigasus-helikon-sessions-sqlite/Cargo.toml:19`
- Modify: `crates/paigasus-helikon-evals/Cargo.toml:33`
- Modify: `crates/paigasus-helikon-sessions-postgres/Cargo.toml:18` (comment only)
- Modify: `crates/paigasus-helikon-sessions-postgres/CHANGELOG.md` (under `## [Unreleased]`)

**Interfaces:**
- Consumes: `bash scripts/check-sqlite-scope.sh` from Task 1.

- [ ] **Step 1: Edit the manifests**

- Root: `sqlx = { version = "0.9.1", default-features = false, features = ["runtime-tokio", "macros", "migrate"] }`. Comment above: only `sessions-sqlite` and `evals`/`trace-sqlite` enable `sqlite` (SMA-822); `scripts/check-sqlite-scope.sh` in `build-no-default-features` fails if SQLite reaches `sessions-postgres`.
- `sessions-sqlite`: `sqlx = { workspace = true, features = ["sqlite"] }`.
- `evals`: `sqlx = { workspace = true, optional = true, features = ["sqlite"] }`.
- `sessions-postgres` comment line 18: say that it adds the postgres driver and aws-lc-rs rustls TLS on top of the workspace sqlx base (`runtime-tokio`, `macros`, `migrate`), which has no `sqlite` (SMA-822).

- [ ] **Step 2: Add the CHANGELOG upgrade note**

Under `## [Unreleased]` in `crates/paigasus-helikon-sessions-postgres/CHANGELOG.md`, add `### Changed` with one entry. It says: the crate no longer enables the sqlx `sqlite` feature, so Postgres-only users do not compile bundled SQLite (`libsqlite3-sys`) or get SQLite advisories such as RUSTSEC-2026-0459. A crate that uses `sqlx::sqlite`, or `sqlx::AnyPool` with a `sqlite:` URL, and got the feature only through this crate must add `features = ["sqlite"]` to its own `sqlx` dependency. The `AnyPool` case compiles but fails at run time.

- [ ] **Step 3: Run the guard — it must pass**

Run: `bash scripts/check-sqlite-scope.sh; echo "exit=$?"`
Expected: four passes, `exit=0`.

Run: `cargo tree --color never -p paigasus-helikon-sessions-postgres -i libsqlite3-sys -e normal,build`
Expected: no tree on stdout (cargo 1.99: `did not match any packages`).

- [ ] **Step 4: Build each crate alone and check the lockfile**

Run each; each must pass:

```bash
cargo build -p paigasus-helikon-sessions-postgres
cargo build -p paigasus-helikon-sessions-sqlite
cargo build -p paigasus-helikon-evals --features trace-sqlite
cargo build -p paigasus-helikon --no-default-features --features sessions-postgres
git diff --exit-code "$(git merge-base origin/main HEAD)" -- Cargo.lock
```

- [ ] **Step 5: Mutation checks (commit first, then revert each with `git checkout -- <file>`)**

Do the Step 9 commit before this step. The root `Cargo.toml` and the `evals` `Cargo.toml` carry the Step 1 edits, so a `git checkout` before the commit would erase them. After the commit, `git checkout -- <file>` on the one mutated file restores the committed fix.

1. Put `"sqlite"` back into the root `sqlx` features → guard fails on both absent checks, both present checks pass.
2. Set the facade feature `sessions-postgres = ["dep:paigasus-helikon-sessions-postgres", "dep:paigasus-helikon-sessions-sqlite"]` in `crates/paigasus-helikon/Cargo.toml:52` → guard fails only on `facade[sessions-postgres]`.
3. Remove `features = ["sqlite"]` from `evals` → guard fails only on `evals[trace-sqlite]`.

After all three: `git status --short` shows no modified tracked files, and the guard passes again.

- [ ] **Step 6: Check the packaged manifests**

Run: `cargo package -p paigasus-helikon-sessions-postgres -p paigasus-helikon-sessions-sqlite -p paigasus-helikon-evals --no-verify --allow-dirty`
Then for each crate, read `<name>-<version>/Cargo.toml` from `target/package/<name>-<version>.crate` with `tar -xOzf` (exact names from each crate's `Cargo.toml` version, no globs) and show the `[dependencies.sqlx]` table.
Expected: `sessions-postgres` features have no `sqlite`; `sessions-sqlite` and `evals` features include `sqlite`.

- [ ] **Step 7: Book and README check**

Run: `grep -rn 'sqlite' docs/book/src crates/paigasus-helikon-sessions-postgres/README.md | grep -i 'postgres'`
Expected: no line names the sqlx features of `sessions-postgres` or says Postgres users get SQLite. Record the result for the PR body.

- [ ] **Step 8: Run the local gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-sessions-sqlite -p paigasus-helikon-evals --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
npx markdownlint-cli2
```

Expected: all pass. (`sessions-it` runs in CI.)

- [ ] **Step 9: Commit (run it after Step 4, before Step 5)**

```bash
git add Cargo.toml crates/paigasus-helikon-sessions-sqlite/Cargo.toml crates/paigasus-helikon-evals/Cargo.toml crates/paigasus-helikon-sessions-postgres/Cargo.toml crates/paigasus-helikon-sessions-postgres/CHANGELOG.md
git commit -m "fix(sessions): SMA-822 stop sessions-postgres from compiling sqlite"
```

---

## After merge (coordinator, not an implementer task)

- The release PR bumps `sessions-postgres`, `sessions-sqlite`, `evals`, the facade, and the CLI. Watch its CI.
- After publish, a scratchpad project with only the new `paigasus-helikon-sessions-postgres` shows no `libsqlite3-sys` in `cargo tree -i libsqlite3-sys -e normal,build`.
