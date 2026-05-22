# SMA-349 Trybuild Toolchain Gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restrict the `paigasus-helikon-macros` trybuild UI suite to the latest-stable CI matrix row (instead of running on both stable and 1.75 MSRV), without source-level `#[cfg]` or `#[ignore]` gates, so local `cargo test` keeps working by default.

**Architecture:** Workflow-level skip via a libtest `--skip <substring>` filter attached per-toolchain row through a GitHub Actions matrix `include:` block. The trybuild test function is renamed to a workspace-unique name (`trybuild_ui`) so the skip substring cannot collide. The now-redundant `#[rustversion::attr(not(stable), ignore)]` and the `rustversion` dev-dependency are removed.

**Tech Stack:** Rust 2021, GitHub Actions, `trybuild` crate, libtest, `cargo test --workspace --all-features`.

**Spec:** `docs/superpowers/specs/2026-05-22-sma-349-trybuild-toolchain-gate-design.md`

**Branch:** `feature/sma-349-gate-trybuild-ui-suite-to-a-specific-stable-toolchain` (already created and active; the spec doc commit is already on it).

---

## File Structure

Three files change. Each owns one responsibility:

1. **`crates/paigasus-helikon-macros/tests/trybuild.rs`** — rename the test function and remove the rustversion gate. Source of truth for trybuild execution at the test layer.
2. **`crates/paigasus-helikon-macros/Cargo.toml`** — drop the now-unused `rustversion` dev-dependency.
3. **`.github/workflows/ci.yml`** — attach per-toolchain-row `test_args` via matrix `include:` so the MSRV row passes `-- --skip trybuild_ui` to libtest. Source of truth for trybuild execution at the CI layer.

No new files. No moved files.

---

## Task 1: Rename the trybuild test function and drop the rustversion gate

**Files:**
- Modify: `crates/paigasus-helikon-macros/tests/trybuild.rs`

This change has no behavioral effect on its own — `cargo test` locally still runs trybuild on stable, and CI still runs it on both rows until Task 3 lands. We commit it first so that the workflow change in Task 3 has a stable target to skip by name.

- [ ] **Step 1: Replace the entire file content**

Write `crates/paigasus-helikon-macros/tests/trybuild.rs` to exactly:

```rust
//! UI tests for #[tool] and tools!. The workflow restricts execution to
//! the latest-stable CI matrix row (`.github/workflows/ci.yml`) because
//! trybuild `.stderr` snapshots pin rustc diagnostic text byte-for-byte
//! and that text drifts across rustc releases.

#[test]
fn trybuild_ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/bad_*.rs");
    t.compile_fail("tests/ui/no_description.rs");
    t.compile_fail("tests/ui/empty_description.rs");
    t.pass("tests/ui/facade_only_consumer.rs");
}
```

Changes vs. previous:
- Module doc-comment updated to point at `ci.yml` as the gating mechanism (was "Gated to stable rustc because...").
- `#[rustversion::attr(not(stable), ignore)]` attribute removed.
- Function renamed `ui` → `trybuild_ui`.

- [ ] **Step 2: Verify the file compiles and the renamed test runs on the local stable toolchain**

Run: `cargo test -p paigasus-helikon-macros --test trybuild`

Expected (abridged):
```
   Compiling paigasus-helikon-macros v0.1.0 (...)
    Finished `test` profile [unoptimized + debuginfo] target(s) in <time>
     Running tests/trybuild.rs (target/debug/deps/trybuild-<hash>)

running 1 test
test trybuild_ui ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; ...
```

The test name reported by libtest must be `trybuild_ui` (not `ui`). If it still says `ui`, the rename did not take — re-check the edit.

- [ ] **Step 3: Verify `rustversion` has no remaining uses in the macros crate**

Run: `grep -rn "rustversion" crates/paigasus-helikon-macros/src crates/paigasus-helikon-macros/tests`

Expected: no output (empty result). Any hit means the rustversion attribute or another rustversion call survived the edit — fix and re-grep until empty.

The `crates/paigasus-helikon-macros/Cargo.toml` reference will still show — that is removed in Task 2.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-macros/tests/trybuild.rs
git commit -m "$(cat <<'EOF'
test(macros): SMA-349 rename trybuild test and drop rustversion gate

Rename `fn ui` -> `fn trybuild_ui` so a workflow-level libtest
`--skip trybuild_ui` filter has a globally unique substring that
cannot collide with other tests. Drop the
`#[rustversion::attr(not(stable), ignore)]` attribute because
`rustversion::stable` matches the 1.75 MSRV row as much as latest
stable -- it was never doing what its comment claimed. The
workflow change in a follow-up commit becomes the single source
of truth for "trybuild on the latest-stable row only".

