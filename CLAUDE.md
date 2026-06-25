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
```

The full list lives in `CONTRIBUTING.md` (single source of truth for contributor policies).

## Workspace layout

15 crates under `crates/`. The facade `paigasus-helikon` re-exports `paigasus-helikon-core` unconditionally and the other 12 sibling crates behind Cargo features.

**Implementation status** (as of 2026-06-24): ten crates carry real implementations and publish to crates.io — `paigasus-helikon-core`, `paigasus-helikon`, `paigasus-helikon-macros`, `paigasus-helikon-providers-openai`, `paigasus-helikon-providers-anthropic`, `paigasus-helikon-providers-bedrock` (SMA-329), `paigasus-helikon-sessions-sqlite`, `paigasus-helikon-runtime-tokio` (ascended from stub in SMA-346), `paigasus-helikon-mcp` (SMA-327), and `paigasus-helikon-tools` (SMA-328); the workspace was first published to crates.io in SMA-385. The remaining `-evals`, `-runtime-axum`, `-runtime-temporal`, `-runtime-agentcore` crates are docstring-only stubs pre-published at `0.0.0` as name-claim placeholders with `publish = false` + `release = false` — real implementations land in subsequent SMA-* tickets via the 4-step ascend recipe below. `paigasus-helikon-cli` is binary-only and never published as a library.

Workspace inheritance is **mandatory**: per-crate `Cargo.toml`s only set `name`, `description`, and any crate-specific bits. Everything else (`edition`, `rust-version`, `authors`, `license`, `repository`, `homepage`, `keywords`, `categories`) inherits from `[workspace.package]` in the root `Cargo.toml`. Don't hardcode these per-crate.

**Per-crate version is the one exception**, with a two-state lifecycle:

1. **Stub state — `version = "0.0.0"` + `publish = false` in Cargo.toml + `release = false` block in `release-plz.toml`.** Every stub was pre-published once to crates.io at `0.0.0` during SMA-385 to claim the name and satisfy the facade's optional-dep resolver. After that pre-publish, cargo refuses to republish (the per-crate `publish = false`); release-plz ignores them entirely (the `release = false`).
2. **Released state — bumped to a real version (≥ `0.1.0`)** after the first real public-API ticket lands. The 4-step ascend recipe:
   - Bump `version = "0.0.0"` → `"0.1.0"` in the crate's `Cargo.toml`.
   - Remove `publish = false` from that `Cargo.toml`.
   - Remove the crate's `[[package]] … release = false` block from `release-plz.toml`.
   - Land as one `chore(release): SMA-### lift stage-1 gates for <crate>` commit on the feature branch alongside the implementation. release-plz handles the first crates.io publish on CI.

   The 4-step recipe applies to **stubs ascending from `0.0.0`**. The ten already-released crates (`-core`, facade, `-macros`, `-providers-openai`, `-providers-anthropic`, `-providers-bedrock`, `-sessions-sqlite`, `-runtime-tokio`, `-mcp`, `-tools`) ship through release-plz's normal flow — no manual ritual needed for their future bumps. The historical chain of `chore(release): … escape release-plz 0.0.0 trap …` commits in the git log (SMA-317/347/350/372/382) is pre-Stage-1 archaeology and won't recur.

   **Caveat — when the ascending crate uses `paigasus-helikon-core` API added in the *same* PR, bump `core` too** (a 5th step). `cargo publish --verify` builds the ascending crate's tarball against the **registry** core (the `path` is stripped at publish), so if crates.io `core` lacks the new API the publish fails with `failed to verify package tarball`, and release-plz's combined job (SMA-351) aborts before its release-PR step — a deadlock, since `core` never gets its auto bump (a squashed `feat(<ascending-crate>)` commit attributes nothing to `core`). Fix: in the same PR, also bump `paigasus-helikon-core` (patch for additive/non-breaking-behind-`#[non_exhaustive]`, e.g. `0.2.0` → `0.2.1`) and its `[workspace.dependencies]` pin + CHANGELOG. release-plz then publishes `core` first, then the ascending crate verifies against the fresh `core` (dependency-ordered publish). Diagnosed in SMA-321: PR #45's release failed against the stale `core` `0.2.0`; PR #46 (`chore(release)` bumping `core` to `0.2.1`) cleared it.

   **Second-order caveat — the manual core bump silently defeats `dependencies_update`, so the facade drifts.** `release-plz.toml` sets `dependencies_update = true`, which is *supposed* to cascade: when a sibling's version changes, release-plz bumps the facade's `[workspace.dependencies]` pin and gives the facade a patch bump. But that cascade only runs when **release-plz itself performs the sibling bump**. The same-PR manual bump above means the sibling version is already at target when the PR merges, so release-plz just publishes it and never runs the dependent-bump step — the facade (`paigasus-helikon`) is left untouched and stops tracking. Consequence: the facade stays at its old version with stale published dep reqs (e.g. after SMA-346, facade `0.2.0` still requested `paigasus-helikon-runtime-tokio = ^0.0.0`, so the new runner-boundary surface was unreachable through the facade's `runtime-tokio` feature). **Fix: in any PR that uses the same-PR manual bump, ALSO bump the facade** (patch: `version` in `crates/paigasus-helikon/Cargo.toml` + its `[workspace.dependencies]` self-pin + CHANGELOG) so it republishes with current sibling reqs. Diagnosed after SMA-346: PRs #48/#49 shipped `core 0.2.3` + `runtime-tokio 0.1.1` but left facade `0.2.0`; PR #50 (`chore(release)` bumping facade to `0.2.1`) cleared it. NB: feature branches must match the `branch-names` ruleset (`feature/**` or `hotfix/**`); a `chore/**` branch is rejected at push with `GH013 … creations being restricted`.

**Released vs stub crates** (exact versions move every release — read each crate's `Cargo.toml` for current numbers, don't trust hardcoded values here): ten crates publish normally (`paigasus-helikon-core`, the `paigasus-helikon` facade, `-macros`, `-providers-openai`, `-providers-anthropic`, `-providers-bedrock` (SMA-329), `-sessions-sqlite`, `-runtime-tokio` (ascended from stub in SMA-346), `-mcp` (SMA-327), and `-tools` (SMA-328)). The four remaining stubs (`-evals`, `-runtime-axum`, `-runtime-temporal`, `-runtime-agentcore`) stay at `0.0.0` with `publish = false` + a `release = false` block in `release-plz.toml`. `paigasus-helikon-cli` is binary-only and never published.

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
- **Always implement GitHub Actions against the latest stable major.** Before adding or updating any `uses:` line in `.github/workflows/`, resolve the latest release of the action and pin to its commit SHA (never a moving `@vN` tag). Use:
  ```bash
  gh api repos/<owner>/<repo>/releases/latest | jq -r '.tag_name'
  gh api repos/<owner>/<repo>/git/ref/tags/<tag> | jq -r '.object.sha'
  # if .object.type == "tag" (annotated), dereference:
  # gh api repos/<owner>/<repo>/git/tags/<sha> | jq -r '.object.sha'
  ```
  Do not use a plan-time version pin if a newer major has shipped between plan-writing and implementation — bump immediately, then let Dependabot's `github-actions` group track patch/minor updates from there. The above-the-fold human-readable version stays as a `# action vX.Y.Z` comment so the SHA is auditable.
- **After a PR merges to `main`, release-plz opens/updates a `chore: release` PR** (authored by the paigasusbot App) carrying the version bumps + CHANGELOG; in the normal flow **merging that PR is what publishes to crates.io** (the merged feature PR left versions matching the registry, so its own `main` push publishes nothing). The exception is a PR that bumps its own version — the stub-ascend ritual — which publishes on its own merge with no separate release PR. Check the release PR after every merge and watch its CI — its release-PR `cargo update` can pull a fresh advisory that reddens `audit`/`deny` on the bot PR **only** (independent of `main`); fix with a `chore(deps)` pin and release-plz regenerates the PR clean.

## CI

`.github/workflows/ci.yml` runs six jobs on every PR (the `commits` job is PR-only; the other five also run on push to `main`): `fmt`, `clippy`, `test` (matrix: `{ubuntu, macos, windows} × {stable, 1.94}`, `fail-fast: false`), `docs` (with `RUSTDOCFLAGS=-D warnings`), `doc-coverage` (nightly rustdoc `--show-coverage`, aggregated by `scripts/check-doc-coverage.sh`, gated at `DOC_COVERAGE_THRESHOLD` — default 80%), and `commits` (SMA-335: `convco check` against the PR's commit range, gated by `if: github.event_name == 'pull_request'`). The `paigasus-helikon-cli` crate is excluded from both the `missing_docs` lint and the coverage aggregator until its public API stabilizes.

`.github/workflows/pr-title.yml` (SMA-335) runs `amannn/action-semantic-pull-request` on `pull_request_target` to gate the PR title — the squashed commit on `main`. Permissions are minimal (`pull-requests: read`, `statuses: write`); no `actions/checkout` step under `pull_request_target` keeps PR-controlled code out of the runner. Concurrency keys on `github.event.pull_request.number` because `pull_request_target` sets `github.ref` to the base ref and keying on it would cross-cancel different PRs. Dependabot PRs are exempt from the title check via `ignoreLabels: [area:deps]` — their auto-generated `Bump …` titles capitalize the subject and can't be reconfigured, so they'd otherwise block every dependency PR; the ignore label makes the check **pass** for them (not skip-and-block, which would leave the required context unreported and still block). Don't remove it.

`.github/workflows/msrv.yml` runs `cargo msrv --path crates/paigasus-helikon-core verify` as a non-required signal that the declared MSRV is truthful.

The required-status-check contexts gated on `main` are (bare job names, as posted by the GitHub Actions app): `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `test (macos-latest, stable)`, `docs`, `doc-coverage`, `book-build`, `commits`, `pr-title`, `audit`, `deny`. The macOS job is required because it is the only gate that compiles and runs the Seatbelt backend. The canonical declaration is `.github/rulesets/main-protection-checks.json` (see CONTRIBUTING.md → "Repo configuration"). Other matrix variants (`test (windows-latest, …)`, `test (…, 1.94)`) run as signals only. Concurrency cancels in-flight PR runs but lets `main` pushes complete; both workflows declare `permissions: contents: read`.

Supply-chain workflows (`.github/workflows/audit.yml`, `deny.yml`, `sbom.yml`) are separate from `ci.yml` because they have independent triggers and failure semantics. Required status checks added in SMA-306: `audit`, `deny` (declared in `.github/rulesets/main-protection-checks.json` alongside the CI gates). The `audit` workflow has two jobs gated by `github.event_name`: the PR-time `audit` job uses `taiki-e/install-action` for deterministic behavior; the daily `scheduled-audit` job uses `rustsec/audit-check@v2` for its auto-issue-filing behavior on advisory hits — these are the only places in the repo where a wrapper action is preferred over direct tool invocation.

The SBOM workflow invokes `cargo cyclonedx --manifest-path crates/paigasus-helikon/Cargo.toml --format json --spec-version 1.5 --all-features`. cargo-cyclonedx 0.5.x has no `-p` flag (must target via `--manifest-path`) and defaults to `--spec-version 1.3`, so 1.5 is pinned explicitly. With `--all-features` the facade's dep graph equals the workspace's dep graph, so one SBOM covers everything. The workflow's `find crates/paigasus-helikon -maxdepth 1 -name '*.cdx.json'` picks the facade's SBOM specifically — cargo-cyclonedx 0.5 walks the workspace and emits one SBOM under each member directory regardless of which member you point at, so scoping the find pattern matters.

`deny.toml` declares `version = 2` under both `[advisories]` and `[licenses]` — v1 fields (`vulnerability`, `unmaintained`, `unsound`, `copyleft`, etc.) are removed in modern cargo-deny and adding them will fail with a schema error. The license allowlist includes `Unicode-3.0` in addition to the ticket-prescribed `Unicode-DFS-2016` because `unicode-ident ≥ 1.0.13` (pulled transitively by `serde_derive`) relicensed in 2024. cargo-deny's advisory DB lives at `~/.cargo/advisory-dbs` (plural) per `deny.toml`'s `db-path`; cargo-audit's DB is at `~/.cargo/advisory-db` (singular) — each tool caches its own, and the CI cache directories are scoped per-workflow.

Dependabot is configured for `cargo` + `github-actions` ecosystems, weekly Monday 06:00 UTC (aligned with the daily audit cron), with patch + minor updates grouped into one PR per ecosystem.

The **`microvm`/forkd live-KVM path is not validated locally or in GitHub CI** — the dev host is arm64 macOS (no `/dev/kvm`) and GitHub runners have none. Validate it on a **GCP nested-virtualization VM** (Ubuntu 24.04 — the forkd binaries need glibc ≥ 2.38; Intel `n2`) per `docs/runbooks/forkd-live-validation.md`. The `tests/forkd_live.rs` tests are env-gated (`FORKD_URL` / `FORKD_TOKEN` / `FORKD_SNAPSHOT`) and loud-skip when no controller is configured, so `cargo test` stays green without one.

## Local hooks

Hooks are managed via `cargo-husky` (user-hooks mode) and live in `.cargo-husky/hooks/`. They're installed into `.git/hooks/` on the next dev-dep realization of `paigasus-helikon` (e.g. `cargo test -p paigasus-helikon --no-run`). To force re-install after editing a hook: `rm -rf target/debug/build/cargo-husky-* && cargo test -p paigasus-helikon --no-run`.

- **`commit-msg`** — runs `convco check --from-stdin` (enforces the `.versionrc` allowlist).
- **`pre-commit`** — intentional no-op (`exit 0`). The file exists to claim the slot so future cargo-husky upgrades don't fill it in with surprise behavior.
- **`pre-push`** — runs `cargo fmt --all -- --check`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`, and `convco check <upstream>..HEAD`. Catches the three fastest CI gates pre-push; deliberately omits `cargo test` and `cargo doc` (too slow for every push). Bypass for WIP branches: `git push --no-verify`.

## Fixture line endings

`.gitattributes` pins `crates/paigasus-helikon-providers-anthropic/tests/fixtures/*.txt` to `text eol=lf`. The streaming tests `include_str!` the SSE fixtures and split them on literal `\n` delimiters; without this, Windows checkouts produce CRLF bytes and the literal-string splits return one part instead of two. When adding wire-format fixtures elsewhere that the test code parses byte-level, extend the rule.

## Cargo.lock

Committed (workspace contains a binary).
