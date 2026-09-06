# Contributing to Paigasus Helikon

This file documents the policies CI enforces. Reviewers will not relitigate what the gates have already checked — failing CI blocks merge.

## Code of Conduct

This project follows the [Contributor Covenant 2.1](./CODE_OF_CONDUCT.md). By participating, you agree to its terms. Report unacceptable behavior to `dev@paigasus.com`.

## Branch naming

All non-bot branches must follow this convention:

```text
^(feature|hotfix)\/[a-z0-9._-]+$
```

The `branch-names` repository ruleset (`.github/rulesets/branch-names.json`) enforces the **prefix** — any branch that is not `main`, not under `refs/heads/feature/**`, and not under `refs/heads/hotfix/**` is rejected on creation, update, or deletion. The character-set portion (`[a-z0-9._-]+`) is a social convention rather than a hard gate, because GitHub's `branch_name_pattern` rule (which would have enforced the full regex) is not available on user-owned repository rulesets. Linear's "Copy git branch name" produces compliant names (e.g. `feature/sma-305-ci-build-test-clippy-fmt-matrix`), so following Linear avoids the character-set ambiguity in practice.

`release-plz[bot]` and `dependabot[bot]` are allow-listed bypass actors for their own automation branches.

## Conventional Commits

Every commit message **and** every PR title must conform to
[Conventional Commits 1.0](https://www.conventionalcommits.org/en/v1.0.0/),
with the type and scope constrained as below. Three layers enforce
this:

| Layer | Fires when | Bypass |
| --- | --- | --- |
| Local `commit-msg` hook | `git commit` | `git commit --no-verify` |
| `ci / commits` job | PR open + sync | none — fix the message |
| `pr-title / pr-title` job | PR open/edit/sync | none — fix the title |

### Allowed types and semver effect

This project is currently pre-1.0 (`0.x.y` across every crate);
release-plz applies its own pre-1.0 bump rules — consult the
[release-plz docs](https://release-plz.dev/docs) for the exact
mapping in effect today. The table below shows the **post-1.0**
mapping that becomes operative after the first `1.0.0` release:

| Type | Semver effect (post-1.0) | Use for |
| --- | --- | --- |
| `feat` | minor | New user-visible capability |
| `fix` | patch | Bug fix |
| `feat!` or any type with `BREAKING CHANGE:` footer | major | API break |
| `chore`, `docs`, `refactor`, `test`, `perf`, `style`, `build`, `ci`, `revert` | none | Everything else |

### Scope allowlist

Scope is optional. If present, must match one of:

- **Crate scopes** (one per workspace member, facade collapsed to `facade`):
  `core`, `cli`, `facade`, `macros`, `mcp`, `tools`, `evals`,
  `providers`, `providers-openai`, `providers-anthropic`,
  `runtime`, `runtime-tokio`, `runtime-axum`, `runtime-temporal`, `runtime-agentcore`,
  `runtime-actix`
- **Cross-cutting scopes:** `workspace`, `workflows`, `ci`, `deps`,
  `release`, `repo`, `docs`, `contributing`, `readme`, `claude`,
  `spec`, `specs`, `plan`, `lints`

Canonical source is [`.versionrc`](./.versionrc). The
`pr-title.yml` workflow mirrors the same list — they must change
together.

### Examples

Valid:

```text
feat(core): SMA-304 add Model trait
fix(providers-openai): SMA-312 handle 429 retry-after header
chore(deps): bump tokio from 1.40 to 1.41
docs(contributing): SMA-310 document supply-chain section
ci(workflows): SMA-306 add cargo-audit workflow
feat(facade)!: SMA-400 reshape feature flag names
```

Invalid:

```text
wip                                  # no type
fix typo                             # no type/scope structure
Update README                        # wrong format; PR title would also fail subjectPattern
feat(unknown-scope): SMA-### foo     # scope not in allowlist
feat(core): Add Model trait          # PR title would fail subjectPattern (uppercase start)
```

### Optional Linear ticket prefix

Include `SMA-###` in the subject when the change is tied to a Linear
ticket. This is recommended for traceability but **not** CI-enforced
— bot commits (Dependabot, release-plz) don't carry an SMA-### and
are exempt. The PR-title check accepts both `feat(core): add foo`
and `feat(core): SMA-304 add foo`.

The PR-title regex tolerates any 2–4 letter Linear-style project
prefix followed by a hyphen and digits (`[A-Z]{2,4}-\d+`), not only
`SMA-`. If a second Linear project ever lands in this repo, that
project's prefix passes the same gate without a config change.

### Local commit-msg hook

The hook is a POSIX `sh` script and works on Unix, macOS, and
Windows via Git Bash or WSL (plain PowerShell will not invoke it).
Installation is driven by `cargo-husky`'s build script, which fires
when the facade's dev-deps are realized. After cloning, run once:

```bash
cargo test -p paigasus-helikon --no-run
```

This compiles cargo-husky's build script, which copies
`.cargo-husky/hooks/commit-msg` (at the workspace root) into
`.git/hooks/`. Verify with `ls .git/hooks/commit-msg`.

The hook execs `convco check`. If `convco` is not on `$PATH`, the
hook prints an install hint and exits non-zero:

```bash
cargo install convco --locked
# or, faster (prebuilt binary):
cargo binstall convco
# or, macOS:
brew install convco
```

(`cargo install convco --locked` builds from source and requires `cmake`; on machines without `cmake`, prefer `cargo binstall` or `brew install`.)

Emergency bypass (use sparingly):

```bash
git commit --no-verify -m "..."
```

CI re-runs the same checks regardless of `--no-verify`, so anything
the bypass lets through still has to be fixed before merge.

### Bot exceptions

- `dependabot[bot]` commits use `chore(deps): …` — valid under the allowlist.
- `release-plz[bot]` commits use `chore: release v…` — valid (scope optional).

No bot bypass is configured. If a future bot's output violates the
allowlist, amend the spec and the allowlist *before* enabling the
bot — not after.

## Build prerequisites

Besides the Rust toolchain (rustup; see [MSRV](#msrv) below), building the workspace requires a system **`protoc`** (Protocol Buffers compiler): the `temporalio-protos` crate (pulled in by `paigasus-helikon-runtime-temporal`'s `temporalio-*` dependencies) compiles its `.proto` definitions at build time via `prost-build`, which has no vendored `protoc` fallback.

```bash
brew install protobuf                 # macOS
sudo apt-get install protobuf-compiler  # Debian/Ubuntu
choco install protoc                  # Windows
```

If `protoc` is installed somewhere non-standard, point the `PROTOC` environment variable at the binary.

CI installs **protoc 35.1** in every job that compiles the workspace, via the repo-local `.github/actions/setup-protoc` action, which verifies the download against a pinned SHA-256 before extracting it. Distribution packages often lag several majors behind — `apt-get install protobuf-compiler` on Ubuntu 24.04 is one example — so to match CI exactly, take the archive for your platform from the [protobuf v35.1 release](https://github.com/protocolbuffers/protobuf/releases/tag/v35.1) rather than relying on the package manager. Matching exactly is rarely necessary; it is worth doing when you are chasing a protoc-related failure that reproduces in CI but not locally.

## MSRV

The workspace MSRV is **1.94** (declared in `[workspace.package].rust-version`). If a dependency raises the floor, bump `rust-version` to the version cargo demands — do **not** downgrade the dependency.

CI verifies MSRV two ways:

1. `ci / test (… , 1.94)` matrix rows actually compile and run on 1.94.
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

## Documentation site

The public docs site is built from `docs/book/`.

```bash
cargo install mdbook --version 0.4.43 --locked
cargo install mdbook-linkcheck --version 0.7.7 --locked
cd docs/book && mdbook serve
```

`mdbook serve` opens `http://localhost:3000` with live-reload. The CI `book-build` job runs `mdbook build`, which includes linkcheck because the `[output.linkcheck]` backend is declared in `book.toml`; broken internal links fail the build.

Deployment to GitHub Pages happens automatically on push to `main` via `.github/workflows/docs.yml`. The repo's Pages source is configured as **GitHub Actions** (Settings → Pages); future contributors do not need to touch this setting unless the workflow's deploy strategy changes.

The `mdbook build` output lands under `docs/book/book/html/` (because the `[output.linkcheck]` backend reorganizes the output tree). This path is only relevant for CI artifact uploads — `mdbook serve` reads from its own in-memory tree.

In `crates/paigasus-helikon/README.md` specifically, tag prose snippets ` ```text `
or ` ```ignore ` — never a bare ` ```rust `. That file is `include_str!`'d into the
facade's rustdoc, so a `rust` fence becomes a doctest that CI's test gate will run.

## Local pre-PR checklist

Run these before pushing — they are the same gates CI runs:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
npm ci                                               # once, or after package-lock.json changes
npx markdownlint-cli2                                # reads .markdownlint-cli2.jsonc
bash scripts/check-markdownlint-config.sh            # asserts that config is in force
bash scripts/check-cargo-profile-env-sync.sh         # cargo-visible workflow env is uniform
```

The Markdown gate needs Node (>= 22); everything else in that list needs only the
Rust toolchain — except `check-doc-coverage.sh` and `check-cargo-profile-env-sync.sh`,
which both use `mapfile` and so need bash >= 4.0. macOS ships bash 3.2, on which
they fail with `mapfile: command not found`; `brew install bash` alongside the
Rust toolchain is the fix.
`markdownlint-cli2` is pinned exactly in `package-lock.json` and is
**not** tracked by Dependabot — bumping it is a deliberate act, like `PROTOC_VERSION`
and `NIGHTLY_TOOLCHAIN`. `npx markdownlint-cli2 --fix` resolves most findings
mechanically; review the diff before committing, since a few fixes edit content
rather than whitespace.

Optionally (CI also runs this):

```bash
cargo install cargo-msrv      # or: cargo binstall cargo-msrv
cargo msrv --path crates/paigasus-helikon-core verify
```

(`cargo-msrv` has no `--workspace` flag. Since every crate's `rust-version` inherits from `[workspace.package]`, verifying one representative crate covers the whole workspace.)

To exercise the OpenAI provider against the real API, set `OPENAI_API_KEY` and run `cargo test -p paigasus-helikon-providers-openai -- --ignored`. Those `--ignored` provider tests are not part of CI. The other env-gated live suites are: `.github/workflows/integration.yml` runs the Temporal integration suite (`temporal-it`, with `HELIKON_REQUIRE_TEMPORAL=1` so a missing server fails the job instead of skipping silently) and the AgentCore image size/cold-start gates (`agentcore-image`, on an arm64 runner) as signal-only, non-required jobs. See CLAUDE.md's CI section for the promotion bar.

## Supply-chain security

Three workflows complement CI and gate PRs alongside the build matrix:

- `audit` — `cargo audit --deny warnings` against the [RustSec Advisory DB](https://rustsec.org/).
  Runs on every PR, every push to `main`, a daily cron at 06:00 UTC, and on
  demand via `workflow_dispatch`. The daily run uses the same job and the same
  command as the PR gate, so `main` cannot report green on an advisory that
  would redden a PR (SMA-479). A second job, `scheduled-audit`, runs
  `rustsec/audit-check` on the cron purely to auto-file a GitHub issue — it
  never fails, at any severity, so read the run's conclusion rather than that
  job's status.
- `deny` — `cargo deny --all-features check` enforces the license allowlist,
  ban list, source registry restrictions, and a second advisory pass. Policy
  lives in `deny.toml` at the workspace root. Same trigger set as `audit`, with
  the cron offset to 06:17 UTC. The job first runs
  `scripts/check-advisory-ignore-sync.sh`, which fails if the
  `[advisories].ignore` lists in `deny.toml` and `.cargo/audit.toml` have
  diverged — keep the two in sync, as the note below requires.
- `sbom` — on every `paigasus-helikon-v*` tag push, generates a CycloneDX SBOM via
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

## Reporting security issues

Please do not file public issues for vulnerabilities. See [SECURITY.md](./SECURITY.md) for the private reporting channel.

## Repo configuration

Branch protection, branch-name enforcement, CODEOWNERS, and merge-method
settings are checked in as JSON + a POSIX `sh` apply script:

| File | Purpose |
| --- | --- |
| `.github/CODEOWNERS` | Review routing — currently `* @SMK1085`. |
| `.github/rulesets/main-protection-checks.json` | Required status checks, linear history, no force-push, no deletion. Enforced on admins (no bypass). Required contexts: `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `test (macos-latest, stable)` (required because it is the only gate that compiles and runs the Seatbelt backend), `test (windows-latest, stable)` (required because it is the only gate that exercises the Windows timeout path in `paigasus-helikon-tools` — `cmd /C` process spawning and a `TerminateProcess`-based kill, which unlike the unix path has no process-group semantics and assigns a real exit code; SMA-569), `docs`, `doc-coverage`, `book-build`, `commits`, `pr-title`, `audit`, `deny`, `sessions-it` (required because it is the only gate that runs the Postgres/Redis session backends against live servers), `build-no-default-features` (required because it is the only gate that compiles both `runtime-axum` and `runtime-actix` with `--no-default-features`, catching `openapi`-feature-gating regressions, plus a `cargo tree` assertion that axum does not leak into the `runtime-actix` feature graph), `markdown-lint` (required because it is the only gate that lints the published Markdown surfaces — the mdBook, every crate README, the repo-root docs, and the runbooks — against `.markdownlint-cli2.jsonc`). |
| `.github/rulesets/main-protection-reviews.json` | 1 approval, dismiss stale, CODEOWNERS review, thread resolution. Admin role bypass — solo-maintainer self-merge is intentional and will auto-engage for non-admins once a second human joins. |
| `.github/rulesets/branch-names.json` | `creation` / `update` / `deletion` blocked on branches not under `refs/heads/feature/**`, `refs/heads/hotfix/**`, or `refs/heads/main`. Enforces the *prefix* portion of the documented branch-naming convention (full regex isn't enforceable on user-owned repository rulesets — `branch_name_pattern` is org-only). Bypass: dependabot (resolved at apply time) + the maintainer's private release-plz App `paigasusbot` (hardcoded ID — private Apps can't be looked up via the public `/apps/{slug}` endpoint). |
| `scripts/apply-repo-config.sh` | Idempotent applier. Resolves bot App IDs at apply time and POST/PUTs each ruleset; sets merge methods + squash-commit format via a direct `gh api -X PATCH` call (not `gh repo edit` — its boolean toggles silently drop `=false` and it doesn't expose `--squash-merge-commit-title`). |

To re-apply (or replay on a fork) after `gh auth login`:

```bash
bash scripts/apply-repo-config.sh
```

There is no drift-check CI job — divergence is detected by the next maintainer
running the script, which is acceptable for the current cadence. A
follow-up can add one if needed.

## Releases

Releases are automated by [release-plz](https://release-plz.dev). The contract:

1. **Conventional Commits drive bumps.** See the
   [Conventional Commits](#conventional-commits) section above for the
   type → semver mapping; `<scope>` is informational at release time
   (release-plz attributes by files changed, not by scope).

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
