# SMA-831: upgrade jiff to 1 as a breaking change of core and the facade

Linear: [SMA-831](https://linear.app/smaschek/issue/SMA-831/upgrade-jiff-to-10-as-a-breaking-change-of-paigasus-helikon-core).
Size: a small change. This is the short design.

## Intent

jiff 1.0.0 was released on 2026-11-15. Dependabot PR #363 bumped the
requirement as a `chore(deps)` change, which release-plz releases as a patch.
That is wrong: `jiff::Timestamp` is public API of `paigasus-helikon-core`
(the `ts` fields of the `SessionEvent` variants, and `SessionEvent::ts()`).
A user who passes a jiff 0.2 `Timestamp` in, or passes the returned value to
jiff 0.2 code, breaks after the upgrade. The upgrade must be released as a
minor bump.

## Goals

- The workspace requires jiff `1`. `Cargo.lock` has jiff 1.x only.
- release-plz makes a **minor** bump of `paigasus-helikon-core` (0.6.0 → 0.7.0).
- release-plz makes a **minor** bump of the facade `paigasus-helikon`
  (0.6.14 → 0.7.0). The facade re-exports core, so the facade break is the same
  break. (Sven's decision, 2026-11-20. It differs from the 0.5 → 0.6 precedent,
  where the facade got a patch.)
- The CHANGELOGs of core and the facade tell users to upgrade to jiff 1.
- A test proves that a sub-second timestamp string that jiff 0.2 wrote still
  reads and writes back to the same bytes with jiff 1.
- All required CI gates pass.

## Non-goals

- No minor bump for the other crates that use jiff (evals, runtime-agentcore,
  sessions-sqlite, sessions-testkit). They use jiff only internally
  (evals `src/run.rs:210`, agentcore `src/a2a/types.rs:51`, testkit
  `src/lib.rs:66,217` are all private). They get the normal patch
  "update Cargo.toml dependencies".
- No change to the general cascade policy (dependents of a core minor get a
  patch). That is a separate question.
- No move of jiff from `[dependencies]` to `[dev-dependencies]` in
  sessions-sqlite, although its `src/` does not use jiff. Separate cleanup.
- No fix for the five `trybuild_ui` mismatches in core. They fail the same way
  on clean `main` with local rustc 1.99.0, and they are not from this change.
- No fix of the `dependencies_update` comment in `release-plz.toml`.

## Anti-goals

- The release PR shows a patch bump of core or the facade.
- Any crate other than core and the facade gets a minor bump.
- The JSON format of a stored `SessionEvent` changes, so old session rows
  cannot be read.

## Constraints and facts found

- A probe compiled the whole workspace (`--all-features --all-targets`) with
  jiff 1.0.1 and no source change. The lockfile moved jiff, jiff-core,
  jiff-static to 1.0.1 and jiff-tzdb, jiff-tzdb-platform to 1.0.0.
- The core `serde_roundtrip` snapshot tests pass with jiff 1. But they use only
  `1970-01-01T00:00:00Z`, so they do not prove the sub-second case. Real rows
  hold `Timestamp::now()` values with nanoseconds.
- jiff-static 1.0 pulls `syn` 3.x next to `syn` 2.x. `deny.toml` has
  `[bans] multiple-versions = "warn"`, so `deny` does not fail on it.
- jiff 1.0 needs Rust 1.85. The workspace MSRV is 1.94.
- release-plz gives a commit to a crate only when the commit changes files in
  that crate's directory. A change to the root `Cargo.toml` alone gives no
  commit to core.
- On a 0.x crate, release-plz turns a breaking commit (`!` after the type, or a
  `BREAKING CHANGE:` footer) into a minor bump, and a `feat` into a patch.
  Precedent: `feat(runtime)!: SMA-482 …` (#177) gave runtime-actix,
  runtime-axum and runtime-agentcore 0.1.x → 0.2.0, and the facade
  0.4.26 → 0.5.0. No precedent has `chore!`, so step 6 must prove it.
- The repo sets `squash_merge_commit_title = PR_TITLE` and
  `squash_merge_commit_message = BLANK`. The squash commit on `main` is thus
  exactly the PR title plus ` (#N)`, with no body. Only the `!` in the title can
  carry the breaking marker; a footer cannot.
- `convco check` accepts `chore(deps)!: SMA-831 upgrade jiff to 1`. The
  `pr-title` regex (`pr-title.yml:85`) accepts the subject, and earlier `!`
  titles passed the gate.
- CI installs the release-plz CLI **v0.4.3** (release-plz run 44807764156).
  The local copy is 0.3.158, so it is not a valid stand-in.
- release-plz keeps hand-written text under `## [Unreleased]` and moves it into
  the release section (precedent: PR #351, the `### Security` notes).
- SMA-810 and SMA-826 are on `main` and not yet released for core. They join
  core 0.7.0.
- Nothing publishes until the release PR merges (`release-plz.yml:51-62`).

## Approaches

- **A. Breaking marker on the squash commit (picked).** The PR title is
  `chore(deps)!: SMA-831 upgrade jiff to 1`. The upgrade notes in the core and
  facade CHANGELOGs make the commit touch both crate directories, so release-plz
  gives the breaking commit to exactly these two crates. Risk: if the `!` is
  removed from the title before the merge, the bump falls back to a patch.
  Step 8 catches that before anything publishes.
- **B. Hand-bump core and the facade to 0.7.0 in the PR.** Rejected. SMA-622
  used a hand bump for `runtime-temporal`, but that crate has only the facade
  as a dependent. Core has about 17 dependents. The release-plz cascade does
  not run for a hand bump, so each dependent's pin and version need hand edits,
  and the PR publishes on its own merge.
- **C. Rely on cargo-semver-checks.** Rejected: it is not certain that it
  detects a major-version change of a type from a dependency.

## Design

1. **Compatibility test first, on jiff 0.2.** Add a core test that
   deserializes a `SessionEvent` whose `ts` is a sub-second RFC 3339 string in
   the form jiff 0.2 writes (for example `2026-11-20T15:08:42.123456789Z`), and
   asserts that serializing it again gives the same bytes. Take the literal from
   real jiff 0.2 output on `main`. The test must pass before the upgrade, and
   again after it.
2. Root `Cargo.toml`: `jiff = { version = "1", features = ["serde"] }`. Replace
   the comment above it. Keep the SMA-818 reason (a caret requirement, not an
   exact pin). Drop the 0.2.29 / RUSTSEC-2026-0173 history, because it applies
   only to 0.2. Keep the statement that `jiff::Timestamp` is public API of core,
   so the next jiff major is a breaking change of core and the facade.
3. `cargo update -p jiff` so the lockfile has jiff 1.x only.
4. `crates/paigasus-helikon-core/CHANGELOG.md` and
   `crates/paigasus-helikon/CHANGELOG.md`: add an `### Upgrade notes` section
   under `## [Unreleased]`. The note says:
   - the `ts` fields of `SessionEvent` and `SessionEvent::ts()` now use
     `jiff::Timestamp` from jiff 1;
   - code that passes a `Timestamp` in or uses the returned value with jiff
     code must change its `jiff` requirement to `1`;
   - code that uses only the constructors that set `ts` to now (for example
     `SessionEvent::user_message`) and does not read `ts` needs no change;
   - upgrade core and the facade together, and upgrade them to 0.7. If the
     lockfile still holds facade 0.6 after a `cargo update`, it can hold two
     copies of core. Check with `cargo tree -d -i paigasus-helikon-core`;
   - the stored JSON format does not change, so old rows stay readable;
   - link the jiff 1.0 changelog for the APIs that jiff itself renamed.
5. mdBook and READMEs: the only jiff reference is
   `docs/book/src/concepts/sessions.md` (`ts: jiff::Timestamp`), with no
   version. It stays correct. No README mentions jiff. No edit.
6. **Verify the bump before the PR.** In a scratch clone with all tags, squash
   the branch onto current `origin/main` as one commit with the message
   `chore(deps)!: SMA-831 upgrade jiff to 1 (#999)` and no body. Install
   release-plz v0.4.3 and cargo-semver-checks into a scratch root. Run
   `release-plz update` and check the full version table: core and the facade
   go to 0.7.0, every other changed crate gets a patch, no other crate gets a
   minor, and the `[Unreleased]` notes move into the 0.7.0 sections.
7. **Allowed diff.** The branch changes only the root `Cargo.toml`,
   `Cargo.lock`, the two CHANGELOGs, the new core test file (inside
   `crates/paigasus-helikon-core`, so it adds no other crate) and
   `docs/superpowers/`. Each branch commit that touches a CHANGELOG also uses a
   `!` type. The PR title is `chore(deps)!: SMA-831 upgrade jiff to 1`, and the
   PR body says that the `!` drives the minor bump and must stay in the title.
8. **Check after the merge.** Before anyone merges the release PR, confirm
   that it shows core 0.7.0 and facade 0.7.0. If it shows patches, do not merge
   it. Recover with a follow-up `chore(deps)!:` PR that touches both
   CHANGELOGs.

## Left to the builder

- The exact wording of the comment and the CHANGELOG notes (ASD-STE100 style).
- How to set up the scratch clone for step 6.
- Dependabot can open a new jiff PR on Monday 06:00 UTC. If this PR is not
  merged by then, close the Dependabot PR. After the merge, the requirement is
  `1`, so Dependabot does not propose jiff 1.0 again.