No behavior change yet: trybuild still runs on both rows in CI
until the workflow is updated.
EOF
)"
```

Note: scope `macros` is in the `.versionrc` allowlist. Type `test` is the closest-fit Conventional Commits type for "change to the test layer of a crate." `docs` would also be valid given the doc-comment rewrite but `test` better signals the rename.

---

## Task 2: Remove the unused `rustversion` dev-dependency

**Files:**
- Modify: `crates/paigasus-helikon-macros/Cargo.toml` (line 34)

- [ ] **Step 1: Delete the `rustversion` dev-dependency line**

In `crates/paigasus-helikon-macros/Cargo.toml`, delete the single line:

```toml
rustversion  = { workspace = true }
```

(Currently line 34, within the `[dev-dependencies]` block.) Do not touch any other line. Do not realign whitespace on the surrounding lines — they are visually aligned by length, and `rustversion`'s removal does not force a re-alignment of the rest.

The workspace pin in the root `Cargo.toml` (`[workspace.dependencies] rustversion = "..."`) stays — leave it alone. That pin is shared infrastructure for any future opt-in.

- [ ] **Step 2: Verify the macros crate still compiles and tests still pass**

Run: `cargo test -p paigasus-helikon-macros`

Expected: all tests pass, no `unresolved import: rustversion` errors, no warnings about an unused dep. (If you see "warning: unused manifest key" or "warning: dependency rustversion ... never used", the rename in Task 1 missed something — go back.)

- [ ] **Step 3: Verify Cargo.lock did not gain or lose `rustversion`**

Run: `git diff Cargo.lock`

Expected: empty diff for `Cargo.lock`. `rustversion` is still referenced by `[workspace.dependencies]` in the root Cargo.toml, so the lock entry stays. If `Cargo.lock` shows `rustversion` removed, the workspace pin was accidentally edited — undo.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-macros/Cargo.toml
git commit -m "$(cat <<'EOF'
chore(macros): SMA-349 drop unused rustversion dev-dependency

The `#[rustversion::attr]` use was removed in the previous commit;
rustversion has no other consumers in the macros crate. The
workspace pin in the root Cargo.toml is intentionally left so
future crates can opt back in without re-pinning.
EOF
)"
```

---

## Task 3: Attach per-toolchain `test_args` via matrix include

**Files:**
- Modify: `.github/workflows/ci.yml` (the `test:` job, lines 39-52)

This is the change that actually shifts behavior — after this commit, CI will run trybuild only on the 3 stable cells.

- [ ] **Step 1: Replace the `test:` job's `strategy` and run step**

In `.github/workflows/ci.yml`, find the `test:` job (currently spanning roughly lines 39-52). Replace the existing `strategy:` block and the trailing `cargo test` step so the job reads:

```yaml
  test:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
        toolchain: [stable, "1.75"]
        include:
          - toolchain: stable
            test_args: --workspace --all-features
          - toolchain: "1.75"
            test_args: --workspace --all-features -- --skip trybuild_ui
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: ${{ matrix.toolchain }}
      - uses: Swatinem/rust-cache@v2
      - run: cargo test ${{ matrix.test_args }}
```

Notes on what NOT to change:
- The `concurrency`, `permissions`, `env`, and other top-level workflow keys are untouched.
- The other 5 jobs (`fmt`, `clippy`, `docs`, `doc-coverage`, `commits`) are untouched.
- The base matrix dimensions (`os`, `toolchain`) and their values are untouched — only the `include:` and the run step's args reference change. This is what keeps the posted job names stable (`test (ubuntu-latest, stable)`, etc.), so no ruleset edits are needed.

- [ ] **Step 2: Lint the workflow YAML locally**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml'))" && echo "YAML ok"`

Expected: `YAML ok`. If `yaml.YAMLError` raises, fix indentation and re-run.

If `actionlint` is installed (`brew install actionlint` or via `gh extension install`), also run: `actionlint .github/workflows/ci.yml`. Expected: no diagnostics. (Skip if not installed — the YAML parse is sufficient.)

- [ ] **Step 3: Sanity-check the matrix expansion mentally**

The `include:` block's `toolchain:` key overlaps the base matrix's `toolchain:` key. GitHub Actions semantics: when an `include` entry's keys match an existing matrix combination's keys with the same values, the entry's other keys (`test_args`) are merged into every matching combination. This produces 6 cells total — same as before — each now carrying a `test_args` value:

