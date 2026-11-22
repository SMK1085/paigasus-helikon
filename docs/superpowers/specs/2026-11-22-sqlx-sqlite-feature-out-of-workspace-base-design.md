# SMA-822: Move the sqlx `sqlite` feature out of the workspace base

Linear: [SMA-822](https://linear.app/smaschek/issue/SMA-822). Size: a small change.

## Intent

The root `[workspace.dependencies]` pin of `sqlx` enables the `sqlite` feature.
Every crate that uses `sqlx` inherits it. Thus every user of
`paigasus-helikon-sessions-postgres` compiles `libsqlite3-sys` with bundled C
SQLite, and gets every SQLite advisory (for example RUSTSEC-2026-0459, SMA-763),
although the Postgres backend uses no SQLite code.

The baseline on `main` (52a74cca) has two paths from `sessions-postgres` to
`libsqlite3-sys`:

```text
libsqlite3-sys v0.40.0 └── sqlx-sqlite └── sqlx └── paigasus-helikon-sessions-postgres
libsqlite3-sys v0.40.0 └── sqlx-sqlite └── sqlx-macros-core └── sqlx-macros (proc-macro) └── sqlx
```

In sqlx 0.9.1, both edges depend on the `sqlite` feature
(`sqlite` → `sqlite-bundled` → `sqlx-macros?/sqlite`, and `sqlx-sqlite` is
optional in `sqlx-macros-core`). Thus the removal of the one feature removes
both edges. `macros` and `migrate` do not need `sqlite`.

## Goals

- Only the crates that use SQLite enable the `sqlx` `sqlite` feature:
  `paigasus-helikon-sessions-sqlite` and `paigasus-helikon-evals` (through its
  `trace-sqlite` feature).
- `cargo tree -p paigasus-helikon-sessions-postgres -i libsqlite3-sys -e normal,build`
  finds no path.
- A CI step fails if SQLite comes back into the `sessions-postgres` graph, or if
  a SQLite crate loses the feature.
- The published `sessions-postgres` manifest on crates.io loses `sqlite` in the
  next release.

## Non-goals

- No change to the Rust code of any crate.
- No change to the `deny.toml` or `.cargo/audit.toml` advisory lists.
- No change to how `sessions-sqlite` builds SQLite (bundled stays).
- No check of the full facade feature set without `sessions-sqlite`. That list
  needs an enumeration of every facade feature, and the enumeration drifts. The
  targeted checks below cover the known leak.

## Anti-goals

- `sessions-postgres` must not lose `migrate!` or the `macros` feature.
- `sessions-sqlite` and `evals --features trace-sqlite` must not lose any `sqlx`
  feature that they have today.
- `Cargo.lock` must not change. Features are not recorded in the lockfile.

## Constraints

- Workspace inheritance is mandatory (CLAUDE.md). Each crate keeps
  `sqlx = { workspace = true, ... }` and adds its own `features`.
- **Semver: patch.** `sqlx` types are in the public API (`PgPool` in
  `sessions-postgres`, `SqlitePool` in `sessions-sqlite`). The API of
  `sessions-postgres` does not change. A downstream crate that got `sqlx::sqlite`
  only through feature unification with `sessions-postgres` depended on a side
  effect that we never documented. A breaking bump would also force a breaking
  bump of the facade (`scripts/check-breaking-cascade.sh`) with no API change.
  There are two failure modes for such a downstream crate, and the upgrade note
  names both:
  - `sqlx::sqlite` items do not compile.
  - `sqlx::AnyPool` with a `sqlite:` URL compiles but fails at run time, because
    `install_default_drivers` registers the SQLite driver only under
    `cfg(feature = "_sqlite")`.
- **Release triggers.** release-plz releases a crate for two independent
  reasons: (a) a commit touches a file in the crate directory, and (b) the
  packaged crate differs from its registry copy. SMA-763 changed only the root
  `sqlx` pin, and `sessions-postgres` 0.1.10 still got a release with the entry
  "update Cargo.toml dependencies". That precedent was a version-requirement
  change. We did not confirm that a feature-only change gives the same diff. So
  each of the three crates also gets a file change in its own directory
  (trigger a): `sessions-sqlite` and `evals` get `features = ["sqlite"]`, and
  `sessions-postgres` gets a CHANGELOG upgrade note and a corrected comment.
  The comment edit exists for this reason. The post-merge check below is the
  real safeguard. The facade and CLI cascade is automatic; no version is
  bumped by hand.
- **Commit scope.** `sessions-postgres` is not an allowed scope (`.versionrc`).
  PR title: `fix(sessions): SMA-822 stop sessions-postgres from compiling sqlite`.
  release-plz writes this line into each CHANGELOG, so it must be true for all
  three crates. It is true: SQLite is enabled where it is used and nowhere else.

## Approaches

1. **Remove only `sqlite` from the base (chosen).** The base keeps
   `runtime-tokio`, `macros`, and `migrate`, which all three crates use. Each
   SQLite crate adds `features = ["sqlite"]`. This is the smallest change and
   matches the ticket.
2. **Make the base minimal (`runtime-tokio` only).** Each crate lists every
   feature it uses. This prevents the same kind of leak for a future
   non-macro user (for example a MySQL backend). No such crate is planned, and
   all three current crates use `macros` and `migrate`. Rejected for now
   (my call).
3. **Add a second workspace entry (`sqlx-sqlite`).** Cargo does not allow two
   `[workspace.dependencies]` keys for one package without a rename. A rename
   changes the name in the code. Rejected.

## Design

1. Root `Cargo.toml`, line 84: remove `"sqlite"` from the `sqlx` feature list.
   Result: `features = ["runtime-tokio", "macros", "migrate"]`. Add a comment
   above the entry: only `sessions-sqlite` and `evals`/`trace-sqlite` enable
   `sqlite` (SMA-822), and a `build-no-default-features` step fails if SQLite
   gets into `sessions-postgres`.
2. `crates/paigasus-helikon-sessions-sqlite/Cargo.toml`:
   `sqlx = { workspace = true, features = ["sqlite"] }`.
3. `crates/paigasus-helikon-evals/Cargo.toml`:
   `sqlx = { workspace = true, optional = true, features = ["sqlite"] }`.
   `trace-sqlite = ["dep:sqlx"]` stays.
4. `crates/paigasus-helikon-sessions-postgres/Cargo.toml`: update the comment on
   line 18 so that it names the new base (`runtime-tokio`, `macros`, `migrate`,
   and no `sqlite`).
5. `crates/paigasus-helikon-sessions-postgres/CHANGELOG.md`, under
   `## [Unreleased]`: a hand-written upgrade note. It says that the crate no
   longer enables the sqlx `sqlite` feature, so Postgres-only users do not
   compile bundled SQLite or get SQLite advisories such as RUSTSEC-2026-0459. It
   says that a crate which uses `sqlx::sqlite`, or `AnyPool` with a `sqlite:`
   URL, must add `features = ["sqlite"]` to its own `sqlx` dependency. The note
   goes on the feature branch, not into the release PR, because release-plz
   drops edits to the release PR when it makes the PR again.
6. `.github/workflows/ci.yml`, job `build-no-default-features`: add one step
   `Assert SQLite only where it is used`. It uses
   `cargo tree --color never --target all -e normal,build -i libsqlite3-sys` with
   these packages:
   - **Negative:** `-p paigasus-helikon-sessions-postgres`, and
     `-p paigasus-helikon --no-default-features --features sessions-postgres`
     (the facade path most users take). Each must find no path.
   - **Positive controls:** `-p paigasus-helikon-sessions-sqlite`, and
     `-p paigasus-helikon-evals --features trace-sqlite`. Each must print a tree
     that names `libsqlite3-sys`.

   How to read "no path": with cargo 1.99, when the package is not in the
   graph, `cargo tree -i` exits 101 and writes
   `error: package ID specification ... did not match any packages` to stderr.
   An older form exits 0 and writes `warning: nothing to print` to stderr. The
   negative check passes only if one of those two outcomes occurs and stdout is
   empty. Any other outcome fails the step, and the step prints the output. A
   typo in the package name gives the same error as "no path". The positive
   controls use the same spelling, so they prove on every run that the name
   and the command form still find `libsqlite3-sys`. Each failure writes an
   `::error::` line that says what to fix. The step resolves the graph only, so
   it does not build. The step must not depend on `pipefail`: capture each
   command's stdout, stderr, and exit code separately, then test them.
7. `docs/runbooks/ci-architecture.md`, line 11: add the new step to the list of
   `build-no-default-features` steps (SMA-822).

## Book and README

The test: no page in `docs/book/src` and no `README.md` of the three crates
names the sqlx features of `sessions-postgres`, or says that Postgres users get
SQLite. `docs/book/src/concepts/sessions.md` names only `tls-rustls-aws-lc-rs`
for Postgres, and that stays true. The builder confirms this with a search and
records the result in the PR body.

## Verification

Before merge:

- The `cargo tree` command from the ticket finds no path.
- `git diff --exit-code "$(git merge-base origin/main HEAD)" -- Cargo.lock`
  passes.
- `cargo build -p paigasus-helikon-sessions-postgres` (lib only, no dev-deps)
  passes, so `migrate!` works without `sqlite`.
- `cargo build -p paigasus-helikon-sessions-sqlite` and
  `cargo build -p paigasus-helikon-evals --features trace-sqlite` pass alone.
- The guard step, run locally as a script, passes on the branch.
- Mutation checks, each reverted after the run:
  - Put `"sqlite"` back into the root base: both negative checks fail.
  - Add `"dep:paigasus-helikon-sessions-sqlite"` to the facade's
    `sessions-postgres` feature: only the facade check fails.
  - Remove `features = ["sqlite"]` from `evals`: only the `evals` positive
    control fails.
- `cargo package -p paigasus-helikon-sessions-postgres --no-verify`, then read
  `Cargo.toml` from the `.crate` archive by exact name. The `[dependencies.sqlx]`
  features must not include `sqlite`. Do the same for `sessions-sqlite` and
  `evals`; there `sqlite` must be present.
- All CI gates from CLAUDE.md pass locally where possible. `sessions-it` runs
  in CI and compiles `sessions-postgres` outside workspace unification.

After merge:

- The release PR bumps `sessions-postgres`, `sessions-sqlite`, `evals`, the
  facade, and the CLI. Watch the CI of that PR.
- After publish: a scratchpad project that depends only on the new
  `paigasus-helikon-sessions-postgres` version must show no `libsqlite3-sys`
  in `cargo tree -i libsqlite3-sys -e normal,build`.

## Rollback

A revert in the repository does not change manifests on crates.io. To roll
back: add `"sqlite"` back to the crate's own `sqlx` features, release a new
patch, and let the facade cascade follow.

## Left to the builder

- The exact text of the comments, the CHANGELOG note, and the `::error::`
  messages.
- Whether the guard is an inline `run:` block or a script under `scripts/`.
