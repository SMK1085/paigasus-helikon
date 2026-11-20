# SMA-763: remove the RUSTSEC-2026-0459 ignore — design

## Intent

On 2026-11-12, RUSTSEC-2026-0459 made the `audit` and `deny` gates fail on
`main`. The advisory flags `libsqlite3-sys` 0.37.0. That crate bundles C
SQLite with CVE-2026-119375. The fix is `libsqlite3-sys` >= 0.39. At that
time, `sqlx-sqlite` 0.9.0 capped `libsqlite3-sys` at `<0.38.0`, so Sven
accepted a temporary ignore in `.cargo/audit.toml` and `deny.toml`. This
ticket removes the ignore.

## Current state (checked 2026-11-20)

- `sqlx` 0.9.1 was released on 2026-11-18. Its `sqlx-sqlite` 0.9.1 requires
  `libsqlite3-sys >=0.30.1, <0.41.0`. 0.9.0 required `<0.38.0`.
- The release PR #350 (`7ed91e7e`) ran `cargo update`. `Cargo.lock` on `main`
  now has `sqlx` 0.9.1 and `libsqlite3-sys` 0.40.0. The bundled SQLite moved
  from 3.51.3 to 3.54.1.
- On PR #350 and on the `main` push of `7ed91e7e`, all six `test` legs and
  `sessions-it` passed. Thus this PR does not change the build graph, and it
  needs no extra SQLite testing.
- With both ignore entries removed, `cargo audit --deny warnings` and
  `cargo deny check advisories` return exit 0 on that tree.
- The workspace pin is `sqlx = { version = "0.9", ... }` in the root
  `Cargo.toml`. The published `paigasus-helikon-evals`,
  `paigasus-helikon-sessions-sqlite` and `paigasus-helikon-sessions-postgres`
  thus accept `sqlx` 0.9.0, which can only resolve a vulnerable
  `libsqlite3-sys`.
- `paigasus-helikon-cli` 0.1.41 was published from #350 with
  `libsqlite3-sys` 0.40.0 in its lockfile. Thus `cargo install --locked`
  already gets the fix.

## What the floor raise does, and what it does not do

- **It does:** exclude `sqlx` 0.9.0. A dependency graph that is pinned to
  `sqlx` 0.9.0 cannot resolve our new releases. Thus `sqlx` no longer
  blocks a fixed `libsqlite3-sys` in a graph that uses our new releases.
  Another dependency can still block it. For example, `rusqlite` 0.37.0
  requires `libsqlite3-sys ^0.35.0`, and `links = "sqlite3"` allows only
  one version in a graph.
- **It does not:** move `libsqlite3-sys` in an existing lockfile.
  `sqlx-sqlite` 0.9.1 still accepts 0.37.0. A user who runs
  `cargo update -p paigasus-helikon-sessions-sqlite` gets `sqlx` 0.9.1, but
  keeps `libsqlite3-sys` 0.37.0. A minimal-versions resolve picks 0.30.1. The
  user must also run `cargo update -p libsqlite3-sys`, and then check the
  resolved version with `cargo tree -i libsqlite3-sys`. If another
  dependency caps it below 0.39, the user must update or remove that
  dependency too. The PR body and the
  release notes must say this.

## Goals

1. Remove the `RUSTSEC-2026-0459` entry and its comment block from
   `.cargo/audit.toml` and from `deny.toml`. In each file, put a one-line
   historical note in its place, as for the removed rsa and async-std
   entries. Write the ID without quotes, because
   `scripts/check-advisory-ignore-sync.sh` counts quoted `"RUSTSEC-…"`
   strings as entries.
2. Raise the workspace pin from `sqlx = "0.9"` to `sqlx = "0.9.1"`. Keep the
   `default-features` and `features` keys unchanged. Add a two-line comment
   above the pin that names SMA-763 and the `sqlx-sqlite` 0.9.0 `<0.38.0`
   cap, as for the jiff floor.
3. The CI supply-chain gates pass with no ignore for this advisory.

## Non-goals

- No direct dependency on `libsqlite3-sys` to force `>= 0.39`. The crate
  declares `links = "sqlite3"`, so a graph can hold only one version, and
  `sqlx` deliberately keeps the range flexible.
- No hand bump of crate versions. release-plz performs the bumps and the
  cascade after merge (see the fallback below).
