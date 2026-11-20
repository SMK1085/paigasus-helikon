# SMA-828 — Share the deferral test helpers between core and runtime-tokio

Status: revised after the spec challenge (Stage 2). Linear: SMA-828.
Precedent: PR #359 (SMA-826).

## Intent

Three deferral test helpers have two copies each: one in the core tests and one
in the runtime-tokio tests. No test fails when a copy drifts. A change to the
tool pairing rule must go into both crates by hand. This change gives each
helper one source that both crates use.

## Goals

These are the "Done when" items of SMA-828:

- `assert_tool_calls_paired`, `RequestLog` and `with_late_transfer_results` each
  have one source, and core and runtime-tokio tests use it.
- The behaviour of each test does not change. The test-name list of each changed
  test binary does not change.
- The doc comments that name "the other copy" are removed.

## Non-goals

These are out of scope. The reason for each is scope, not a technical limit.

- The `MockModel` copies in `crates/paigasus-helikon-core/tests/common/mod.rs`
  and `crates/paigasus-helikon-runtime-tokio/tests/common/mod.rs`.
- `DeferB` (core) and `DeferPolicy` / `PayTool` (runtime-tokio). They defer
  different tools, so they are not copies.
- The `text_of`, `text_turn` and `transfer_turn` helpers in
  `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs` and
  `crates/paigasus-helikon-core/tests/deferred_resume.rs`.
- The `CANCEL` constant in `crates/paigasus-helikon-core/tests/handoff_transfer_result.rs`.
  It checks the text that the current runner writes, not the legacy text.
- `#[track_caller]` or other changes to the helpers.

## Anti-goals

- A test changes its behaviour, or a test binary changes its list of test names,
  its count of passed tests, or its count of ignored tests.
- A new published crate, or a change to the public API of a published crate.
- A versioned dev-dependency between core and its dependants. It can deadlock the
  release-plz publish.
- A cross-crate `#[path = "../../paigasus-helikon-core/tests/..."]`.

## Approaches

1. **A new unpublished workspace member (chosen by Sven).** The member is
   `tests/deferral-testkit`, crate `paigasus-helikon-deferral-testkit`. Core and
   runtime-tokio use it as a path-only dev-dependency.
2. **A published testkit crate**, like `paigasus-helikon-sessions-testkit`.
   Rejected: it changes the crate roster, the READMEs, the mdBook and the release
   flow, and it makes an internal test oracle into public API. Users get no value
   from these helpers.
3. **A `test-util` feature in core.** Rejected: it adds public, semver-checked
   API to core for a test oracle.

### Known limit (accepted, not new)

Cargo removes a path-only dev-dependency from the published manifest. The
published `tests/` files of core and runtime-tokio then import a crate that is
not in the manifest, so the published tests do not compile. The `#[path]` route
has the same failure. `cargo publish` builds only the lib, so the publish is not
affected. The repo used the same release pattern from SMA-330 to SMA-805: the
session backends were published with a path-only dev-dependency on an
unpublished `0.0.0` testkit (`docs/superpowers/specs/2026-06-28-sma-330-production-session-backends-design.md`).
`deny.toml` (`allow-wildcard-paths = true`) and `scripts/check-release-gaps.sh`
already accept this. This change does not add a new type of risk.

## Design

### The new crate

`tests/deferral-testkit/Cargo.toml`:

- `name = "paigasus-helikon-deferral-testkit"`, `version = "0.0.0"`,
  `publish = false`, a `description` that starts with `Internal:`, and the
  workspace-inherited fields, as the other `tests/*` members do.
- `[dependencies]`: `paigasus-helikon-core`, `async-trait`, `futures-core`,
  `futures-util` and `anyhow`, each with `workspace = true`. Add only what the
  code needs.
- `[lints] workspace = true`. Every `pub` item has a `///` doc comment, so
  `missing_docs` and the doc-coverage gate stay green. The doc-coverage script
  reads every workspace member from `cargo metadata`, so it includes this crate.
- The docs contain no code blocks, so the crate adds no doctest.

`tests/deferral-testkit/src/lib.rs` exports:

- `pub fn assert_tool_calls_paired(messages: &[Item])`. The body is the current
  body, with no change. Its doc comment is the current rule text. It adds one
  line that says the oracle's self-test is
  `pairing_oracle_rejects_a_call_after_a_result_while_a_call_waits` in core
  `tests/deferred_resume.rs`.
- `pub struct RequestLog` with `pub fn with_scripts(Vec<Vec<ModelEvent>>) ->
  Arc<Self>` and `pub fn requests(&self) -> Vec<ModelRequest>`, and
  `impl Model for RequestLog`. `invoke` keeps the current order: it pushes the
  request first, then pops one script from its own queue, and it ignores
  `cancel`. It streams the script with `futures_util::stream::iter`. When the
  queue is empty, it returns
  `ModelError::Other(anyhow!("no more scripted responses"))`, the same error as
  both current `MockModel`s. `capabilities()` returns
  `ModelCapabilities::default()`. It does not use either crate's `MockModel`.
  The doc comment is new, because the current one links to `MockModel`, which
  the testkit does not have. A broken intra-doc link fails the `docs` gate.
- `pub async fn with_late_transfer_results(session: &Arc<MemorySession>) ->
  Arc<MemorySession>`. The body is the current body, with no change. The doc
  comment is the general part of the two current comments. It does not name a
  caller and does not name another copy.
- `pub const LEGACY_CANCEL_TEXT: &str`, the core 0.6.0 cancel text that
  `with_late_transfer_results` writes. Its doc comment says: this is the fixed
  core 0.6.0 text; today it is equal to `CANCELLED_TOOL_CALL_TEXT` in core
  `session.rs`; do not change it to follow a later change of that constant.
  `CANCELLED_TOOL_CALL_TEXT` is `pub(crate)`, so the doc names it in backticks
  only, with no intra-doc link.
- A private `text_of`, a copy of the test files' helper. It is not exported.

### Workspace wiring

- Root `Cargo.toml`: add `"tests/deferral-testkit"` to `members`. Do not add the
  crate to `[workspace.dependencies]`, because the dev-dependencies are
  path-only.
- `Cargo.lock`: commit the new `[[package]]` entry.
- `release-plz.toml`: add a `[[package]]` entry with `publish = false` and
  `release = false`. Change the comment "these three workspace members" to
  "these four workspace members".
- `crates/paigasus-helikon-core/Cargo.toml` and
  `crates/paigasus-helikon-runtime-tokio/Cargo.toml`, `[dev-dependencies]`:
  `paigasus-helikon-deferral-testkit = { path = "../../tests/deferral-testkit" }`,
  with no `version`. Its comment says two things. First, it is path-only so that
  cargo removes it from the published manifest. Second (core only), it is for
  integration tests only: code in core `src/` under `#[cfg(test)]` must not use
  it, because that code is a different copy of core and gets error E0308.
- The dev-dependency cycle core → deferral-testkit → core is permitted. The core
  integration tests and the testkit link the same core lib.

### Changes to the tests

- Core `tests/common/mod.rs`: remove `RequestLog` and `assert_tool_calls_paired`,
  with their doc comments. Remove the imports that only they used.
- Core `tests/deferred_resume.rs`, `tests/handoff_transfer_result.rs`,
  `tests/subagent_propagation.rs`: import the helpers from
  `paigasus_helikon_deferral_testkit` directly, not through `common`.
  `deferred_resume.rs` also removes its `with_late_transfer_results` and its
  `CANCELLED`. Its legacy assertion (`[CANCELLED, CANCELLED]`, near line 1654)
  uses `LEGACY_CANCEL_TEXT`.
- runtime-tokio `tests/deferred.rs`: remove `RecordingModel`,
  `assert_tool_calls_paired` and `with_late_transfer_results`, and import them
  from the testkit. Rename each use of `RecordingModel` to `RequestLog`. Keep the
  local `CANCELLED`, because `cancelled_resume_closes_the_deferral` (near line
  1012) checks the text of a real cancel with it. The legacy assertion near line
  1692 uses `LEGACY_CANCEL_TEXT`. Remove the imports that only the deleted code
  used.
- Do not change a test function name, a script, or the value that an assertion
  checks. The only permitted changes in test files are deletions, `use` changes,
  the `RecordingModel` → `RequestLog` rename and the two
  `CANCELLED` → `LEGACY_CANCEL_TEXT` changes above.

