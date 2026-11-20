# SMA-826 — Move the shared core test helpers into `tests/common`

Size: a small change. Test-only. No public API, book or README change.

## Intent

Three core integration test files keep their own copies of the same test
helpers. A change to one copy does not reach the other copies, and no test
fails when they drift. SMA-810 changed the pairing oracle and had to change
each copy by hand. After this change, each helper has one source in core.

One copy of the oracle stays in runtime-tokio. No test catches drift between
that copy and the core source. We accept this because a shared crate is out
of scope. Doc comments in both directions name the other copy.

## Current state (measured on `main` at `e7ac5377`)

| Helper | Copies | Difference between copies |
|---|---|---|
| `assert_tool_calls_paired` | `tests/deferred_resume.rs:579`, `tests/handoff_transfer_result.rs:187`; runtime-tokio `tests/deferred.rs:427` | The code is equal in all three. Only comments differ. The `handoff_transfer_result.rs` doc comment has one more sentence about the round rule. The `deferred_resume.rs` and runtime-tokio copies have inline comments. |
| `DeferB` | `tests/deferred_resume.rs:24`, `tests/handoff_transfer_result.rs:601`, `tests/subagent_propagation.rs:338` | The `subagent_propagation.rs` copy uses the prompt `"policy says wait"`; the other two use `"pay?"`. |
| `RequestLog` | `tests/deferred_resume.rs:45`, `tests/handoff_transfer_result.rs:622` | None. |
| `CapturingModel` (not named in the issue) | `tests/subagent_propagation.rs:359` | Same behaviour as `RequestLog`. It has no constructor and no `requests()` getter; the test reads the field directly. |

`tests/common/mod.rs` already exists. 25 core test files include it with
`#[path = "common/mod.rs"] mod common;`. It has `#![allow(dead_code, ...)]`, so
a helper that one binary does not use causes no warning.

## Goals

- `assert_tool_calls_paired`, `DeferB` and `RequestLog` are `pub` items in
  `crates/paigasus-helikon-core/tests/common/mod.rs`. The three test files
  import them from `common` and keep no local copy.
- `CapturingModel` in `subagent_propagation.rs` is removed. Its test uses
  `RequestLog`.
- Each test keeps its behaviour. `cargo test -p paigasus-helikon-core` passes,
  and the test count of each changed test binary does not change.
- The runtime-tokio copy of `assert_tool_calls_paired` gets a doc comment that
  names `crates/paigasus-helikon-core/tests/common/mod.rs` as the source.

## Non-goals

- No shared crate between core and runtime-tokio (the issue excludes it).
- No change to the runtime-tokio permission policies `DeferPolicy`
  (`tests/deferred.rs:31`) and `DeferPay` (`tests/turn_checkpoint.rs:317`).
  They defer the tool `pay`, so they are not copies of `DeferB`.
- No `exclude = ["tests/"]` in a crate manifest. That changes the published
  package contents and needs its own ticket.
- No other test helper moves (for example `tool_result_texts`, `call_b_turn`).

## Anti-goals

- A changed oracle rule. The shared oracle must have the same code as the
  current copies, character for character, apart from comments.

## Approaches

1. **Move into the existing `tests/common/mod.rs` (chosen).** The issue names
   this module. All files already include it, and its `dead_code` allow
   covers unused helpers.
2. **A new submodule, for example `tests/common/deferral.rs`.** A
   `pub mod deferral;` line in `mod.rs` would load it, so the 25 include
   sites would not change. The issue names `mod.rs`, so this approach is not
   used.
3. **A shared dev-only crate for core and runtime-tokio.** The issue puts it
   out of scope.
4. **runtime-tokio includes the core file with a cross-crate `#[path]`.**
   The path is outside the runtime-tokio package. Each crate publishes its
   `tests/` directory, so the published runtime-tokio tests would not
   compile. This approach is not used, and the runtime-tokio copy stays.

## Design

- **Oracle.** Move it into `common/mod.rs` with the fullest comments: the
  `handoff_transfer_result.rs` doc comment (with the round-rule sentence)
  and the inline comments of the `deferred_resume.rs` copy. The code does not
  change. Add this text to its doc comment: "A copy is in
  `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs`. Keep the two
  copies the same."
