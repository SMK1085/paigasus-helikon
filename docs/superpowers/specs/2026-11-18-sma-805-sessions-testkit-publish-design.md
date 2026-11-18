# SMA-805: publish `paigasus-helikon-sessions-testkit` to crates.io

Status: approved by Sven on 2026-11-18 (revised after the spec challenge).
Linear: SMA-805. Blocks SMA-790.

## Intent

A `Session` implementation outside the Helikon repository must run the same
conformance suite as the built-in backends. The first user is `PgRunSession`
in the always-on agent host. That host implements `Session` itself, because it
writes session events inside its own run-commit transaction (ADR-0028,
decision (b)). It adds the testkit as a dev-dependency from crates.io.

Today the testkit has `version = "0.0.0"` and `publish = false`, and
`release-plz.toml` has a `release = false` block for it.

## Goals

- The testkit is on crates.io, and an external crate can run `run_all` against
  its own `Session`.
- The version that carries the deferred-tool-calls change (SMA-803) also
  carries a conformance test for `SessionEvent::ToolDeferred`.
- The docs tell an external backend author how to run the suite.

## Non-goals

- No facade re-export. The facade has no testkit feature, and this change does
  not add one. Consumers add the testkit as a dev-dependency.
- No change to `paigasus-helikon-core`, and no change to `SessionEvent`.
  SMA-803 owns `ToolDeferred`.
- No new conformance tests in PR 1. PR 1 publishes the existing four tests.
- No Trusted Publishing. The `release-plz` workflow publishes with the
  `CARGO_REGISTRY_TOKEN` environment secret.

## Anti-goals

- A first publish that fails and stalls the `release-plz` workflow. The
  `release` step runs before the `release-pr` step, and `release-pr` has no
  `if:` (`.github/workflows/release-plz.yml:51-62`). A failed `release` step
  thus also stops the release PR, and with it the SMA-803 core release.
- A testkit that compiles only inside this workspace.

## Constraints and facts

- The crate name is free on crates.io (the API returned 404 on 2026-11-18).
  It was never claimed as a `0.0.0` stub.
- **A first publish of a new name needs the crates.io `publish-new` scope.**
  The SMA-745 design rotated the token to "publish-update scope"
  (`docs/superpowers/specs/2026-10-31-sma-745-release-plz-workflow-ref-403-design.md:154`).
  After that rotation, no new crate name was published. So the scope of the
  current token is not known. See "Preflight".
- **The testkit publishes when PR 1 merges, not when the release PR
  merges.** PR 1 bumps the testkit's own version. On the push to `main`, the
  `release` step publishes every crate whose version is not on the registry.
  `release-plz.toml` has no `release_always` key, so the default (`true`)
  applies. The precedent: `-sessions-postgres` and `-sessions-redis` first
  published this way, and their CHANGELOGs have no release-plz `0.1.0` entry.
- `lib.rs` uses only public items from `paigasus-helikon-core`:
  `ContentPart`, `Item`, `SequenceId`, `Session`, `SessionEvent` and
  `project`. The registry core (0.5.21) has all of them, so PR 1 needs no
  core bump and thus no facade bump (CLAUDE.md, SMA-321 and SMA-346 caveats).
- `async-trait` and `futures-util` are unused in the testkit. Remove them
  before the first publish, so that consumers do not compile them.
- The three backends declare the testkit as a path-only dev-dependency. Cargo
  removes it from a published manifest, so the backends publish as before.
  No cycle forms, because core does not dev-depend on the testkit.
- `scripts/check-release-gaps.sh` gets the publishable set from
  `cargo metadata`. Before the first publish, crates.io returns 404 for the
  testkit, and the script reports "could not check" (exit 2). On a green
  `release` step that is only a `::warning::`. After the publish, the script
  checks the testkit like every other crate.
- `SessionEvent` is `#[non_exhaustive]`, so a new variant (SMA-803) is an
  additive core change.
- `SessionEvent`, `ConversationSnapshot` and `Item` do not derive
  `PartialEq`. A test that compares whole values must compare their
  `serde_json::Value` forms.
- The testkit's public API exposes core types (`Arc<dyn Session>`). The
  consumer's crate and the testkit must resolve the same core version, or the
  build fails with "expected `dyn Session`, found `dyn Session`". release-plz
  gives dependents only a patch bump when core makes a minor bump (for
  example, core 0.4.1 to 0.5.0 and sqlite 0.1.7 to 0.1.8 on the same day).

## Approaches

- **A. Two PRs (picked).** PR 1 publishes the existing suite as `0.1.0` now.
  PR 2 adds the `ToolDeferred` test after SMA-803 merges, and goes out in the
  same release as the SMA-803 core change. SMA-790 can start now, and the
  first-publish risk is separate from SMA-803. The cost: for a short time
  `0.1.0` has no `ToolDeferred` coverage.
- **B. One PR after SMA-803 merges.** One clean release, but this work and
  SMA-790 wait for SMA-803.
