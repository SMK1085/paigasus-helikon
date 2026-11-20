# jiff 1 upgrade Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the workspace to jiff `1` and make release-plz release it as a minor bump of `paigasus-helikon-core` and the facade `paigasus-helikon`.

**Architecture:** A dependency-requirement change in the root `Cargo.toml`, a lockfile update, upgrade notes in two CHANGELOGs, and one new compatibility test in core. The minor bump comes from the `!` in the squash title, which release-plz gives to the two crates whose directories the commit touches. A scratch-clone `release-plz update` run proves the bump before the PR.

**Tech Stack:** Rust 1.94+ workspace, jiff, serde_json, release-plz CLI v0.4.3, cargo-semver-checks.

**Spec:** `docs/superpowers/specs/2026-11-20-jiff-1-upgrade-design.md`

All paths are relative to the worktree root
`/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-831`.
Guard every `cd` with `|| exit 1`. Never run `git checkout <branch>`,
`git switch`, `git reset --hard`, or `git stash` in this worktree. Work in the
foreground until each command has a final status.

## Global Constraints

- jiff requirement: `jiff = { version = "1", features = ["serde"] }` in root `Cargo.toml` `[workspace.dependencies]`. No exact pin (SMA-818).
- `Cargo.lock` has jiff 1.x only (jiff, jiff-core, jiff-static, jiff-tzdb, jiff-tzdb-platform).
- No source change outside the new test file. The probe proved the workspace compiles as is.
- Allowed diff: root `Cargo.toml`, `Cargo.lock`, `crates/paigasus-helikon-core/CHANGELOG.md`, `crates/paigasus-helikon/CHANGELOG.md`, `crates/paigasus-helikon-core/tests/jiff_wire_compat.rs`, `docs/superpowers/`. No other `crates/*` path.
- Each branch commit that touches a CHANGELOG uses a `!` type.
- PR title: `chore(deps)!: SMA-831 upgrade jiff to 1`.
- Target versions: core 0.6.0 → 0.7.0, facade 0.6.14 → 0.7.0. Every other changed crate gets a patch.
- All user-facing text (CHANGELOG notes, comments) is in ASD-STE100 Simplified Technical English.
- Do not touch the five `trybuild_ui` mismatches. They fail on clean `main` with local rustc 1.99.0.

## Review Focus

1. A stored `ts` with nine fractional digits must read back and write back to the same bytes. Covered: Task 1 `nanosecond_ts_keeps_its_bytes`.
2. jiff writes the shortest fractional part (`.5`, not `.500000000`). A row written that way must keep its bytes. Covered: Task 1 `trimmed_fraction_ts_keeps_its_bytes`.
3. A whole-second `ts` has no fractional part. Covered: Task 1 `whole_second_ts_keeps_its_bytes`.
4. A row that another writer stored with a `+00:00` offset must still parse to the same instant. Covered: Task 1 `utc_offset_ts_parses`.
5. The `build-no-default-features` gate compiles crates with default features off. jiff 1 must not need a default feature that the workspace turns off. Covered: Task 2 Step 6.

---

### Task 1: Pin the stored timestamp format on jiff 0.2

**Files:**
- Revert: `Cargo.toml`, `Cargo.lock` (the uncommitted probe edit)
- Create: `crates/paigasus-helikon-core/tests/jiff_wire_compat.rs`

**Interfaces:**
- Consumes: `paigasus_helikon_core::{SessionEvent, ContentPart}`, `jiff::Timestamp`.
- Produces: the test binary `jiff_wire_compat`, which Task 2 runs again on jiff 1.

- [ ] **Step 1: Remove the probe edit**

Run: `git restore Cargo.toml Cargo.lock && grep -n '^jiff' Cargo.toml`
Expected: `jiff                  = { version = "0.2.46", features = ["serde"] }`, and `git status --porcelain` lists only `docs/superpowers/` files.

- [ ] **Step 2: Write the test file**

Module doc: the file pins the JSON form of `SessionEvent.ts`, as jiff 0.2 writes it, so that a jiff major upgrade cannot change stored session rows (SMA-831).

One helper: `fn assert_bytes_kept(json: &str, expected_ts: Timestamp)`. It deserializes `json` (compact, one line) into `SessionEvent`, asserts that the event's `ts()` equals `expected_ts`, serializes it again with `serde_json::to_string`, and asserts that the result equals `json` byte for byte.

Each test uses a `user_message` event with one text part `"hi"`. The JSON shape is `{"type":"user_message","content":[{"type":"text","text":"hi"}],"ts":"<TS>"}`.

