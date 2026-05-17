# Contributing to Paigasus Helikon

This file documents the policies CI enforces. Reviewers will not relitigate what the gates have already checked — failing CI blocks merge.

## Branch naming

All non-bot branches must match this regex (enforced via the SMA-309 repository ruleset once it lands):

```text
^(feature|hotfix)\/[a-z0-9._-]+$
```

Linear's "Copy git branch name" produces compliant names (e.g. `feature/sma-305-ci-build-test-clippy-fmt-matrix`).

`release-plz[bot]` and `dependabot[bot]` are allow-listed bypass actors for their own automation branches.

## Commit messages

Use the Conventional-Commits-style prefix with the Linear ticket ID:

```text
<type>(<scope>): SMA-### <message>
```

`<type>` is one of `feat`, `fix`, `docs`, `ci`, `chore`, `refactor`, `test`. `<scope>` is the affected area (`workspace`, `facade`, `workflows`, `lints`, …). Once SMA-335 lands, a GitHub Action enforces this in PR titles too.

## MSRV

The workspace MSRV is **1.75** (declared in `[workspace.package].rust-version`). If a dependency raises the floor, bump `rust-version` to the version cargo demands — do **not** downgrade the dependency.

CI verifies MSRV two ways:

1. `ci / test (… , 1.75)` matrix rows actually compile and run on 1.75.
2. `msrv / verify` runs `cargo msrv --path crates/paigasus-helikon-core verify` to confirm the declared MSRV is truthful. (cargo-msrv has no `--workspace` flag; since every member inherits `rust-version` from `[workspace.package]`, verifying one representative crate is sufficient.)

## Docstring coverage

Every `pub` item in non-CLI crates must have a doc comment. The CI gate is workspace-wide ≥ **80%**, configurable via `DOC_COVERAGE_THRESHOLD`. The CLI crate (`paigasus-helikon-cli`) is exempt until its public surface stabilizes (see the SMA-305 design spec §7).

The policy lives in `Cargo.toml`:

```toml
[workspace.lints.rust]
missing_docs = "warn"
```

Each non-CLI crate opts in with:

```toml
[lints]
workspace = true
```

The CLI opts out with:

```toml
[lints.rust]
missing_docs = "allow"
```

To check coverage locally:

```bash
rustup toolchain install nightly-2026-05-01     # one-time
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
```

The CI job posts a per-crate breakdown to the PR's Checks tab via `$GITHUB_STEP_SUMMARY`.

## Local pre-PR checklist

Run these before pushing — they are the same gates CI runs:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
```

Optionally (CI also runs this):

```bash
cargo install cargo-msrv      # or: cargo binstall cargo-msrv
cargo msrv --path crates/paigasus-helikon-core verify
```

(`cargo-msrv` has no `--workspace` flag. Since every crate's `rust-version` inherits from `[workspace.package]`, verifying one representative crate covers the whole workspace.)

## Supply-chain security

Three workflows complement CI and gate PRs alongside the build matrix:

- `audit` — `cargo audit --deny warnings` against the [RustSec Advisory DB](https://rustsec.org/).
  Runs on every PR + push to `main`, plus a daily scheduled run on `main` that
  auto-files a GitHub issue if a new advisory affects the locked deps.
- `deny` — `cargo deny --all-features check` enforces the license allowlist,
  ban list, source registry restrictions, and a second advisory pass. Policy
  lives in `deny.toml` at the workspace root.
- `sbom` — on every `v*` tag push, generates a CycloneDX SBOM via
  `cargo-cyclonedx` and uploads it as a release asset.

Local repro:

```bash
cargo install cargo-audit cargo-deny cargo-cyclonedx   # one-time
cargo audit --deny warnings
cargo deny --all-features check
cargo cyclonedx --manifest-path crates/paigasus-helikon/Cargo.toml \
  --format json --spec-version 1.5 --all-features
```

Adding a new dependency that pulls a license outside the allowlist will fail
`deny`. Either add the license to `deny.toml`'s `[licenses].allow` list (if
permissively compatible) or carve a per-crate exception under
`[licenses].exceptions`. Do **not** lower `confidence-threshold` or add to
`[advisories].ignore` without recording a rationale in the same commit.

Dependabot watches `cargo` and `github-actions` weekly (Monday 06:00 UTC),
grouping patch + minor updates per ecosystem. Major bumps remain ungrouped
so breaking changes are reviewed in isolation.

## Releases

Releases are automated by [release-plz](https://release-plz.dev). The contract:

1. **Conventional Commits drive bumps.** The mapping below applies to
   post-1.0 versions; release-plz adjusts the effective bump level for
   pre-1.0 (`0.x.y`) versions automatically per its own conventions —
   consult the [release-plz docs](https://release-plz.dev/docs) for the
   precise pre-1.0 rules. `feat(<scope>):` → minor. `fix(<scope>):` →
   patch. `feat!:` / `BREAKING CHANGE:` footer → major. `chore`, `docs`,
   `ci`, `refactor`, `test`, `style` → no bump. `<scope>` is informational
   — release-plz attributes by files changed.

2. **A rolling release PR.** The `release-plz` workflow runs on every push to
   `main` and maintains one open release PR titled `chore: release v...`. It
   enumerates which crates will bump and to what version, and shows the
   generated `CHANGELOG.md` additions. Reviewers verify, then squash-merge.

3. **Merging the release PR** triggers the workflow's `release-plz-release`
   job, which creates per-crate git tags (`<crate>-v<version>`), GitHub
   releases, and — once Stage 1 lifts `publish = false` — publishes to
   crates.io. The CLI crate (`paigasus-helikon-cli`) is permanently
   `publish = false` because it's a binary, not a library dependency.

4. **Overriding release-plz.** If the proposed bumps are wrong, edit the
   release PR's `Cargo.toml` / `CHANGELOG.md` directly — release-plz
   respects manual edits and won't clobber them on subsequent runs.

5. **Bootstrap commits on release infrastructure.** Any commit that edits
   `release-plz.toml`, `.github/workflows/release-plz.yml`, or every crate's
   `Cargo.toml` simultaneously must use `chore(...)` or `docs(...)` types —
   never `feat`/`fix`. Otherwise release-plz attributes a workspace-wide bump
   to the infrastructure change. See CLAUDE.md for the full rule.

### Authentication

release-plz authenticates as the [release-plz GitHub App](https://github.com/apps/release-plz)
installation on this repo. The workflow mints a per-job installation token
via `actions/create-github-app-token@v1` from two repo secrets
(`RELEASE_PLZ_APP_ID`, `RELEASE_PLZ_APP_PRIVATE_KEY`). The App identity is
listed as a bypass actor in SMA-309's branch-name ruleset, so release-plz's
`release-plz-<timestamp>` branch prefix is permitted.

A fine-grained PAT (with `contents: write` + `pull-requests: write` scoped
to this repo, stored as `RELEASE_PLZ_TOKEN`) is the documented fallback if
the App becomes unavailable. The workflow would be changed to read
`secrets.RELEASE_PLZ_TOKEN` instead of minting an App token.