| os            | toolchain | test_args                                           |
|---------------|-----------|-----------------------------------------------------|
| ubuntu-latest | stable    | `--workspace --all-features`                        |
| macos-latest  | stable    | `--workspace --all-features`                        |
| windows-latest| stable    | `--workspace --all-features`                        |
| ubuntu-latest | 1.75      | `--workspace --all-features -- --skip trybuild_ui`  |
| macos-latest  | 1.75      | `--workspace --all-features -- --skip trybuild_ui`  |
| windows-latest| 1.75      | `--workspace --all-features -- --skip trybuild_ui`  |

If you cannot reconstruct this table from the YAML you wrote, the edit is wrong — go back.

- [ ] **Step 4: Run the same commands locally to confirm both forms are valid invocations**

Run (simulates the stable row's full command):

```bash
cargo test --workspace --all-features
```

Expected: all workspace tests pass, including `trybuild_ui`.

Then run (simulates the MSRV row's command — works on any toolchain because libtest accepts `--skip` everywhere):

```bash
cargo test --workspace --all-features -- --skip trybuild_ui
```

Expected: all workspace tests pass **except** `trybuild_ui`, which appears as `1 filtered out` in the `tests/trybuild.rs` integration test binary's summary. Look for a line like:

```
     Running tests/trybuild.rs (target/debug/deps/trybuild-<hash>)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; ...
```

If you see `running 1 test` (with `trybuild_ui`) instead of `running 0 tests` and `1 filtered out`, the `--skip trybuild_ui` substring isn't matching — re-check that Task 1's rename is committed and present in the working tree (`grep -n trybuild_ui crates/paigasus-helikon-macros/tests/trybuild.rs` should show the renamed function).

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "$(cat <<'EOF'
ci(workflows): SMA-349 skip trybuild_ui on the MSRV matrix row

Attach a per-toolchain-row `test_args` value via `matrix.include`
so the stable row runs the full suite and the 1.75 row runs the
suite with `-- --skip trybuild_ui`. `rustversion::stable` cannot
distinguish 1.75 from latest stable, so the libtest filter is
the precise gate. Trybuild `.stderr` snapshots stay generated
against -- and verified on -- the latest-stable row only.

Matrix cell count and posted check names are unchanged, so
`.github/rulesets/main-protection-checks.json` needs no edits.
EOF
)"
```

Note: scope `workflows` is in the `.versionrc` allowlist; `ci` is also valid. `workflows` better signals "I edited a file in `.github/workflows/`".

---

## Task 4: Local end-to-end verification on the feature branch

**Files:** none modified.

- [ ] **Step 1: Confirm the three SMA-349 commits and the spec commit are all present on the branch**

Run: `git log --oneline main..HEAD`

Expected (most-recent first; commit SHAs will differ):

```
<sha> ci(workflows): SMA-349 skip trybuild_ui on the MSRV matrix row
<sha> chore(macros): SMA-349 drop unused rustversion dev-dependency
<sha> test(macros): SMA-349 rename trybuild test and drop rustversion gate
<sha> docs(spec): SMA-349 add trybuild toolchain-gate design
```

If commits are missing or out of order, do not reorder via `rebase -i` (the rule in CLAUDE.md prohibits the `-i` flag in Bash); just verify the actual content matches and continue.

- [ ] **Step 2: Run the full CI gate set locally**

Per the `CLAUDE.md` "reproduce every CI gate locally" block. Run each in order and confirm each passes before moving on:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected for each: exit code 0, no warnings, no test failures.

If `cargo clippy` flags any line in the changed files, fix it in a `style(macros):` or `style(workflows):` follow-up commit on this branch before opening the PR — do not bundle into the existing commits.

The doc-coverage gate and convco-check gate are run by CI; replaying them locally is optional. Skip unless you've changed something that affects either (this plan does not).

- [ ] **Step 3: Confirm convco accepts every commit on the branch**

Run: `convco check main..HEAD`

Expected: no errors. (The `commits` CI job runs the same check against the PR base.)

If convco is not installed locally, skip — the CI job will catch it. Install via `cargo install convco@0.6.3` if you want a pre-flight check matching the workflow pin.

- [ ] **Step 4: Push the branch and observe CI**

```bash
git push -u origin feature/sma-349-gate-trybuild-ui-suite-to-a-specific-stable-toolchain
```

Then watch the workflow runs in GitHub or via `gh run watch`. Verify the per-row trybuild behavior:

```bash
# After the run completes:
gh run view --log --job "test (ubuntu-latest, stable)"   | grep -E "trybuild_ui|filtered out|1 passed"
gh run view --log --job "test (ubuntu-latest, 1.75)"     | grep -E "trybuild_ui|filtered out|0 passed"
```

Expected:
- `test (ubuntu-latest, stable)` log contains `test trybuild_ui ... ok`.
- `test (ubuntu-latest, 1.75)` log contains `1 filtered out` for the `trybuild` integration-test binary and does **not** contain a `test trybuild_ui` line.
- Repeat the spot-check for `macos-latest` and `windows-latest` if the rate-limit allows; one of each toolchain is sufficient evidence.

- [ ] **Step 5: Open the PR**

The PR title must start with a lowercase verb after `SMA-349 ` (per `pr-title.yml` regex `^([A-Z]{2,4}-\d+ )?[^A-Z].+$`). Use the same wording style as `feat(...)` commits even though this PR has multiple commit types:

```bash
gh pr create --title "ci(workflows): SMA-349 gate trybuild UI suite to the latest-stable row" --body "$(cat <<'EOF'
## Summary
- Workflow-level libtest `--skip trybuild_ui` attached per-toolchain-row via matrix `include:`. The latest-stable row runs the full trybuild UI suite; the 1.75 MSRV row skips it (and only it).
- Test function `fn ui` renamed to `fn trybuild_ui` so the skip substring is workspace-unique.
- Removed the now-redundant `#[rustversion::attr(not(stable), ignore)]` and the `rustversion` dev-dependency from `paigasus-helikon-macros`. `rustversion::stable` was never able to distinguish 1.75 from latest stable, which is the bug.
- Matrix cell count and posted check names are unchanged; no ruleset edits required.
- Design doc: `docs/superpowers/specs/2026-05-22-sma-349-trybuild-toolchain-gate-design.md`.

## Test plan
- [x] `cargo test --workspace --all-features` locally on stable -- trybuild runs.
- [x] `cargo test --workspace --all-features -- --skip trybuild_ui` locally -- trybuild is `1 filtered out`.
- [ ] CI run shows `trybuild_ui` executed on every `(*, stable)` cell.
- [ ] CI run shows `1 filtered out` on every `(*, 1.75)` cell.

Closes SMA-349.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

After opening, address any CodeRabbit feedback per the `coderabbit:autofix` flow. Specifically watch for: CodeRabbit may re-suggest pinning the test to a specific rustc version — that suggestion is wrong for the same reason as on the SMA-315 PR; reject it with a link to this spec's §4 (rejected Option C).

---

## Spec coverage check

Spec sections and the task(s) that implement each:

| Spec § | Requirement | Implemented by |
|---|---|---|
| §5.1 | Workflow `test_args` via matrix include | Task 3 step 1 |
| §5.1 | Job names unchanged, no ruleset edit | Task 3 step 1 + step 3 (table check) |
| §5.2 (1) | Rename `ui` → `trybuild_ui` | Task 1 step 1 |
| §5.2 (2) | Remove `#[rustversion::attr]` | Task 1 step 1 |
| §5.2 (3) | Update module doc-comment | Task 1 step 1 |
| §5.3 | Drop `rustversion` from `[dev-dependencies]` | Task 2 step 1 |
| §5.3 | Keep workspace pin | Task 2 step 1 (explicit "leave it alone") |
| §5.3 | Grep verification | Task 1 step 3 + Task 2 step 2 |
| §6 | AC #1 (one row only) | Task 3 + Task 4 step 4 (CI log check) |
| §6 | AC #2 (MSRV still compiles + other tests) | Task 4 step 2 (`cargo test --workspace --all-features -- --skip trybuild_ui`) |
| §6 | AC #3 (local default works) | Task 1 step 2 (`cargo test -p paigasus-helikon-macros --test trybuild`) |
| §7 | Risk: CodeRabbit re-suggestion | Task 4 step 5 (PR body addresses it preemptively) |
| §7 | Risk: future maintainer tidy-up | Task 1 step 1 (module doc-comment names `ci.yml`) |
| §8 (1) | CI log per-row verification | Task 4 step 4 |
| §8 (2) | Local stable verification | Task 1 step 2, Task 2 step 2, Task 4 step 2 |
| §8 (3) | Informational MSRV-local check | Not in plan — explicitly informational per spec, not a gate |

No gaps. The single intentional omission (§8.3 stretch check) is called out as informational by the spec itself.
