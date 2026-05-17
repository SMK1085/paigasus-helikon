# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

The Paigasus AI SDK (codename **Helikon**, after Mt Helicon where Pegasus's hoof struck the Hippocrene spring). A Rust SDK for building AI agents. All crates live under the `paigasus-helikon-*` namespace.

The full architectural reference lives in Notion: ["Crate Reference"](https://www.notion.so/355830e8fbaa813c92e8c1aa9985fd3f). Linear project: `Paigasus Helikon` (issues prefixed `SMA-`).

## Common commands

```bash
cargo build --workspace                              # all 13 crates
cargo build --workspace --all-features               # facade with every optional crate
cargo run -p paigasus-helikon-cli --bin helikon
cargo run -p paigasus-helikon-cli --bin paigasus-helikon
```

To reproduce **every** CI gate locally (matches `.github/workflows/ci.yml` job-for-job):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh                 # requires: rustup toolchain install nightly-2026-05-01
```

The full list lives in `CONTRIBUTING.md` (single source of truth for contributor policies).

## Workspace layout

13 crates under `crates/`. The facade `paigasus-helikon` re-exports `paigasus-helikon-core` unconditionally and the other 10 sibling crates behind Cargo features. Stub crates print docstrings only — real implementations land in subsequent SMA-* tickets.

Workspace inheritance is **mandatory**: per-crate `Cargo.toml`s only set `name`, `description`, and any crate-specific bits. Everything else (`edition`, `rust-version`, `authors`, `license`, `repository`, `homepage`, `keywords`, `categories`) inherits from `[workspace.package]` in the root `Cargo.toml`. Don't hardcode these per-crate. **Exception**: `version` is per-crate — each `crates/*/Cargo.toml` sets `version = "0.0.0"` explicitly so release-plz can bump crates independently (see SMA-307). The `workspace.package.version = "0.0.0"` default in the root `Cargo.toml` stays as a safety net for new crates that forget to declare their own.

Third-party version pins live in `[workspace.dependencies]` (root). Members reference them via `dep.workspace = true`. Internal crate paths are also in `[workspace.dependencies]` so the facade can use `workspace = true` consistently.

## Non-obvious patterns to preserve

- **Feature naming**: kebab-case in `[features]` (`runtime-tokio`), snake-case in `pub use` aliases (`runtime_tokio`). They must stay paired across the facade's `Cargo.toml` and `src/lib.rs`.
- **`paigasus-helikon-cli` uses `autobins = false`** because the `paigasus-helikon` (hyphen) binary maps to `src/bin/paigasus_helikon.rs` (underscore — hyphens are illegal in Rust filenames). Removing `autobins = false` reintroduces a phantom `paigasus_helikon` binary that conflicts with the explicit `[[bin]]` entry.
- **`paigasus-helikon-macros` is a proc-macro crate from day one** (`[lib] proc-macro = true`). Don't convert it to a regular lib even though it currently has no macros.
- **The `paigasus-helikon` facade lib shares its name with the `paigasus-helikon` CLI binary by design** (Notion ref's "fully-qualified shim alias"). This produces a non-fatal `cargo doc` filename-collision warning. Don't "fix" it by renaming either — both names are user-facing API. The accepted future fix is `doc = false` on the CLI binary entry.
- **License is MIT only** (decided 2026-05-16). Don't add `LICENSE-APACHE` or set `license = "Apache-2.0 OR MIT"` even though the Cargo ecosystem convention is dual-licensing.
- **MSRV is `1.75`** (workspace-package level). If a dep raises MSRV, bump `rust-version` to what cargo demands rather than downgrading the dep.
- **Workspace-wide `missing_docs` enforcement** lives in root `Cargo.toml` (`[workspace.lints.rust] missing_docs = "warn"`). Each non-CLI crate opts in with `[lints] workspace = true`. The CLI overrides locally with `[lints.rust] missing_docs = "allow"` and does **not** include `workspace = true` — cargo treats `[lints] workspace = true` and an inline `[lints.<tool>]` table as mutually exclusive. When adding a new crate, copy the opt-in block. When adding a new `pub use` re-export to the facade, give it a `///` doc comment or `-D warnings` will fail the docs job.
- **`cargo msrv` has no `--workspace` flag.** The msrv workflow verifies one representative inheriting crate: `cargo msrv --path crates/paigasus-helikon-core verify`. Because every member uses `rust-version.workspace = true`, success on one is success on all. Don't "fix" the workflow by adding `--workspace`; that's what the first SMA-305 CI run died on.
- **Nightly is date-pinned** (`NIGHTLY_TOOLCHAIN: nightly-2026-05-01` at the workflow `env:` level in `ci.yml`, threaded into the doc-coverage script as `NIGHTLY_CHANNEL`). The rustdoc JSON coverage format is `-Z unstable-options` and can shift between nightlies; floating `nightly` would be a CI footgun. Bumping is a one-line follow-up chore, not an emergency.
- **Bootstrap commits on release infrastructure must use `chore(...)` or `docs(...)` types**, never `feat`/`fix`. release-plz parses every commit since the last per-crate tag — a `feat(workspace): ...` commit that touches every `Cargo.toml` would attribute a bump to every crate. The SMA-307 bootstrap PR followed this rule; the same rule applies to any future `release-plz.toml` or `release-plz.yml` edits.

## Workflow conventions

- Branch per Linear issue: `feature/<sma-####>-<kebab-title>`. The branch name is pre-computed in each Linear ticket's `gitBranchName` field.
- Design artifacts per ticket land under `docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md` and `docs/superpowers/plans/YYYY-MM-DD-<topic>.md` on `main` before implementation starts.
- Commit prefix: `<type>(<scope>): SMA-### <message>` (e.g. `feat(facade): SMA-304 ...`).
- Don't auto-close Linear issues from PR merge — move status manually after review.

## CI

`.github/workflows/ci.yml` runs five jobs on every PR and every push to `main`: `fmt`, `clippy`, `test` (matrix: `{ubuntu, macos, windows} × {stable, 1.75}`, `fail-fast: false`), `docs` (with `RUSTDOCFLAGS=-D warnings`), and `doc-coverage` (nightly rustdoc `--show-coverage`, aggregated by `scripts/check-doc-coverage.sh`, gated at `DOC_COVERAGE_THRESHOLD` — default 80%). The `paigasus-helikon-cli` crate is excluded from both the `missing_docs` lint and the coverage aggregator until its public API stabilizes.

`.github/workflows/msrv.yml` runs `cargo msrv --path crates/paigasus-helikon-core verify` as a non-required signal that the declared MSRV is truthful.

The required-status-check IDs SMA-309 will gate merge on are: `ci / fmt`, `ci / clippy`, `ci / test (ubuntu-latest, stable)`, `ci / docs`, `ci / doc-coverage`. Other matrix variants run as signals. Concurrency cancels in-flight PR runs but lets `main` pushes complete; both workflows declare `permissions: contents: read`.

Supply-chain workflows (`.github/workflows/audit.yml`, `deny.yml`, `sbom.yml`) are separate from `ci.yml` because they have independent triggers and failure semantics. Required status checks added in SMA-306: `audit / audit`, `deny / deny`. The `audit` workflow has two jobs gated by `github.event_name`: the PR-time `audit` job uses `taiki-e/install-action` for deterministic behavior; the daily `scheduled-audit` job uses `rustsec/audit-check@v2` for its auto-issue-filing behavior on advisory hits — these are the only places in the repo where a wrapper action is preferred over direct tool invocation.

The SBOM workflow invokes `cargo cyclonedx --manifest-path crates/paigasus-helikon/Cargo.toml --format json --spec-version 1.5 --all-features`. cargo-cyclonedx 0.5.x has no `-p` flag (must target via `--manifest-path`) and defaults to `--spec-version 1.3`, so 1.5 is pinned explicitly. With `--all-features` the facade's dep graph equals the workspace's dep graph, so one SBOM covers everything. The workflow's `find crates/paigasus-helikon -maxdepth 1 -name '*.cdx.json'` picks the facade's SBOM specifically — cargo-cyclonedx 0.5 walks the workspace and emits one SBOM under each member directory regardless of which member you point at, so scoping the find pattern matters.

`deny.toml` declares `version = 2` under both `[advisories]` and `[licenses]` — v1 fields (`vulnerability`, `unmaintained`, `unsound`, `copyleft`, etc.) are removed in modern cargo-deny and adding them will fail with a schema error. The license allowlist includes `Unicode-3.0` in addition to the ticket-prescribed `Unicode-DFS-2016` because `unicode-ident ≥ 1.0.13` (pulled transitively by `serde_derive`) relicensed in 2024. cargo-deny's advisory DB lives at `~/.cargo/advisory-dbs` (plural) per `deny.toml`'s `db-path`; cargo-audit's DB is at `~/.cargo/advisory-db` (singular) — each tool caches its own, and the CI cache directories are scoped per-workflow.

Dependabot is configured for `cargo` + `github-actions` ecosystems, weekly Monday 06:00 UTC (aligned with the daily audit cron), with patch + minor updates grouped into one PR per ecosystem.

## Cargo.lock

Committed (workspace contains a binary).
