# SMA-823: remove dead advisory ignores and the dead `jiff` Dependabot ignore

## Intent

Policy entries in `.cargo/audit.toml`, `deny.toml` and `.github/dependabot.yml`
refer to dependencies that the workspace no longer resolves. Their comments are
stale. The ticket says: correct each comment, or remove the entry when its
reason is gone. The reasons are gone, so this change removes the entries.

## Findings (checked on `main` at `e7ac5377`, 2026-11-20)

`Cargo.lock` at the repo root is the only lockfile. All `tests/*` crates are
workspace members. The lockfile covers all targets, all member features, and
dev and build dependencies, so a crate that is absent from it cannot match an
advisory in `cargo audit` or `cargo deny`.

1. **`paste` (RUSTSEC-2024-0436).** `grep -c '^name = "paste"' Cargo.lock` → 0.
   The `parquet` 60.1.0 manifest has no `paste` dependency of any kind. The
   ignore suppresses nothing. (Note: `pastey`, a different crate, is in the
   graph through `rmcp` 3.11.0. It is not related to parquet and has no
   advisory.)
2. **`backoff` (RUSTSEC-2025-0012) and `instant` (RUSTSEC-2024-0384).** Neither
   crate is in `Cargo.lock`. Their comments name "temporalio-* 0.5". The
   lockfile resolves `temporalio-client` 1.0.0 and `temporalio-sdk-core` 0.9.0.
   Both ignores suppress nothing. (Found by the spec challenger. Not named in
   the ticket.)
3. **`jiff` 0.2.29 (Dependabot).** The root `Cargo.toml` requires
   `jiff = "0.2.46"` (caret, SMA-818). `Cargo.lock` resolves 0.2.46.
   `proc-macro-error2` and `defmt` are not in the lockfile. Dependabot does not
   propose a version below the requirement, so the ignore has no effect. The
   `Cargo.toml` comment at lines 49-52 already records the 0.2.29 history, so
   `dependabot.yml` needs no historical note.
4. **Other stale comments in the same files.** The smartstring comment says
   "rhai 1.25.1 is the latest release". The lockfile resolves rhai 1.27.0. The
   entry is still necessary, because smartstring is still in the lockfile. The
   `.cargo/audit.toml` header says "the deny job in ci.yml". The deny job is in
   `.github/workflows/deny.yml`.

## Goals

- Remove `"RUSTSEC-2024-0436"`, `"RUSTSEC-2025-0012"` and `"RUSTSEC-2024-0384"`,
  with their comments, from both `.cargo/audit.toml` and `deny.toml`, in the same
  commit. `scripts/check-advisory-ignore-sync.sh` must stay green.
- In both files, put one unquoted historical note for each removed entry, in
  the style of the existing libsqlite3-sys note. The note states only what the
  evidence shows. For example:
  `# Historical note: the paste (RUSTSEC-2024-0436) entry was removed in SMA-823. parquet 60 does not depend on paste, and paste is not in Cargo.lock.`
  The sync script matches only quoted IDs, so unquoted notes do not affect it.
- Put a bare `#` line after each historical note, so that it does not read as
  part of the next entry's comment.
- Remove the full `ignore:` block from `.github/dependabot.yml`. The jiff entry
  is its only entry.
- Correct the rhai version in the smartstring comment (both files) and the
  deny-job location in the `.cargo/audit.toml` header.

## Non-goals

- Do not edit old specs and plans that mention these advisories or `=0.2.28`
  (SMA-479, SMA-461, SMA-763). They are historical records.
- Do not change any dependency version or `Cargo.lock`.
- No mdBook or crate README edit: this is a pure-internal policy change.

## Anti-goals

- The `audit` or `deny` gate goes red. That happens only if a removed crate is
  still reachable. The lockfile check above excludes this.

## Approaches

1. **Remove all dead entries in this PR (chosen).** The ticket's "Done when"
   names removal for the case where the dependency is gone. The same reason
   applies to backoff and instant, which sit in the same arrays. A dead ignore
   can hide a later reintroduction of the crate.
2. **Remove only the two entries the ticket names, and file a follow-up issue
   for backoff and instant.** Keeps this PR to the ticket's letter, but leaves
   the same defect in the same lines for a second PR.
3. **Keep the entries and rewrite only the comments.** Rejected: a comment that
   says "this ignore is dead" is noise.

## How the tools treat a dead ignore

cargo-deny reports an ignore that matches no crate as `advisory-not-detected`,
at warning level. `deny.toml` does not raise that level, and CI passes no
deny-warnings flag. cargo-audit accepts an unused ignore silently. So this
change does not change either gate's result. It only removes the
`advisory-not-detected` warnings.

## Verification

- Primary: for each of `paste`, `backoff`, `instant`, `proc-macro-error2`,
  `grep -c '^name = "<crate>"$' Cargo.lock` gives 0.
- `bash scripts/check-advisory-ignore-sync.sh` exits 0.
- The exact CI commands pass: `cargo audit --deny warnings` (`audit.yml`) and
  `cargo deny --all-features check` (`deny.yml`). The deny output has no
  `advisory-not-detected` line for the removed IDs.
- `.github/dependabot.yml` parses as YAML. After the merge, the repo's
  Dependabot page shows no configuration error for the `cargo` ecosystem.
- Note: `docs/superpowers/**` is excluded from markdownlint, so no lint step
  applies to this spec.

## Commit and PR

- Commit type `chore(deps)`. The `deps` scope is on the `.versionrc` and
  `pr-title.yml` allowlists.
- No file in a published crate's package changes, so release-plz has nothing
  to bump. (The reason is the file paths, not the commit type.)
- PR title: `chore(deps): SMA-823 remove dead advisory ignores and the jiff dependabot ignore`.

## Left to the builder

The exact comment wording, within the rules above.