### Documentation

`docs/book/src/reference/crates.md`:

- Line 3: "three further internal, non-published Cargo workspace members"
  becomes "four".
- Line 10: add runtime-tokio and core to the named path-only dev-dependency
  exceptions, in the style of that line.
- Line 45: "three further members" becomes "four", and add a bullet for
  `paigasus-helikon-deferral-testkit` that says it is a helper library for the
  core and runtime-tokio tests.
- Line 51: "All three are intentional non-published test harnesses" becomes a
  sentence that covers four members, and says that one of them is a test helper
  library, not a harness.

No README change: no crate's public surface changes. No facade change.
`CLAUDE.md` counts only `crates/`, so it stays.

### Checked, no change needed

The workspace-lints tests need no change. `tracing_target_syntax.rs` and
`tracing_target_docs.rs` scan `crates` and `tests` for `tracing` macros and
`target:` literals, and the testkit has neither. `tracing_target_coverage.rs`
reads only `crates/*/src`. `build-no-default-features`, the cache env sync and
the `sessions-it` filter need no change.

## Verification

1. **Test lists and counts.** On `main` and on the branch, on the stable
   toolchain, run `cargo test -p paigasus-helikon-core -- --list` and
   `cargo test -p paigasus-helikon-runtime-tokio -- --list`. The lists must be
   identical. Also compare the `passed` and `ignored` counts on each
   `test result:` line of `cargo test -p paigasus-helikon-core` and
   `cargo test -p paigasus-helikon-runtime-tokio`. Do not skip `trybuild_ui`:
   trybuild also builds core's dev-dependencies.
2. **The moved bodies.** Diff each moved item against `main`:
   `assert_tool_calls_paired` against `crates/paigasus-helikon-core/tests/common/mod.rs`
   and `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs`;
   `with_late_transfer_results` against `crates/paigasus-helikon-core/tests/deferred_resume.rs`
   and `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs`; and the
   `RequestLog::invoke` order. Only `pub`, the doc comments, the inlined pop and
   the `CANCELLED` → `LEGACY_CANCEL_TEXT` name may differ.
3. **The test-file diffs.** `git diff -U0 main -- crates/*/tests` adds only `use`
   lines, the rename and the two `LEGACY_CANCEL_TEXT` changes.
4. **One source (mutation check, one time, not committed).** Put `panic!()` at
   the start of the testkit's `assert_tool_calls_paired`. Confirm that tests fail
   in both core and runtime-tokio. Revert.
5. **No copy notes remain.**
   `git grep -nE "Keep the two copies|Copy of .assert_tool_calls_paired|A copy is in .crates/|\(A copy of the" -- crates tests`,
   `git grep -n "runtime-tokio/tests/deferred.rs" -- crates/paigasus-helikon-core` and
   `git grep -nE "core/tests/(common/mod|deferred_resume)\.rs" -- crates/paigasus-helikon-runtime-tokio`
   return nothing.
6. **The CI gates.** `cargo fmt --all -- --check`,
   `cargo clippy --workspace --all-features --all-targets -- -D warnings`,
   `cargo test --workspace --all-features`,
   `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`,
   the doc-coverage script, `npx markdownlint-cli2` and `mdbook build docs/book`.
7. **Packaging.** After the commit (or with `--allow-dirty`), run
   `cargo package -p paigasus-helikon-core --no-verify` and the same for
   runtime-tokio. The normalized `Cargo.toml` in each `.crate` file has no
   `paigasus-helikon-deferral-testkit` entry. (`Cargo.toml.orig` keeps the line
   by design.)

## Commits and release effect

Commit scopes from `.versionrc`: `test(core)`, `test(runtime-tokio)`,
`chore(workspace)` and `docs(docs)`. There is no testkit scope, so do not use
one.

The change touches packaged files of core and runtime-tokio (`Cargo.toml`,
`tests/`). release-plz attributes bumps by file path, so it can propose a patch
bump for both crates and the facade cascade. This is harmless, and SMA-826 had
the same effect.

## Left to the builder

- The exact `description` text and the module doc of the new crate.