- **C. Stack on the SMA-803 branch.** Fragile, because the SMA-803 spec can
  still change at its gate.
- **D. Ship the suite as an optional module of core** (raised by the spec
  challenge). This removes the core version skew by design. Rejected: it
  changes core, which SMA-803 and SMA-804 edit now; it adds a `tokio`
  dependency to core behind a feature; and the ticket names the testkit
  crate.

Sven picked A on 2026-11-18.

## Preflight (before PR 1 merges)

Sven checks the token in the `release` environment at
crates.io/settings/tokens:

1. The endpoint scopes include `publish-new`.
2. The crate scope is empty, or a pattern that matches
   `paigasus-helikon-sessions-testkit` (for example `paigasus-helikon*`).
3. The token has not expired.

If a check fails, rotate the `release` environment secret
(`CARGO_REGISTRY_TOKEN`) first. The agent cannot see token scopes.

Result: Sven confirmed the token scope on 2026-11-18.

Also: merge PR 1 only when no `release-plz` run is queued or in progress, and
when no PR that changes `.github/workflows/*` is about to merge (SMA-745
rule).

## Design: PR 1 (this branch)

1. **`crates/paigasus-helikon-sessions-testkit/Cargo.toml`**
   - `version = "0.1.0"` (the runbook's ascend step).
   - Remove `publish = false`.
   - Remove the unused `async-trait` and `futures-util` dependencies.
   - New description, without "Internal:": "Session conformance suite for
     Paigasus Helikon backends, including your own."
   - All other metadata stays inherited from `[workspace.package]`. Cargo
     detects `README.md` by default, as for every other crate.
2. **`Cargo.lock`**: the testkit entry changes to `0.1.0`, and the two
   removed dependencies leave its dependency list.
3. **`release-plz.toml`**: remove the testkit `[[package]]` block. Replace
   the "Stubs" comment and the "as the testkit above" comment with one
   comment about the three internal test-harness members that remain.
4. **`CHANGELOG.md`** for the testkit: write it by hand, with
   `## [Unreleased]` and a `## [0.1.0]` section. release-plz never proposes
   `0.1.0`, because the `release` step publishes it first. Precedent:
   `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md`.
5. **Testkit README and crate-level rustdoc**: rewrite them for an external
   user. The "how to run the suite" content goes here:
   - `cargo add --dev paigasus-helikon-sessions-testkit`, and a `tokio`
     dev-dependency with `macros` and `rt-multi-thread`.
   - A test that calls `run_all` with a factory. The factory returns
     `Arc<dyn Session>` and must give a fresh, empty session on each call. For
     a shared database, show a fresh id per call (a process id plus a
     counter, as in `sessions-postgres/tests/conformance.rs`).
   - `run_all` must run inside a Tokio runtime, because
     `run_concurrent_writers` calls `tokio::spawn`. Recommend
     `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`, so that
     the 16 writers really run in parallel. The default current-thread
     runtime also works, but it does not test real parallelism.
   - The individual `run_*` functions are public, so a backend can run a
     subset.
   - **Same core version.** Your crate and the testkit must resolve the same
     `paigasus-helikon-core`. Check with
     `cargo tree -i paigasus-helikon-core`. If it fails with
     "specification ... is ambiguous", the crate and the testkit resolve two
     core versions.
   - **Stability.** `run_all` gets new sub-tests when the `Session` contract
     grows (for example `ToolDeferred` in a later `0.1.x`). On 0.x,
     release-plz releases such an addition as a patch. A `cargo update` can
     thus make a passing backend fail. That is the purpose of the suite. To
     control the upgrade, pin the version (`=0.1.x`).
   - The crate-level rustdoc carries one runnable example: a
     `#[tokio::main(flavor = "multi_thread")] async fn main()` that calls
     `run_all` with `MemorySession` (a public core export). It compiles and
     runs as a doctest, so it cannot drift. The README shows the
     `#[tokio::test]` form as text. The README is not pulled into rustdoc with
     `include_str!`.
6. **Docs that say "internal, never published"**: bring them into line.
   - `README.md` (root): the "Twenty are published ... one is an internal
     test harness" line becomes "all twenty-one are published", and the crate
     list gets a testkit bullet.
   - `CLAUDE.md`: the workspace-layout paragraph.
   - `docs/book/src/introduction.md`: "Twenty crates are published".
   - `docs/book/src/reference/crates.md`: the paragraph above the table, the
     testkit row (docs.rs link, `published`), the paragraph below the table,
     and "Like `-sessions-testkit`, all three are ...".
   - `docs/book/src/reference/api-docs.md`: the "Published crates" list gets
     a testkit entry, and the "20 non-internal crates" count changes.
   - `docs/book/src/getting-started/workspace-layout.md`.
   - `docs/book/src/concepts/sessions.md`: the "Backend conformance" section.
     Remove "internal", fix the stale "forthcoming" wording for Postgres and
     Redis, and add a short "run it against your own backend" part that links
     to the testkit docs.
   - `docs/runbooks/release-ascend.md`: the status snapshot, plus a
     `publish-new` token-scope note in the ascend recipe and the "If the first
     publish fails" steps below.
7. **PR title**: `chore(sessions): SMA-805 publish the sessions testkit to crates.io`.
   PRs are squash-merged, so only this title reaches `main`. `chore` follows
   the CLAUDE.md rule for a change to `release-plz.toml`. The scope `sessions`
   is on the `main` allowlist. Branch commits use `chore(release)` and
   `docs(...)`.

### Verification for PR 1, before the merge

- `cargo package -p paigasus-helikon-sessions-testkit` succeeds. This builds
  the tarball against the registry core, the same check that `release-plz`
  makes.
- `cargo publish -p paigasus-helikon-sessions-testkit --dry-run` succeeds.
- **A check from outside the workspace.** A scratch crate in the scratchpad,
  outside the workspace, depends on the packaged
  `target/package/paigasus-helikon-sessions-testkit-0.1.0` by path and on
  core from the registry. It runs the README example against
  `MemorySession` and passes.
- The CI gates in CLAUDE.md pass locally: fmt, clippy, test, doc, the
  doc-coverage script, markdownlint, `mdbook build docs/book`, and the
  release-gap self-test.

### Verification for PR 1, after the merge

Check the `release-plz` run of the PR 1 merge commit:

1. crates.io lists `paigasus-helikon-sessions-testkit` `0.1.0`.
2. The tag `paigasus-helikon-sessions-testkit-v0.1.0` exists.
3. A non-draft GitHub release exists for that tag.
4. The gap report says that there are no gaps.

Do not expect the testkit in the release PR.

### If the first publish fails

1. Read the run log, not only the conclusion. A red run can still have
   published crates.
2. Fix the cause (for example, rotate the token).
3. Re-run the failed run. Confirm that a new attempt really started, because
   `gh run rerun` can exit 0 and start nothing. `release-plz.yml` has no
   `workflow_dispatch`.
4. If the cause cannot be fixed quickly, land a `chore(release)` PR that puts
   back `publish = false` and the `release = false` block. That unblocks the
   `release-pr` step and the SMA-803 release.

## Design: PR 2 (follow-up, after SMA-803 merges)

- **Gate:** merge PR 2 only after crates.io lists the testkit `0.1.0`, and
  after SMA-803 is on `main`. If `0.1.0` were still unpublished, the PR 2
  merge would publish `0.1.0` with code that uses `ToolDeferred`, and
  `cargo publish --verify` would build it against a registry core without the
  variant (the SMA-321 deadlock).
- Add a `run_tool_deferred_round_trip` test (the name is the builder's call)
  and call it from `run_all`. It appends a `ToolCalled` and a `ToolDeferred`
  for the same `call_id` and asserts:
  - `events(None)` returns both events in order, with field values equal by
    `serde_json::Value` comparison;
  - the `serde_json::Value` of `snapshot()` equals that of
    `project(&events(None))`.
  - Use whole-second timestamps, as the existing tests do. A backend that
    stores `timestamptz` keeps only microseconds.
- This adds `serde_json` (workspace pin) to the testkit's dependencies.
- The test asserts only backend behaviour (storage round-trip and projection
  parity). It does not assert the `project()` rule for `ToolDeferred` itself,
  because that rule belongs to core and its own tests.
- The exact fields come from the SMA-803 owner after Sven approves the SMA-803
  spec. The current draft: `call_id`, tool name, effective args, active agent
  name, prompt, `ts`, serde tag `tool_deferred`.
- No hand version bump. release-plz bumps the testkit (patch) and core in the
  same release PR, and publishes core first.
- PR 2 must merge before the release PR that carries SMA-803. If that release
  PR merges first, the testkit publishes in the next release. This only
  delays the "Done when" condition.

## Considered and not done

- **A release rule to minor-bump the testkit when core makes a minor bump**
  (spec challenge, MAJOR (b)). It removes surprise skew, but it is a manual
  step on every core minor release. This spec documents the skew and the
  `cargo tree` check instead. Sven can decide on the rule at the gate.

## Open questions

1. The token scope: answered (see "Preflight").
2. Still open, for SMA-790 (Sven was not sure on 2026-11-18). Can `PgRunSession::append` run outside the host's run-commit transaction,
   with 16 callers at the same time? The suite calls `Session::append`
   directly. If it cannot, the host needs a test-only setup to run the suite.
   This does not change the testkit, but it decides whether Goal 1 is met for
   the first user.

## Left to the builder

- The exact README wording and the order of sections.
- Whether the docs commits are one commit or several.

## Coordination

- SMA-803 (session `paigasus-helikon-aa`) and SMA-804 (session
  `paigasus-helikon-32`) run in parallel. Neither edits the testkit,
  `release-plz.toml` or the backend `Cargo.toml` files. Both edit core only.
- SMA-803 will send this work its final `ToolDeferred` definition.