| Test | `<TS>` | `expected_ts` |
|---|---|---|
| `nanosecond_ts_keeps_its_bytes` | `2026-11-20T15:08:42.123456789Z` | `Timestamp::new(1_795_187_322, 123_456_789)` |
| `trimmed_fraction_ts_keeps_its_bytes` | `2026-11-20T15:08:42.5Z` | `Timestamp::new(1_795_187_322, 500_000_000)` |
| `whole_second_ts_keeps_its_bytes` | `2026-11-20T15:08:42Z` | `Timestamp::new(1_795_187_322, 0)` |

`utc_offset_ts_parses` is different. It deserializes the same JSON with `<TS>` = `2026-11-20T15:08:42.123456789+00:00` and asserts only that `ts()` equals `Timestamp::new(1_795_187_322, 123_456_789)`. A writer can store a `+00:00` offset, and jiff re-serializes it as `Z`, so a byte check is not correct here.

If the JSON field order from `serde_json::to_string` differs from the shape above, take the order from the existing snapshot `tests/snapshots/serde_roundtrip__session_event_user_message_roundtrip.snap` and use it in all four strings.

- [ ] **Step 3: Run the tests on jiff 0.2**

This is a characterization test, so it passes the first time. It must pass on jiff 0.2, because that proves that the literals are real 0.2 output.
Run: `cargo test -p paigasus-helikon-core --test jiff_wire_compat`
Expected: `test result: ok. 4 passed`. If a byte test fails, the literal is not what jiff 0.2 writes. Correct the literal to the 0.2 output, and record the change in the task report. Do not change the assertion.

- [ ] **Step 4: Format, lint, commit**

Run: `cargo fmt --all && cargo clippy -p paigasus-helikon-core --all-features --tests -- -D warnings`
Expected: no diff from fmt after the run, and clippy exits 0.

```bash
git add crates/paigasus-helikon-core/tests/jiff_wire_compat.rs
git commit -m "test(core): SMA-831 pin the stored timestamp format before the jiff 1 upgrade"
```

---

### Task 2: Upgrade jiff to 1, with the upgrade notes

**Files:**
- Modify: `Cargo.toml:49-53` (the jiff comment and requirement)
- Modify: `Cargo.lock`
- Modify: `crates/paigasus-helikon-core/CHANGELOG.md` (under `## [Unreleased]`)
- Modify: `crates/paigasus-helikon/CHANGELOG.md` (under `## [Unreleased]`)

**Interfaces:**
- Consumes: the `jiff_wire_compat` test from Task 1.
- Produces: the branch state that Task 3 squashes and checks.

- [ ] **Step 1: Change the requirement and the comment**

Set the line to `jiff                  = { version = "1", features = ["serde"] }` (keep the column alignment). Replace lines 49-52 with a comment that says: a caret requirement, not an exact pin, because an exact pin goes into every published manifest and blocks consumers that need a newer jiff (SMA-818); `jiff::Timestamp` is public API of core (`SessionEvent.ts`), so the next jiff major is a breaking change of core and the facade, and its PR title must carry `!` (SMA-831). Do not keep the 0.2.29 / RUSTSEC-2026-0173 text.

- [ ] **Step 2: Update the lockfile**

Run: `cargo update -p jiff && grep -A1 -E '^name = "jiff' Cargo.lock`
Expected: jiff, jiff-core, jiff-static at `1.0.1` (or a later 1.x), jiff-tzdb and jiff-tzdb-platform at `1.0.0` (or a later 1.x). No `0.` version of any jiff crate.

- [ ] **Step 3: Add the upgrade notes**

Insert in both CHANGELOGs, directly under `## [Unreleased]`, with one blank line above and below:

```markdown
### Upgrade notes

- This release requires `jiff` 1. The `ts` fields of `SessionEvent` and the return value of `SessionEvent::ts()` now have the type `jiff::Timestamp` from jiff 1. A `Timestamp` from jiff 0.2 is a different type.
- If your code passes a `Timestamp` into a `SessionEvent`, or uses the value from `ts()` with jiff code, change your `jiff` requirement to `1`. Read the [jiff 1.0 changelog](https://github.com/BurntSushi/jiff/blob/master/CHANGELOG.md) for the APIs that jiff renamed.
- If your code uses only the constructors that set `ts` to the current time (for example `SessionEvent::user_message`) and does not read `ts`, it needs no change.
- Upgrade `paigasus-helikon-core` and `paigasus-helikon` to 0.7 together. If your lockfile keeps `paigasus-helikon` 0.6 after `cargo update`, it can hold two copies of core. Run `cargo tree -d` to find them.
- The JSON format of a stored `SessionEvent` does not change. Session rows that an earlier version wrote stay readable.
```

