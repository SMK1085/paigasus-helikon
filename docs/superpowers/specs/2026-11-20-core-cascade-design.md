# SMA-838: give the dependents of a breaking bump a breaking bump

Linear: [SMA-838](https://linear.app/smaschek/issue/SMA-838/release-the-dependents-of-a-core-minor-bump-as-a-minor-bump-not-a).

## Intent

When `paigasus-helikon-core` gets a breaking bump (0.6 → 0.7), release-plz
gives each dependent only a patch bump. Every library dependent exposes core
types in its public API, so the patch is not semver-correct. A user who
requires core 0.6 runs `cargo update`, gets a dependent patch that requires
core 0.7, and has two copies of core in one build ("trait not implemented").

Release PR #361 (merged 2026-11-20) did exactly this. It published core 0.7.0,
facade 0.7.0, CLI 0.1.44, and patch versions of 17 library dependents. The same
class happened before between siblings: on 2026-08-14 runtime-temporal went
0.3.1 → 0.4.0 while the facade, which re-exports it, went 0.5.2 → 0.5.3.

The work lands as two PRs, in this order:

1. **PR 1, repair.** Publish the 17 dependents again as breaking versions, and
   facade 0.7.2 and CLI 0.1.46 on top of them. Then Sven yanks the 19 patch
   versions, facade 0.7.0 and 0.7.1, and CLI 0.1.44 and 0.1.45.
2. **PR 2, prevention.** A guard that fails a PR whose version changes give a
   crate a breaking bump without a breaking bump of each of its dependents,
   plus the written rule. PR 2 lands after PR 1 has published, so its own diff
   changes no version.

## Goals

- The 17 library dependents are on crates.io at their next breaking version
  (table below), each requiring core 0.7.
- Facade 0.7.2 and CLI 0.1.46 are on crates.io and require the new versions.
- After that, Sven yanks the 19 patch versions, facade 0.7.0 and 0.7.1, and CLI 0.1.44 and 0.1.45.
- `scripts/check-breaking-cascade.sh` fails when a publishable crate gets a
  breaking bump and a non-exempt dependent of it does not. A self-test pins
  its behaviour. Both run in the required `fmt` job.
- `docs/runbooks/release-ascend.md` and `CLAUDE.md` describe the rule and the
  breaking-marker procedure, and the wrong `dependencies_update` text is
  corrected in all three places.

## Non-goals

- No change to release-plz itself, no `version_group`, no switch away from
  release-plz.
- No guard step in `release-plz.yml`. The release PR is a PR, so the `fmt`
  gate already runs on it before the merge that publishes. The checks
  ruleset has no bypass actors (`main-protection-checks.json:11`), so a red
  `fmt` blocks the merge even with `--admin`.
- No fix of the wrong upgrade note in `crates/paigasus-helikon-core/CHANGELOG.md`
  in this work (see "No file in core or macros").

## Anti-goals

- A version that users on core 0.7 can resolve disappears before its
  replacement is published. (So: publish first, yank second.)
- The guard passes vacuously: it cannot read the base versions, finds no
  dependents, or runs on an unexpected event. It must fail loudly instead.
- The guard blocks a normal PR whose versions do not change, or a PR that adds
  a new crate.
- PR 1 or PR 2 touches a file in `crates/paigasus-helikon-core/` or
  `crates/paigasus-helikon-macros/`.

## Facts found

- **Exposure.** Every crate except `macros` has core in `[dependencies]`.
  Every one of them except the CLI exposes core in its public API, for example
  `impl Session for SqliteSession` (`sessions-sqlite/src/lib.rs:126`),
  `impl Model for OpenAiModel` (`providers-openai/src/model.rs:54`), and the
  facade's `pub use paigasus_helikon_core as core;` (`src/lib.rs:4`). The CLI
  library is internal (no stability guarantee, `cli/src/lib.rs:1-4`). `macros`
  has core only as a path-only dev-dependency. All sibling dev-dependencies are
  path-only, so there is no publish-time cycle.
- **release-plz cannot propagate a breaking bump** (v0.4.3 source,
  `command/update/updater.rs`): a dependent with no commits of its own gets a
  hard-coded `patch_increment`. Issue release-plz#1599 is closed: release-plz
  does not inspect types. `changelog_include` is capped at a patch.
  `version_group` would give all members one version. cargo-semver-checks has
  the same gap (cargo-semver-checks#447, open).
- **A breaking commit that touches a crate's directory does work.** On 0.x, a
  `!` commit gives a minor bump to each crate whose directory it touches
  (#177, #367).
- **release-plz compares each package with its registry copy.** A change to
  any packaged file of a crate opens a release for it. So an edit to the core
  CHANGELOG would release core 0.7.1 and give patch bumps to all 19 crates
  again.
- **release-plz cannot detect yanked versions** (release-plz docs, "yanked
  packages"; release-plz#524). If the local version is the yanked one,
  release-plz refuses to update the crate. Publishing the new versions before
  the yank avoids this.
- **A hand bump publishes on its own merge**, in dependency order, with no
  separate release PR (`release-ascend.md:36,40`). Hand-written `## [x.y.z]`
  CHANGELOG sections are the precedent (SMA-622 spec, 2026-09-07).
- **The `dependencies_update` text is wrong** in `release-plz.toml:5-9`,
  `release-ascend.md:40` and `CLAUDE.md`. The option only makes release-plz
  run `cargo update` on the lockfile. It does not cascade bumps.
- **Book pins.** `docs/book/src/concepts/model-providers.md:282` pins
  `paigasus-helikon-providers-gemini = { version = "0.1" }`, and `:402-410` pin
  the facade at `"0.5"`. No other hardcoded sibling `version =` pin exists in
  `crates/`, the READMEs or the examples.
- **CLI 0.1.44** requires evals ^0.1.22, runtime-tokio ^0.1.27, openai ^0.2.34,
  anthropic ^0.1.29 and mcp ^0.1.26 (`cli/Cargo.toml:25-29`). After the yank,
  only `cargo install --locked` can install it.
- **CI.** The `fmt` job checks out with depth 1 (the PR merge commit) and runs
  one guard and two self-tests (`ci.yml:56-67`). The release-plz bot pushes
  with its App token, so each new version of the release PR runs `fmt` again.
- `cargo yank` needs Sven's crates.io token, and `cargo login` is interactive
  only, so Sven runs the yanks.

## Approaches

- **A. Breaking-marker rule and a release guard (picked).** A PR that breaks a
  crate uses a `!` title and adds an upgrade note to the CHANGELOG of each
  dependent, so release-plz gives each a minor bump. A guard script in the
  `fmt` job fails any PR (in practice the release PR) where the cascade is
  missing.
- **B. Hand-bump rule and the same guard.** Rejected: hand bumps carry the
  dangers listed in `CLAUDE.md`, and each break would repeat a repair.
- **C. Guard only.** Rejected: the guard can only fail the bot's release PR,
  and a hand fix on that PR does not survive the next regeneration.

## Design

### No file in core or macros

Neither PR may change a file in `crates/paigasus-helikon-core/` or
`crates/paigasus-helikon-macros/`. A change there opens a release of core
0.7.1, which restarts the patch cascade. The wrong note in the core 0.7.0
CHANGELOG stays until core's next real release. Instead, Sven (or the
coordinator, with Sven's approval) edits the body of the GitHub release
`paigasus-helikon-core-v0.7.0` to correct it.

### PR 1: the repair

Branch `feature/sma-838-release-the-dependents-of-a-core-minor-bump`. PR title:
`chore(release): SMA-838 republish the core 0.7 dependents as breaking
versions`. A `chore` title, not `!`: the versions are set by hand, and
release-plz then only publishes them.

| Crate | Published by #361 (yank) | New version |
|---|---|---|
| `paigasus-helikon-evals` | 0.1.22 | 0.2.0 |
| `paigasus-helikon-mcp` | 0.1.26 | 0.2.0 |
| `paigasus-helikon-providers-anthropic` | 0.1.29, 0.1.30 | 0.2.0 |
| `paigasus-helikon-providers-bedrock` | 0.1.11 | 0.2.0 |
| `paigasus-helikon-providers-gemini` | 0.1.11 | 0.2.0 |
| `paigasus-helikon-providers-litellm` | 0.1.11 | 0.2.0 |
| `paigasus-helikon-providers-openai` | 0.2.34 | 0.3.0 |
| `paigasus-helikon-runtime-actix` | 0.2.11 | 0.3.0 |
| `paigasus-helikon-runtime-agentcore` | 0.2.16, 0.2.17 | 0.3.0 |
| `paigasus-helikon-runtime-axum` | 0.2.11 | 0.3.0 |
| `paigasus-helikon-runtime-temporal` | 0.5.5 | 0.6.0 |
| `paigasus-helikon-runtime-tokio` | 0.1.27 | 0.2.0 |
| `paigasus-helikon-sessions-postgres` | 0.1.11 | 0.2.0 |
| `paigasus-helikon-sessions-redis` | 0.1.10 | 0.2.0 |
| `paigasus-helikon-sessions-sqlite` | 0.1.30 | 0.2.0 |
| `paigasus-helikon-sessions-testkit` | 0.1.3 | 0.2.0 |
| `paigasus-helikon-tools` | 0.2.26 | 0.3.0 |
| `paigasus-helikon` (facade) | 0.7.0, 0.7.1 | 0.7.2 |
| `paigasus-helikon-cli` | 0.1.44, 0.1.45 | 0.1.46 |

Changes:

1. Each crate's `Cargo.toml` `version` per the table.
2. Root `Cargo.toml` `[workspace.dependencies]`: each sibling pin's `version`
   per the table, and the facade self-pin to 0.7.2. Check for any other
   version-carrying reference to `sessions-testkit`.
3. `Cargo.lock` via a normal build.
4. A hand-written `## [<new>]` section with today's date under
   `## [Unreleased]` in each of the 19 CHANGELOGs, in the format release-plz
   writes (compare link to the previous tag). The 17 dependents' entry says:
   - this version replaces `<yanked>`, which requires core 0.7 but was released
     as a patch; the content is the same; it requires `paigasus-helikon-core`
     0.7;
   - users who stay on core 0.6 keep the previous version, which `cargo update`
     selects after the yank;
   - users who already moved to core 0.7 must change their requirement on this
     crate to `<new minor>` (for example `"0.2"`). If they do not, `cargo
     update` moves them back to the core-0.6 build after the yank, and the
     build has two copies of core.
   The facade entry says it moves to the new sibling versions, so the type
   identity of each re-exported sibling changes (for example openai 0.2 → 0.3).
   This is accepted as a patch only because facade 0.7.0 is yanked. The CLI
   entry says it moves to the new sibling versions.
5. The facade CHANGELOG: correct the 0.7.0 upgrade note ("the next patch
   release of each other crate requires core 0.7") in place, because the
   facade republishes anyway.
6. The book: `model-providers.md:282` → `version = "0.2"`, and `:402-410` →
   `version = "0.7"`. `mdbook build docs/book` stays clean.

PR 1 does not include the guard, the runbook or the `CLAUDE.md` change.

### PR 2: the guard and the rule

Opened after PR 1 has published and the yanks are done. PR title:
`ci(release): SMA-838 fail a release that breaks a crate without breaking its
dependents`.

`scripts/check-breaking-cascade.sh`, run in the `fmt` job after the existing
guards. The `fmt` checkout gets `fetch-depth: 2`.

- **Base and head.** Head is the working tree. Base, by event:
  - `pull_request`: `HEAD^1`, the base side of the PR merge commit. This needs
    no network, works on forks, and stays the same when the job re-runs.
  - `push`: print a notice and exit 0.
  - Any other event in CI: fail.
  - Local run (no `GITHUB_EVENT_NAME`): the merge-base with `origin/main`.
  - Environment-variable overrides for the self-test, so it needs no git and
    no network.
- **Packages.** From `cargo metadata --no-deps` at the head: each publishable
  workspace member (`publish` not `[]`/false), with its normal (not dev, not
  build) dependencies on other workspace members.
- **Breaking bump.** For `0.y.z`, `y` increases (or the major does). For
  `x.y.z` with `x ≥ 1`, `x` increases. A version that goes down fails with its
  own message.
- **Rule.** If a publishable crate has a breaking bump, each publishable,
  non-exempt workspace crate with a normal dependency on it must also have a
  breaking bump in its own scheme. The rule is not transitive by itself; each
  edge is checked. Failure output lists each dependent with its base version,
  head version and the minimum version it needs.
- **Exemption.** `paigasus-helikon-cli`, with its reason in a comment (binary
  crate; the library is internal).
- **New crates.** A crate that is absent at the base is skipped.
- **Loud failure.** If the base versions cannot be read, or no publishable
  crate is found, the script fails.
- **Message.** The failure text says how to fix it: open a `!` PR that adds an
  upgrade note under `## [Unreleased]` in each listed crate's CHANGELOG, and
  let release-plz regenerate the release PR.

`scripts/check-breaking-cascade-selftest.sh` uses fixture metadata and fixture
versions in `mktemp` directories, like `check-release-gaps-selftest.sh`.
Cases: no version change (pass); core patch with dependent patches (pass);
core breaking with all dependents breaking (pass); core breaking with one
dependent patch (fail, names it); core breaking with an exempt CLI patch
(pass); a sibling breaking with the facade patch (fail, names the facade);
`0.y` → `1.0.0` core with a `1.x` dependent that gets only a minor (fail);
a version that goes down (fail); a new crate absent at base (pass); unreadable
base (fail); empty package list (fail); `push` event (notice, pass); unknown
event (fail).

Docs:

- `docs/runbooks/release-ascend.md`: a section "A breaking change in a crate
  with dependents" with the rule, the `!` and CHANGELOG procedure, what the
  guard checks, and the repair procedure (publish first, then yank) with this
  incident as the example. Correct the `dependencies_update` text at `:40`.
- `CLAUDE.md`: point to that section from the release caveats, and correct the
  `dependencies_update` text.
- `release-plz.toml:5-9`: correct the `dependencies_update` comment.

### Release sequence for PR 1

1. Before the merge: prove that release-plz publishes the 19 versions and
   bumps nothing further, with the SMA-831 scratch-clone method (release-plz
   v0.4.3, squash onto a local `main`), using `release-plz release --dry-run`
   for the publish list and order if it runs without a token, else
   `release-plz update` (expect no version change).
2. Merge PR 1. Its own merge publishes the 19 versions.
3. Do not yank until all 19 versions are on crates.io and have tags. If the
   run fails part way, re-run it; release-plz skips the versions that are
   already published.
4. Check that no release PR opens for the merge commit.
5. Sven runs the 23 yanks (the 19 dependent patch versions in the table, facade 0.7.0 and 0.7.1, CLI 0.1.44 and 0.1.45; see the update note). The
   commands are in the PR body.
6. Check on crates.io that each yanked version shows `yanked: true` and each
   new version does not.
7. Correct the body of the GitHub release `paigasus-helikon-core-v0.7.0`.

## Update 2026-11-20: main moved during implementation

Release PR #369 merged while PR 1 was in progress. It published facade 0.7.1,
CLI 0.1.45, providers-anthropic 0.1.30 (with the SMA-834 fix) and
runtime-agentcore 0.2.17, all as patches that require core 0.7. PR #370 merged
the SMA-832 bedrock fix, not yet released. PR 1 was rebased onto that `main`:

- The facade moves to 0.7.2 and the CLI to 0.1.46 (the next free numbers).
- providers-anthropic 0.2.0 and runtime-agentcore 0.3.0 replace two yanked
  versions each. providers-bedrock 0.2.0 also contains the SMA-832 fix.
- The yank list is 23 versions (table above).
- No `chore: release` PR may merge before PR 1. Each one publishes more patches
  that require core 0.7.

## Left to the builder

- The exact guard implementation (bash plus `jq`, matching the existing
  scripts).
- The exact CHANGELOG wording, in ASD-STE100 style.
- The exact pre-merge proof command (step 1 above).