- No change to the other ignore entries.
- No edit to `crates/paigasus-helikon-sessions-postgres/Cargo.toml:19`. Its
  "sqlx 0.9.0 feature name" comment is still true for 0.9.1.

## Approaches

| Approach | Trade-off | Decision |
|---|---|---|
| Remove the ignore and raise the floor to `0.9.1` | Excludes `sqlx` 0.9.0 from every graph that uses our new releases. It does not move `libsqlite3-sys` in an existing lockfile. Costs a patch release of evals, sessions-sqlite and sessions-postgres, plus the CLI and facade cascade. | **Chosen by Sven on 2026-11-20.** The corrected trade-off needs his confirmation at Gate 1. |
| Remove the ignore only | No crate changes, no release. Graphs pinned to `sqlx` 0.9.0 still resolve our crates. In both approaches, users must run `cargo update -p libsqlite3-sys` to get the fix. | Rejected. |
| Add a direct `libsqlite3-sys >= 0.39` dependency | Forces the fix, but adds a `links` dependency that `sqlx` keeps flexible. It can conflict with other SQLite users. | Rejected. |

## Constraints

- Commit and PR type is `chore(deps)`. The scope `deps` is in `.versionrc`.
  release-plz bumps a crate when only a `[workspace.dependencies]`
  requirement changes. Precedent: the SMA-818 jiff pin change gave evals
  0.1.20 and sessions-sqlite 0.1.28 an "update Cargo.toml dependencies"
  entry.
- `Cargo.lock` must not change against the PR base. Check with
  `git diff --exit-code origin/main -- Cargo.lock`. A plain
  `git diff --exit-code Cargo.lock` misses a change that is already
  committed.
- Gates to run before push:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-features --all-targets -- -D warnings`
  - `cargo test --workspace --all-features` (evals' `sqlx` is behind the
    `trace-sqlite` feature, so a per-crate test without features does not
    compile it)
  - `bash scripts/check-advisory-ignore-sync.sh`
  - `cargo audit --deny warnings`
  - `cargo deny --all-features check` (the CI command in `deny.yml`)
  - `cargo package -p paigasus-helikon-sessions-sqlite --no-verify`, then
    read `<name>-<version>/Cargo.toml` from the archive
    `target/package/<name>-<version>.crate` (`--no-verify` does not unpack
    it) and confirm that it has `sqlx` version `0.9.1`. Use the exact
    archive and member names, not globs: GNU tar needs `--wildcards` for a
    member pattern, and a glob can match an older archive.
- No mdBook or README text names the `sqlx` version, so no doc edit is
  needed. This is a conscious call.

## Rollback

If `sqlx` 0.9.1 is yanked before a later 0.9.x release, a fresh resolve of
our new releases fails. Existing lockfiles still build. To roll back:

1. Revert the pin to `"0.9"` and restore the ignore in both policy files.
2. Release new versions of evals, sessions-sqlite and sessions-postgres
   (and the cascade) with the relaxed requirement. A repository revert
   alone does not change manifests that are already on crates.io.
3. Tell users to update to those versions. The releases that require 0.9.1
   cannot be fixed in place.

The trade-off: under `sqlx` 0.9.0, no fixed `libsqlite3-sys` resolves. A
rollback thus brings back the RUSTSEC-2026-0459 exposure and its ignore.

## After merge

1. Confirm that the release-plz PR bumps evals, sessions-sqlite,
   sessions-postgres, the CLI and the facade. If it leaves out any of the
   three `sqlx` crates, hand-bump that crate per CLAUDE.md: the crate
   version, its `[workspace.dependencies]` pin, its CHANGELOG, and the
   facade. Core needs no bump.
2. In the release PR, edit the CHANGELOG sections of the three `sqlx`
   crates. Name RUSTSEC-2026-0459 and `cargo update -p libsqlite3-sys`.
   release-plz otherwise writes only "update Cargo.toml dependencies".
3. Watch the release PR CI.

## Follow-ups (not in scope)

- The workspace `sqlx` base enables the `sqlite` feature, so every
  sessions-postgres user compiles bundled C SQLite. Moving `sqlite` into the
  sessions-sqlite and evals manifests removes that code for Postgres-only
  users.
- The paste ignore comment says "parquet (59.0.0", but the pin is parquet 60.
- `.github/dependabot.yml:16-21` refers to a `=0.2.28` jiff pin that SMA-818
  removed.

## Left to the builder

- The exact wording of the commit message and the comments, within the
  conventions above.
