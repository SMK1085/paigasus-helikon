# SMA-349 — Gate trybuild UI suite to the latest-stable CI row

**Status:** Approved (2026-05-22)
**Linear:** [SMA-349](https://linear.app/smaschek/issue/SMA-349/gate-trybuild-ui-suite-to-a-specific-stable-toolchain-workflow-or)
**Related:** [SMA-315](https://linear.app/smaschek/issue/SMA-315) (added the trybuild suite that this ticket retroactively gates)

## 1. Problem

`crates/paigasus-helikon-macros/tests/trybuild.rs` contains a UI test suite that diff-checks rustc diagnostics byte-for-byte against `.stderr` snapshots. SMA-315 gated the suite with `#[rustversion::attr(not(stable), ignore)]`, intending "only run on the developer's stable rustc." The bug: `rustversion::stable` is true for **any** stable rustc — including the 1.75 MSRV matrix row.

Today this is silently fine: rustc 1.75 and the current latest stable emit byte-identical diagnostics for every fixture the suite captures. The day rustc 1.95+ ships a wording change to one of those diagnostics, the 1.75 column of `.github/workflows/ci.yml` will break — and it will break on the row developers do **not** regenerate snapshots against, with a confusing "your snapshot is wrong" failure mode.

## 2. Goal

Run the trybuild UI suite on the latest-stable matrix row only. Keep all other tests running on every row. Keep local `cargo test -p paigasus-helikon-macros` running trybuild by default so developers don't have to remember an opt-in flag.

## 3. Non-goals

- Not touching MSRV (still 1.75).
- Not changing the matrix shape (still `{ubuntu, macos, windows} × {stable, 1.75}`, still all 6 cells run).
- Not regenerating `.stderr` snapshots.
- Not changing the trybuild fixtures themselves.
- Not changing `audit.yml`, `deny.yml`, `sbom.yml`, `msrv.yml`, or `pr-title.yml`.
- Not changing `.github/rulesets/main-protection-checks.json` — status-check job names are unchanged by this design.

## 4. Approach

Three options were considered (full analysis in the Linear comment thread on SMA-349). Settled on **workflow-level skip**:

- **Option A — workflow-level skip (chosen).** The workflow attaches a per-row `test_args` value that includes a libtest `--skip` filter on the MSRV row. No source-level `#[cfg]` or `#[ignore]` gate.
- **Option B — Cargo feature gate** (rejected): a default-off `trybuild-ui` feature would force developers to remember `--features trybuild-ui` locally, violating the third acceptance criterion. Flipping it to default-on collides with the `cargo test --workspace --all-features` CI convention because Cargo has no "all features except X" — the MSRV row would need an explicit feature list that grows with the workspace.
- **Option C — pin `rustversion::attr(stable(1.75), ignore)`** (rejected): smallest diff but introduces a version literal that must be hand-bumped every MSRV bump. The workflow-level gate keys on `matrix.toolchain` directly, which is already the source of truth for "which row is MSRV."

Option A's stated con — duplicate test invocations — is collapsed by computing `test_args` as a matrix value rather than using two `if:`-gated steps.

## 5. Detailed design

### 5.1 Workflow change

In `.github/workflows/ci.yml`, replace the `test` job's `strategy.matrix` and the `cargo test` run step:

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

How GitHub Actions evaluates this: when an `include:` entry's keys overlap a base-matrix dimension (here, `toolchain`), it **augments** every existing cell that matches the value. It does not add a new dimension and does not create new cells. So all 3 `stable` cells receive `test_args: --workspace --all-features`, and all 3 `1.75` cells receive the skip variant. Cell count stays at 6. Job names posted to the GitHub Checks API stay as `test (ubuntu-latest, stable)`, `test (ubuntu-latest, 1.75)`, etc. — required-status-check rules need no edits.

### 5.2 Test source change

In `crates/paigasus-helikon-macros/tests/trybuild.rs`:

1. Rename the test function: `fn ui()` → `fn trybuild_ui()`. The new name is a globally unique substring across the workspace's test functions, so the workflow's `--skip trybuild_ui` filter (propagated by `cargo test --workspace` to every test binary's libtest harness) cannot collide with an unrelated `ui` test elsewhere.
2. Remove the `#[rustversion::attr(not(stable), ignore)]` attribute. The workflow `--skip` is now the single source of truth; the rustversion gate is redundant for CI, and for local dev it only affects beta/nightly toolchains (not a hot path).
3. Update the module doc-comment to point at the workflow as the gating mechanism, so the next reader knows where to look.

Resulting file:

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

### 5.3 Cargo.toml change

In `crates/paigasus-helikon-macros/Cargo.toml`, drop the `rustversion` line from `[dev-dependencies]`. It was added in SMA-315 solely for the now-removed `#[rustversion::attr]` use; verify with `grep -rn rustversion crates/paigasus-helikon-macros` before the PR — should return empty after the source change.

The workspace-level pin in root `Cargo.toml` (`[workspace.dependencies] rustversion = "..."`) stays. It is not a yanked entry; leaving it allows future opt-in by any member without re-pinning.

## 6. Acceptance criteria verification

| Acceptance criterion | How design satisfies it |
|---|---|
| trybuild UI test runs on exactly one CI matrix row (latest stable) | `--skip trybuild_ui` is attached to every `1.75` cell via the matrix `include:`. The 3 `stable` cells (ubuntu/macos/windows) run the full suite, including trybuild. |
| MSRV row continues to compile and runs every other test | The test function is still **compiled** on 1.75 — it is filtered out at runtime by libtest, not `#[cfg]`-excluded at build time. So a future regression in the trybuild test file's own source would still surface on the MSRV row. Every non-trybuild test runs normally. |
| Local `cargo test -p paigasus-helikon-macros` runs trybuild by default | No `#[ignore]`, no `#[cfg(feature = …)]`, no env-var guard. Local libtest receives no `--skip` filter. Test runs. |

## 7. Risks

1. **Substring collision drift.** `--skip trybuild_ui` is a libtest substring filter applied to every test binary in the workspace via `cargo test --workspace`. A future test function whose name contains `trybuild_ui` as a substring would also be skipped on the MSRV row. The chosen substring is specific enough that accidental collision is unlikely; this is accepted as a low-likelihood ergonomic risk caught by code review.
2. **CodeRabbit may re-suggest "pin to 1.75."** That was its original (backwards) suggestion on the SMA-315 PR. The PR description for this work will explicitly explain why the analysis settled on workflow-level gating, citing the Linear comment thread.
3. **Future maintainer "tidy-up."** Someone unfamiliar with the history might re-add a `rustversion::attr` gate thinking it's safer. The module doc-comment in `trybuild.rs` names the workflow file as the gating mechanism to forestall this.

## 8. Test plan

Manual verification. No new automated tests.

1. After pushing the feature branch, observe the `ci.yml` workflow run:
   - `test (ubuntu-latest, stable)` log: trybuild integration-test binary reports `running 1 test ... test trybuild_ui ... ok`.
   - `test (ubuntu-latest, 1.75)` log: same binary reports `running 0 tests` and `1 filtered out` (libtest's term for skipped-by-filter).
   - Repeat the same two checks for `macos-latest` and `windows-latest`.
2. Locally on the developer's stable toolchain: `cargo test -p paigasus-helikon-macros` runs and passes the trybuild suite.
3. Informational, not gating: `rustup run 1.75 cargo test -p paigasus-helikon-macros` runs trybuild (no `--skip`, no `#[ignore]`) and currently passes — confirms today's 1.75/latest diagnostics are still byte-identical. The day this stretch check starts failing is precisely the day this ticket's design prevents that failure from landing in CI.

## 9. References

- PR review thread that surfaced the bug: <https://github.com/SMK1085/paigasus-helikon/pull/24#discussion_r3289440557>
- SMA-315 spec §6.2 *Why stable-only*: documents the original convention; predates this discovery.
- `rustversion` crate predicates: <https://docs.rs/rustversion> (`stable`, `since`, `before`, `stable(N.M)`).
- GitHub Actions matrix `include:` semantics: <https://docs.github.com/en/actions/using-jobs/using-a-matrix-for-your-jobs#expanding-or-adding-matrix-configurations>