- **Oracle self-test.** The `#[should_panic]` test
  `pairing_oracle_rejects_a_call_after_a_result_while_a_call_waits` stays in
  `deferred_resume.rs`. A `#[test]` in `common/mod.rs` would run once in each
  of the 25 test binaries.
- **`DeferB`.** Move it with the prompt `"pay?"`. *My call:* the
  `subagent_propagation.rs` tests check only the prefix
  `"permission denied: approval deferred, but this run cannot defer: "` with
  `starts_with`, so the prompt text does not change what they check.
- **`RequestLog`.** Move it with its `with_scripts` constructor and its
  `requests()` getter. Both methods must be `pub fn`, because `common` is a
  child module (a private method gives error E0624). The fields stay
  private.
- **`CapturingModel`.** *My call:* replace it with `RequestLog` in
  `subagent_propagation.rs`. It is the same type of copy that the issue wants
  to remove. The test then uses `RequestLog::with_scripts(...)` and
  `.requests()` in place of the struct literal and the direct field read.
- **Imports.** Add `PermissionDecision` and `PermissionPolicy` to
  `common/mod.rs` (the others are already there). Remove the imports that
  become unused. `cargo test` only warns on them; the clippy `-D warnings`
  gate fails. The challenger worked out this list; clippy is the final check:
  - `deferred_resume.rs`: remove `Model`, `ModelCapabilities`, `ModelError`.
  - `handoff_transfer_result.rs`: remove `Mutex`, `PermissionDecision`,
    `PermissionPolicy`.
  - `subagent_propagation.rs`: remove `BoxStream`, `CancellationToken`,
    `Model`, `ModelCapabilities`, `ModelError`, `ModelRequest`,
    `PermissionDecision`, `PermissionPolicy`.
- **runtime-tokio.** Replace the doc comment of
  `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs:427` with the
  core doc comment, and change its pointer from `deferred_resume.rs` to
  `crates/paigasus-helikon-core/tests/common/mod.rs`. Keep the sentence
  "Keep the two copies the same." *My call:* remove "(pre-flight ruling F5)",
  because it refers to a SMA-803 working document that is not in the repo
  history a reader has. Its code does not change.
- **Commit type.** `test(core)` for the code, `docs(spec)` / `docs(plan)` for
  the design files. The PR title is `test(core): SMA-826 move the shared core
  test helpers into tests/common`.

## Release

`test(core)` does not stop a release. Neither crate manifest has `include`
or `exclude`, so `tests/` is in each published package. release-plz
attributes a bump by file path, and `.versionrc` controls only convco. Core
0.5.21 and 0.5.5 shipped from commits that were not `feat` or `fix`.

So this PR makes release-plz propose a patch for core and runtime-tokio,
and `dependencies_update` cascades to the dependents and the facade. Keep
`test(core)`: it is the correct type, and it prevents a minor bump.

The SMA-810 core patch (`e7ac5377`) is on `main` and is not released. Release
PR #357 carried it, but #357 was closed without a merge at 18:54 UTC on
2026-11-20, and no release PR is open. The next push to `main` makes
release-plz open a new release PR. If this PR merges before that release PR
merges, the test changes go into the same patch as SMA-810. If a release PR
merges first, this PR makes a release that holds only test changes. That is
harmless but noisy. The merge order is Sven's call.

## Verification

- Before and after the move, record `cargo test -p paigasus-helikon-core
  --test deferred_resume --test handoff_transfer_result --test
  subagent_propagation -- --list` and compare the test names.
- `cargo test -p paigasus-helikon-core` and
  `cargo test -p paigasus-helikon-runtime-tokio --test deferred` pass.
- `cargo fmt --all -- --check` and the workspace clippy gate pass.
- `grep -rnE '^(pub )?(struct (DeferB|RequestLog|CapturingModel)\b|fn assert_tool_calls_paired\b)' crates/paigasus-helikon-core/tests`
  gives three lines, all in `common/mod.rs`, and no `CapturingModel`.
- Diff the shared oracle against `git show main:crates/paigasus-helikon-core/tests/deferred_resume.rs`
  (lines 579-619). Only `pub` and the doc comment may differ. Diff the
  runtime-tokio function against the shared one. Only the doc comment and
  `pub` may differ.

## Left to the builder

- Where in `common/mod.rs` the helpers go (at the end is fine).
