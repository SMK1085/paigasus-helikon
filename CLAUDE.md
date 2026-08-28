# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

The Paigasus AI SDK (codename **Helikon**, after Mt Helicon where Pegasus's hoof struck the Hippocrene spring). A Rust SDK for building AI agents. All crates live under the `paigasus-helikon-*` namespace.

The full architectural reference lives in Notion: ["Crate Reference"](https://www.notion.so/355830e8fbaa813c92e8c1aa9985fd3f). Linear project: `Paigasus Helikon` (issues prefixed `SMA-`).

## Common commands

```bash
cargo build --workspace                              # all 14 crates
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
npm ci                                               # once, or after package-lock.json changes
npx markdownlint-cli2                                # reads .markdownlint-cli2.jsonc
bash scripts/check-markdownlint-config.sh            # asserts that config is in force
```

The full list lives in `CONTRIBUTING.md` (single source of truth for contributor policies).

## Workspace layout

15 crates under `crates/`. The facade `paigasus-helikon` re-exports `paigasus-helikon-core` unconditionally and the other 12 sibling crates behind Cargo features.

Every crate carries a real implementation and publishes to crates.io; zero name-claim stubs remain (first published in SMA-385, last ascends in SMA-333). Two standing exceptions: `paigasus-helikon-cli` publishes as a **binary** crate — its lib target is internal (`missing_docs` opted out), carries no stability guarantee, and exists only so `cargo install paigasus-helikon-cli` resolves; and `paigasus-helikon-sessions-testkit` is the sole non-published crate, an internal `Session` conformance harness intentionally kept at `0.0.0` with `publish = false`, not a stub awaiting an ascend. Exact versions move every release — read each crate's `Cargo.toml`, don't trust numbers written here.

Workspace inheritance is **mandatory**: per-crate `Cargo.toml`s only set `name`, `description`, and any crate-specific bits. Everything else (`edition`, `rust-version`, `authors`, `license`, `repository`, `homepage`, `keywords`, `categories`) inherits from `[workspace.package]` in the root `Cargo.toml`. Don't hardcode these per-crate.

**Per-crate version is the one exception.** No stubs remain, so the 4-step ascend recipe is now historical — but two caveats still bite any PR that hand-bumps a version:

- **If a crate uses `paigasus-helikon-core` API added in the *same* PR, bump `core` too**, along with its `[workspace.dependencies]` pin and CHANGELOG. `cargo publish --verify` builds the tarball against the **registry** core, so otherwise the publish fails and release-plz deadlocks.
- **Any PR that hand-bumps a sibling must ALSO bump the facade** (`crates/paigasus-helikon/Cargo.toml` version + its self-pin + CHANGELOG). release-plz's `dependencies_update` cascade only runs when release-plz itself performs the sibling bump, so a manual bump silently leaves the facade advertising stale dep reqs.
- Feature branches must match the `branch-names` ruleset (`feature/**` or `hotfix/**`); a `chore/**` branch is rejected at push with `GH013 … creations being restricted`.

Full lifecycle, the 4-step recipe, and the SMA-321/SMA-346 postmortems: `docs/runbooks/release-ascend.md`.

Third-party version pins live in `[workspace.dependencies]` (root). Members reference them via `dep.workspace = true`. Internal crate paths are also in `[workspace.dependencies]` so the facade can use `workspace = true` consistently.

## Non-obvious patterns to preserve

- **Feature naming**: kebab-case in `[features]` (`runtime-tokio`), snake-case in `pub use` aliases (`runtime_tokio`). They must stay paired across the facade's `Cargo.toml` and `src/lib.rs`.
- **`paigasus-helikon-cli` uses `autobins = false`** because the `paigasus-helikon` (hyphen) binary maps to `src/bin/paigasus_helikon.rs` (underscore — hyphens are illegal in Rust filenames). Removing `autobins = false` reintroduces a phantom `paigasus_helikon` binary that conflicts with the explicit `[[bin]]` entry.
- **`paigasus-helikon-macros` is a proc-macro crate from day one** (`[lib] proc-macro = true`). Don't convert it to a regular lib even though it currently has no macros.
- **The `paigasus-helikon` facade lib shares its name with the `paigasus-helikon` CLI binary by design** (Notion ref's "fully-qualified shim alias"). This produces a non-fatal `cargo doc` filename-collision warning. Don't "fix" it by renaming either — both names are user-facing API. The accepted future fix is `doc = false` on the CLI binary entry.
- **License is dual `Apache-2.0 OR MIT`** (decided 2026-05-20, reversing the 2026-05-16 MIT-only call). Both `LICENSE-APACHE` and `LICENSE-MIT` live at the repo root; the workspace metadata is `license = "Apache-2.0 OR MIT"`. Per Rust ecosystem convention — no Apache-only or MIT-only crates in the workspace. Contributions are accepted under the same dual license by default (the standard inbound-equals-outbound clause is restated in `README.md`).
- **MSRV is `1.94`** (workspace-package level; raised from 1.85 in SMA-329 because sqlx 0.9.0 declares `rust-version = "1.94.0"` — the pre-existing highest floor in the workspace). If a dep raises MSRV, bump `rust-version` to what cargo demands rather than downgrading the dep.
- **Workspace-wide `missing_docs` enforcement** lives in root `Cargo.toml` (`[workspace.lints.rust] missing_docs = "warn"`). Each non-CLI crate opts in with `[lints] workspace = true`. The CLI overrides locally with `[lints.rust] missing_docs = "allow"` and does **not** include `workspace = true` — cargo treats `[lints] workspace = true` and an inline `[lints.<tool>]` table as mutually exclusive. When adding a new crate, copy the opt-in block. When adding a new `pub use` re-export to the facade, give it a `///` doc comment or `-D warnings` will fail the docs job.
- **`cargo msrv` has no `--workspace` flag.** The msrv workflow verifies one representative inheriting crate: `cargo msrv --path crates/paigasus-helikon-core verify`. Because every member uses `rust-version.workspace = true`, success on one is success on all. Don't "fix" the workflow by adding `--workspace`; that's what the first SMA-305 CI run died on.
- **Nightly is date-pinned** (`NIGHTLY_TOOLCHAIN: nightly-2026-05-01` at the workflow `env:` level in `ci.yml`, threaded into the doc-coverage script as `NIGHTLY_CHANNEL`). The rustdoc JSON coverage format is `-Z unstable-options` and can shift between nightlies; floating `nightly` would be a CI footgun. Bumping is a one-line follow-up chore, not an emergency.
- **Bootstrap commits on release infrastructure must use `chore(...)` or `docs(...)` types**, never `feat`/`fix`. release-plz parses every commit since the last per-crate tag — a `feat(workspace): ...` commit that touches every `Cargo.toml` would attribute a bump to every crate. The SMA-307 bootstrap PR followed this rule; the same rule applies to any future `release-plz.toml` or `release-plz.yml` edits.

## Model routing

The general tiering rules are in the global `~/.claude/CLAUDE.md`. Only the repo-specific rules are here.

- **Use Opus for release work.** This covers release-plz, the stub-ascend recipe, a hand-bumped crate version, and any red required gate. A wrong answer breaks a publish; see `docs/runbooks/release-ascend.md`.
- **Haiku can read CI output.** Change to Opus when the question becomes *why* a required gate is red.

## Workflow conventions

- Branch per Linear issue: `feature/<sma-####>-<kebab-title>`. The branch name is pre-computed in each Linear ticket's `gitBranchName` field.
- Design artifacts per ticket (`docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md`, `docs/superpowers/plans/YYYY-MM-DD-<topic>.md`) land on the feature branch alongside the implementation — not pre-merged to `main`.
- **Keep the public mdBook (`docs/book/`) current — update it in the same PR as any user-facing change.** Before opening a PR, check whether the work changes public API, the quickstart/example flow, the crate roster (incl. a stub crate ascending to published), or a documented concept; if so, bring the affected `docs/book/src/*.md` page(s) into line on the same branch. The book is published from `main` and drifts silently otherwise — it sat as the untouched SMA-311 scaffold (13/17 pages still `> **Stub.**`) through all of Stage 1 before anyone noticed; SMA-423 is the one-time catch-up, and this rule keeps the backlog from rebuilding. A pure-internal change (refactor, CI, deps, release plumbing) needs no book edit — but make that a conscious call, not a silent skip. `mdbook build docs/book` must stay clean (`[output.linkcheck] warning-policy = "error"`).
- **Keep crate `README.md` files current — update the affected crate's README in the same PR as any change to its public surface.** Each of the ten published crates' `README.md` is its crates.io (and docs.rs landing-sidebar) page — no crate sets an explicit `readme`, so Cargo uses the default `README.md`. Before opening a PR, for every crate the work touches check whether the change affects that crate's public API / usage example, its install or feature story, or its published status (a stub ascending to published, a renamed/added feature flag); if so, bring its `crates/<crate>/README.md` into line on the same branch — and also the facade `crates/paigasus-helikon/README.md` and the root `README.md` whenever the crate roster or the feature → module map changes. README install snippets deliberately use drift-free `cargo add` (no hardcoded versions), so a routine version bump alone needs no README edit. Like the mdBook, the READMEs drift silently otherwise — they sat as untouched 3-line SMA-304 stubs (`Stub — see SMA-304`) through all of Stage 1 while ten crates shipped real implementations; SMA-424 is the one-time catch-up, and this rule keeps the backlog from rebuilding. A pure-internal change (refactor, CI, deps, release plumbing) needs no README edit — but make that a conscious call, not a silent skip.
- Commit prefix: `<type>(<scope>): SMA-### <message>` (e.g. `feat(facade): SMA-304 ...`).
- **PR titles must satisfy two independent rules from `pr-title.yml`** (`amannn/action-semantic-pull-request`):
  1. **Full Conventional Commits format.** The action enforces a valid `type(scope):` prefix from the action's configured `types` list — independent of the subject regex. `SMA-317 add anthropic provider` (no prefix) fails; `feat(providers-anthropic): SMA-317 add anthropic provider` passes.
  2. **Subject must start lowercase after the `SMA-###` prefix.** The `subjectPattern: ^([A-Z]{2,4}-\d+ )?[^A-Z].+$` rejects `feat(core): SMA-314 LlmAgent + ...` because `L` is uppercase; lead the subject with a lowercase verb (`add`, `wire`, `pin`, `promote`, `implement`, `fix`).
  Per-commit Conventional Commit titles on the feature branch don't trip either rule — only the PR title (which becomes the squashed `main` commit) is gated.
- Linear auto-closes the linked SMA-* issue when its PR merges; no manual status move needed.
- **Always implement GitHub Actions against the latest stable major**, pinned to a commit SHA (never a moving `@vN` tag), keeping the human-readable version as a `# action vX.Y.Z` comment so the SHA is auditable. Dependabot's `github-actions` group is patch + minor only, so **a major bump never arrives on its own and must be swept by hand**. `dtolnay/rust-toolchain` is a special case: it ships rolling branches and a periodically re-pointed `v1` tag, so confirm the direction (`ahead_by > 0, behind_by == 0`) before accepting any bump — the naive "bump to latest release" recipe once would have proposed an ~11-month downgrade. Independently and permanently: every site pinning it **must** pair the SHA with an explicit `with: toolchain: …`, because a SHA ref carries no toolchain selector. Recipes and history: `docs/runbooks/github-actions-pinning.md`.
- **After a PR merges to `main`, release-plz opens/updates a `chore: release` PR** (authored by the paigasusbot App) carrying the version bumps + CHANGELOG; in the normal flow **merging that PR is what publishes to crates.io** (the merged feature PR left versions matching the registry, so its own `main` push publishes nothing). The exception is a PR that bumps its own version — the stub-ascend ritual — which publishes on its own merge with no separate release PR. Check the release PR after every merge and watch its CI — its release-PR `cargo update` can pull a fresh advisory that reddens `audit`/`deny` on the bot PR **only** (independent of `main`); fix with a `chore(deps)` pin and release-plz regenerates the PR clean.

## CI

`.github/workflows/ci.yml` runs nine jobs on every PR (`commits` is PR-only; the other eight also run on push to `main`): `fmt`, `clippy`, `test` (matrix `{ubuntu, macos, windows} × {stable, 1.94}`, `fail-fast: false`), `build-no-default-features`, `docs` (`RUSTDOCFLAGS=-D warnings`), `doc-coverage` (gated at `DOC_COVERAGE_THRESHOLD`, default 80%), `commits` (`convco check`), `sessions-it` (Postgres/Redis, path-filtered), and `markdown-lint`. The `paigasus-helikon-cli` crate is excluded from both the `missing_docs` lint and the coverage aggregator until its public API stabilizes.

`.github/workflows/integration.yml` runs `temporal-it` and `agentcore-image` as **signal-only** jobs, deliberately outside `ci.yml` so an expected flake cannot make "ci is red on `main`" meaningless. **Signal-only means *not listed as required*, never `continue-on-error`.**

`.github/workflows/msrv.yml` runs `cargo msrv --path crates/paigasus-helikon-core verify` as a non-required signal that the declared MSRV is truthful.

Supply-chain workflows (`audit.yml`, `deny.yml`, `sbom.yml`) are separate from `ci.yml` because they have independent triggers and failure semantics. `audit.yml` and `deny.yml` both run on push to `main`, PRs, a daily cron, and `workflow_dispatch`, so `main` is re-evaluated daily at exactly PR severity.

**To read an audit verdict, use the Checks API, not the legacy commit-status API** — the latter returns only `CodeRabbit` on this repo, rendering a confident `state: success` that contains no audit verdict whatsoever:

```bash
gh api repos/SMK1085/paigasus-helikon/commits/main/check-runs \
  --jq '.check_runs[] | select(.name=="audit" or .name=="deny") | {name, status, conclusion}'
```

Note also that the `scheduled-audit` job's green status means nothing at any severity — read the *run* conclusion, never that job's status.

The required-status-check contexts gated on `main` are (bare job names, as posted by the GitHub Actions app): `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `test (macos-latest, stable)`, `docs`, `doc-coverage`, `book-build`, `commits`, `pr-title`, `audit`, `deny`, `sessions-it`, `build-no-default-features`, `markdown-lint`. The macOS job is required because it is the only gate that compiles and runs the Seatbelt backend; `sessions-it` because it is the only gate that runs the live Postgres/Redis session backends; `build-no-default-features` because it is the only gate that compiles `runtime-axum` and `runtime-actix` with default features off. The canonical declaration is `.github/rulesets/main-protection-checks.json` (see CONTRIBUTING.md → "Repo configuration"). Other matrix variants run as signals only.

Four pins are hand-bumped with nothing tracking them: `PROTOC_VERSION` and its three digests in `.github/actions/setup-protoc/install.sh`, `TEMPORAL_CLI_VERSION`/`TEMPORAL_CLI_SHA256` in `integration.yml`, `NIGHTLY_TOOLCHAIN` in `ci.yml`, and `markdownlint-cli2` via `package-lock.json`. **A checksum mismatch is never a signal to update the digest** — the causes are, in order, a truncated download, an upstream re-tag, and tampering; verify upstream independently first.

Job-by-job rationale, the protoc bump runbook, the audit/deny signal semantics, and the incident history behind all of the above: `docs/runbooks/ci-architecture.md`.

The **`microvm`/forkd live-KVM path is not validated locally or in GitHub CI** — the dev host is arm64 macOS (no `/dev/kvm`) and GitHub runners have none. Validate it on a **GCP nested-virtualization VM** (Ubuntu 24.04 — the forkd binaries need glibc ≥ 2.38; Intel `n2`) per `docs/runbooks/forkd-live-validation.md`. The `tests/forkd_live.rs` tests are env-gated (`FORKD_URL` / `FORKD_TOKEN` / `FORKD_SNAPSHOT`) and loud-skip when no controller is configured, so `cargo test` stays green without one.

## Local hooks

Hooks are managed via `cargo-husky` (user-hooks mode) and live in `.cargo-husky/hooks/`. They install into `.git/hooks/` on the next dev-dep realization of `paigasus-helikon`. To force a re-install after editing one: `rm -rf target/debug/build/cargo-husky-* && cargo test -p paigasus-helikon --no-run`.

- **`commit-msg`** — runs `convco check --from-stdin` (enforces the `.versionrc` allowlist).
- **`pre-commit`** — intentional no-op (`exit 0`). The file exists to claim the slot so future cargo-husky upgrades don't fill it in with surprise behavior.
- **`pre-push`** — runs `cargo fmt --all -- --check`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`, and `convco check <merge-base>..HEAD`. Catches the three fastest CI gates pre-push; deliberately omits `cargo test` and `cargo doc` (too slow for every push). Bypass for WIP branches: `git push --no-verify`.

Three rules here are easy to get wrong, and each has already cost a debugging session:

- **The re-install silently does nothing inside a git worktree** — cargo-husky searches for a `.git` *directory* and a worktree's is a file, so it installs nothing and the old hook keeps running. Run the re-install from the **main checkout**, and verify with `grep` against the installed copy rather than assuming.
- **Never hand-copy a hook onto `.git/hooks/<name>`** — on this machine the Entire CLI owns `pre-push` and chains to `.git/hooks/pre-push.pre-entire`, which is where the cargo-husky hook actually lives. Check for a `.pre-entire` sibling first and install into that slot.
- **The convco baseline must be a merge-base, not a branch tip** — `convco check A..B` silently falls back to walking the entire history when `A` is not an ancestor of `B`, rejecting correct branches with failures the author did not write.

Mechanics and the SMA-547 postmortem: `docs/runbooks/local-hooks.md`.

## Fixture line endings

`.gitattributes` pins `crates/paigasus-helikon-providers-anthropic/tests/fixtures/*.txt` to `text eol=lf`. The streaming tests `include_str!` the SSE fixtures and split them on literal `\n` delimiters; without this, Windows checkouts produce CRLF bytes and the literal-string splits return one part instead of two. When adding wire-format fixtures elsewhere that the test code parses byte-level, extend the rule.

## Cargo.lock

Committed (workspace contains a binary).