- [ ] **Step 4: Run the compatibility test on jiff 1**

Run: `cargo test -p paigasus-helikon-core --test jiff_wire_compat`
Expected: `test result: ok. 4 passed`. A failure here means the stored format changed. Stop and report it. Do not edit the literals.

- [ ] **Step 5: Run the jiff crates' tests**

Run: `cargo test -p paigasus-helikon-core -p paigasus-helikon-sessions-sqlite -p paigasus-helikon-sessions-testkit -p paigasus-helikon-evals -p paigasus-helikon-runtime-agentcore -p paigasus-helikon --all-features --no-fail-fast 2>&1 | grep -E '^test result|FAILED|panicked'`
Expected: every `test result: ok`, except the `trybuild_ui` binary, which fails with the 5 known mismatches (`builder_missing_model`, `builder_missing_name`, `builder_model_twice`, `builder_name_twice`, `builder_source_blocks_build`). Any other failure is a real failure.

- [ ] **Step 6: Run the local CI gates**

Run each command and check that it exits 0:
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-features --all-targets -- -D warnings`
- `cargo build --workspace --no-default-features`
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
- `cargo deny check` (if installed; the `syn` 2/3 duplicate is a warning, not an error)
- `npx markdownlint-cli2` (run `npm ci` first if `node_modules` is missing)

- [ ] **Step 7: Check the diff scope and commit**

Run: `git diff --name-only origin/main...HEAD; git diff --name-only`
Expected: only the paths in the Global Constraints "Allowed diff" line.

```bash
git add Cargo.toml Cargo.lock crates/paigasus-helikon-core/CHANGELOG.md crates/paigasus-helikon/CHANGELOG.md
git commit -m "chore(deps)!: SMA-831 upgrade jiff to 1"
```

---

### Task 3: Prove the release-plz bump in a scratch clone

This task changes nothing in the worktree. It is release work, so it runs on Opus.

**Files:** none in the repo. Use `$S=/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/9134c40b-e34c-4e03-aba6-d1c143283d73/scratchpad/sma-831-rp`.

**Interfaces:**
- Consumes: the branch head after Task 2.
- Produces: a version table for the PR body and the GATE 2 report.

- [ ] **Step 1: Install the CI tool versions into a scratch root**

Run: `cargo binstall --no-confirm --root "$S/tools" release-plz@0.4.3 cargo-semver-checks` (fall back to `cargo install --locked --root "$S/tools"` if binstall is missing). Put `$S/tools/bin` first on `PATH` for the next steps.
Expected: `release-plz --version` prints `release-plz 0.4.3`.

- [ ] **Step 2: Build the squash commit in a scratch clone**

Clone the worktree into `$S/clone` with all tags (`git clone --no-local <worktree> "$S/clone"`, then `git fetch --tags https://github.com/SMK1085/paigasus-helikon.git +main:refs/remotes/gh/main`). In the clone only: create a local branch named `main` at `gh/main` (release-plz reads the local `main`), apply the branch diff as one commit (`git diff gh/main...<branch-head> | git apply --index`), and commit with the message `chore(deps)!: SMA-831 upgrade jiff to 1 (#999)` and no body.
Expected: `git log -1 --format=%B` prints the title line only.

- [ ] **Step 3: Run release-plz update**

Run in the clone: `release-plz update --config release-plz.toml` (no token needed; it publishes nothing).
Expected: the clone's crate `Cargo.toml` files show core `0.7.0` and facade `0.7.0`. Every other crate with a version change has a patch bump. No other crate has a minor bump. Both CHANGELOGs have a `## [0.7.0]` section that holds the `### Upgrade notes` text, and `## [Unreleased]` is empty again.

- [ ] **Step 4: Record the version table**

Write every crate whose version changed as `crate: old → new` to `$S/version-table.md`. If core or the facade is not at 0.7.0, stop and report. Do not change the design.

---

### Final check (coordinator)

- [ ] `git log --format=%s origin/main..HEAD` shows the Task 1 and Task 2 commits plus the spec and plan commits, and `convco check origin/main..HEAD` exits 0.
- [ ] The version table from Task 3 matches the spec goals.
