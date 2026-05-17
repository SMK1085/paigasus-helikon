# SMA-335 — Enforce Conventional Commits (CI + PR title + local hook)

**Linear issue**: [SMA-335](https://linear.app/smaschek/issue/SMA-335/enforce-conventional-commits-ci-pr-title-local-hook)
**Status**: design approved 2026-05-17
**Branch**: `feature/sma-335-enforce-conventional-commits-ci-pr-title-local-hook`
**Blocks**: SMA-309 (branch protection — consumes the two new required-status-check IDs)
**Related**: SMA-307 (release-plz reads commit types), SMA-308 (`chore(deps):` Dependabot prefix), SMA-310 (CONTRIBUTING.md exists)

## 1. Goal & non-goals

**Goal.** Make Conventional Commits a hard gate, so the assumptions baked into SMA-307 (release-plz reads commit types to compute bumps) and SMA-308 (`chore(deps):` Dependabot prefix is honored) become enforced rather than hoped-for. Three layers, each with a distinct purpose:

| Layer | Fires on | Gates | Why separate |
|---|---|---|---|
| Local commit-msg hook | `git commit` on dev machine | Each commit message | Fastest feedback; saves a push round-trip |
| `ci / commits` job | PR open + sync | Each commit in `<base>..HEAD` | Keeps in-PR history scannable; catches typos before reviewers see them |
| `pr-title / pr-title` job | PR open/edit/sync | The PR title only | Squash-merge is enabled, so the PR title is *the* main-branch commit message that release-plz parses |

**Non-goals.**

- Enforcing the project-local `SMA-### <message>` body convention. Documented as recommended-with-bot-exceptions, not gated. See §6.
- Auto-fix or auto-rewrite of bad messages.
- Migrating old commits. Spot-check of `git log --all` shows existing history is already valid under the allowlist proposed in §3.
- Adding an `xtask install-hooks` ceremony. cargo-husky's build-script-driven install handles this; if it proves flaky in practice we revisit in a follow-up.
- Auto-merging or revalidating bot PRs. Bots emit valid Conventional Commits under the §3 allowlist with no bypass required (§7).

**Deliverables** (file list):

```
.github/workflows/ci.yml                                          # add `commits` job
.github/workflows/pr-title.yml                                    # NEW
.versionrc                                                        # NEW (workspace root; YAML, auto-discovered by convco)
crates/paigasus-helikon/Cargo.toml                                # add cargo-husky dev-dep
.cargo-husky/hooks/commit-msg                                     # NEW (workspace root; cargo-husky resolves relative to .git)
CONTRIBUTING.md                                                   # new "Conventional Commits" section
docs/superpowers/specs/2026-05-17-sma-335-…-design.md             # this doc
docs/superpowers/plans/2026-05-17-sma-335-….md                    # follow-up plan
```

## 2. Tool choice: convco

The ticket pre-selects convco. Confirmed reasoning:

- Rust-native single static binary. No Node toolchain to introduce alongside the Rust workspace.
- `convco check` is exactly the linter we want; we do not need the full cocogitto suite because release-plz already owns version bumps and changelogs.
- Pre-built binary releases on GitHub make CI installation fast via `taiki-e/install-action` — already the standard install mechanism in this repo (used by `audit.yml` and `deny.yml` for cargo-audit/cargo-deny). Consistency over novelty.

**Pin strategy.** Use `taiki-e/install-action@v2` with `tool: convco@<pinned-version>`. The exact version pin is chosen in the implementation plan and surfaces as a single `env:` variable in `ci.yml` so future bumps are one-line edits.

## 3. Scope allowlist — hybrid

The ticket says "scope is optional; if present, must be one of the crate names." Real commit history already uses many non-crate scopes:

```
docs(claude)      docs(readme)        docs(specs)
docs(plan)        docs(contributing)  chore(repo)
chore(release)    chore(deps)         ci(workflows)
fix(workflows)    feat(workspace)     feat(facade)
```

Dependabot produces `chore(deps): …`. **Strict crate-names-only would break our own history and Dependabot.**

Three options were considered:

- **(A) Strict (crate names only).** Matches ticket wording verbatim. Breaks Dependabot, breaks ~half of past commit subjects, forces every future "edit a workflow" commit into a contortion. *Rejected.*
- **(B) Wide-open (any kebab-case token).** Loses signal — anyone can invent a scope. *Rejected.*
- **(C) Hybrid: crate-name set + a small, listed set of cross-cutting scopes.** **Selected.**

### 3.1 The allowlist

Kebab-case throughout, no `paigasus-helikon-` prefix on crate names:

**Crate scopes** (one per workspace member; the facade `paigasus-helikon` collapses to `facade` for brevity):

```
core, cli, facade, macros, mcp, tools, evals,
providers, providers-openai, providers-anthropic,
runtime, runtime-tokio, runtime-axum, runtime-temporal, runtime-agentcore
```

**Cross-cutting scopes** (already in active use; documented purpose each):

| Scope | Use case |
|---|---|
| `workspace` | Edits to root `Cargo.toml`, workspace-level config that fans out to every crate |
| `workflows` | Changes to `.github/workflows/*.yml` |
| `ci` | CI-adjacent edits that aren't a workflow file (e.g., `scripts/check-doc-coverage.sh`) |
| `deps` | Dependency bumps. Reserved primarily for Dependabot but allowed for humans |
| `release` | release-plz configuration (`release-plz.toml`, `.github/workflows/release-plz.yml`) |
| `repo` | Top-level repo hygiene (`.gitignore`, `.editorconfig`, `rust-toolchain.toml`) |
| `docs` | Generic docs changes that aren't scoped to a specific docs file/directory |
| `contributing` | Edits to `CONTRIBUTING.md` |
| `readme` | Edits to `README.md` |
| `claude` | Edits to `CLAUDE.md` |
| `spec`, `specs` | Edits under `docs/superpowers/specs/` (either singular or plural form, matching existing history) |
| `plan` | Edits under `docs/superpowers/plans/` |
| `lints` | Workspace-wide lint policy changes (`[workspace.lints]` table, per-crate opt-in shims) |

### 3.2 Mechanism — single source of truth

`.versionrc` declares the canonical regex. `pr-title.yml`'s `scopes:` list mirrors the same set. CONTRIBUTING.md documents both as derived from `.versionrc`. When the allowlist changes, both files must change together; the implementation plan adds a `<!-- keep-in-sync-with: .versionrc -->` comment in `pr-title.yml`.

**convco config format.** convco 0.6.3 auto-discovers `.versionrc` at the git repo root. The file is YAML (not TOML). Scope enforcement uses the top-level `scopeRegex` key (camelCase). Type enforcement uses a top-level `types` list of objects with `{type, increment, section, hidden}` fields (matching the schema shown by `convco config --default`). A `description.length.min: 1` override avoids the default minimum-length gate (default is 10 characters) blocking short-but-valid descriptions.

## 4. The local hook — cargo-husky

### 4.1 Placement

cargo-husky's build script (in the `cargo-husky` dep itself) resolves the hook source directory by walking up the filesystem until it finds a `.git` directory, then reading from `.cargo-husky/hooks/` at that location. **The crate that declares the dev-dependency only controls when the install fires, not where hooks are read from.** Hooks therefore live at the workspace root:

```
.cargo-husky/hooks/commit-msg     (workspace root, NOT under the facade crate)
```

The dev-dep itself lives on the facade crate (`paigasus-helikon`) so `cargo test -p paigasus-helikon --no-run` triggers the install (§4.2).

**Correction note (verified 2026-05-17):** The original spec said cargo-husky required `.cargo-husky/hooks/` under the consumer crate. That was wrong — cargo-husky resolves relative to `.git`. The first `cargo test` attempt that placed the hooks under `crates/paigasus-helikon/.cargo-husky/` failed with `InvalidUserHooksDir`, confirming the workspace-root location is the correct one.

### 4.2 Activation footgun

cargo-husky installs hooks via a build script that runs when cargo resolves the facade's dev-dependencies. In practice this means **`cargo test -p paigasus-helikon --no-run` (or any `cargo test`/`cargo build --tests` that pulls the facade)** triggers it. `cargo build --workspace` without `--tests` will not reliably trigger it because the build script only fires when the dev-dep graph is realized.

CONTRIBUTING.md will document this explicitly:

> After cloning, run `cargo test -p paigasus-helikon --no-run` once. This is what installs the local commit-msg hook. If you skip this step the hook is not present and bad messages reach CI instead.

The ticket's "Hooks install automatically on first `cargo build` after clone" is slightly optimistic; the corrected wording above is the safe form.

### 4.3 Hook script

```sh
#!/usr/bin/env sh
# .cargo-husky-managed commit-msg hook for paigasus-helikon.
# See SMA-335 design doc and CONTRIBUTING.md "Conventional Commits".
# Bypass for emergencies: git commit --no-verify

if ! command -v convco >/dev/null 2>&1; then
  echo "commit-msg hook: convco not on PATH." >&2
  echo "Install: cargo install convco --locked" >&2
  echo "  alternates: cargo binstall convco   |   brew install convco" >&2
  exit 1
fi

exec convco check --from-stdin < "$1"
```

The exit-with-hint behavior on missing `convco` is intentional: a silent pass would defeat the hook's purpose; the one-line install hint converts a frustrating failure into a self-serve fix.

## 5. CI jobs

### 5.1 `ci / commits` (added to `.github/workflows/ci.yml`)

```yaml
commits:
  runs-on: ubuntu-latest
  if: github.event_name == 'pull_request'
  steps:
    - uses: actions/checkout@v6
      with:
        fetch-depth: 0
    - uses: taiki-e/install-action@v2
      with:
        tool: convco@<pinned-version>
    - run: convco check ${{ github.event.pull_request.base.sha }}..HEAD
```

Key choices:

- **`if: github.event_name == 'pull_request'`** — nothing to lint on push-to-main; the squashed commit there was already gated by `pr-title` before merge.
- **`base.sha` over `origin/main`** — `base.sha` is the merge-base GitHub computed at PR open/sync; `origin/main` drifts if `main` moves while the PR is open and produces false positives on commits the PR did not author.
- **`fetch-depth: 0`** — required to walk the range; the default shallow clone has only the PR head.
- **Added to required-status-checks in SMA-309** — see §8.

### 5.2 `.github/workflows/pr-title.yml` (NEW)

```yaml
name: pr-title
on:
  pull_request_target:
    types: [opened, edited, synchronize]
permissions:
  pull-requests: read
  statuses: write
jobs:
  pr-title:
    runs-on: ubuntu-latest
    steps:
      - uses: amannn/action-semantic-pull-request@<pinned-sha>
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        with:
          types: |
            feat
            fix
            chore
            docs
            refactor
            test
            perf
            style
            build
            ci
            revert
          # keep-in-sync-with: .versionrc scopeRegex
          scopes: |
            core
            cli
            facade
            macros
            mcp
            tools
            evals
            providers
            providers-openai
            providers-anthropic
            runtime
            runtime-tokio
            runtime-axum
            runtime-temporal
            runtime-agentcore
            workspace
            workflows
            ci
            deps
            release
            repo
            docs
            contributing
            readme
            claude
            spec
            specs
            plan
            lints
          requireScope: false
          subjectPattern: '^([A-Z]{2,4}-\d+ )?[^A-Z].+$'
          subjectPatternError: |
            PR title subject must not start with an uppercase letter.
            Use sentence case after the colon, e.g. `feat(core): add Model trait`.
            An optional `SMA-### ` prefix is accepted, e.g. `feat(core): SMA-304 add Model trait`.
```

Three subtle decisions baked in:

- **`pull_request_target` over `pull_request`.** Required so the action can write commit statuses for PRs from forks. Permissions are minimal (`pull-requests: read`, `statuses: write`) and there is no `actions/checkout` step — the action reads PR metadata over the API and writes a status; it never executes PR-controlled code. This is the safe usage pattern for `pull_request_target` documented in GitHub's security guidance.
- **Pin to a SHA, not a tag.** `@v5` is a moving tag and would silently change behavior. Dependabot's `github-actions` ecosystem will keep this SHA updated weekly per SMA-308's `gh-actions` group.
- **`subjectPattern: ^([A-Z]{2,4}-\d+ )?[^A-Z].+$`** rejects PR titles whose subject (text after `: `) starts with a capital letter — the most common "looks-conventional-but-isn't" mistake (`feat: Add foo`). The optional leading group permits an `SMA-###` token (or any 2–4-letter Linear-style project prefix) before the lowercase requirement applies to the remainder. Worked examples:

  | Title | Subject the action sees | Matches? | Why |
  |---|---|---|---|
  | `feat(core): add Model trait` | `add Model trait` | yes | starts lowercase |
  | `feat(core): Add Model trait` | `Add Model trait` | no | starts uppercase |
  | `feat(core): SMA-304 add Model trait` | `SMA-304 add Model trait` | yes | optional group consumes `SMA-304 `, remainder starts lowercase |
  | `feat(core): SMA-304 Add Model trait` | `SMA-304 Add Model trait` | no | optional group consumes `SMA-304 `, remainder starts uppercase |

### 5.3 `.versionrc`

> **Implementation note (verified 2026-05-17):** The plan originally proposed `convco.toml` (TOML format) with keys `scope_regex` and `types = [...]`. This was incorrect. convco 0.6.3 uses YAML format, auto-discovers the file as `.versionrc` at the git repo root, and expects `scopeRegex` (camelCase, top-level) for scope enforcement. Types are specified as full objects matching the schema from `convco config --default`. The deliverables list in §1 is updated to reflect `.versionrc` instead of `convco.toml`.

```yaml
# Conventional Commits enforcement for paigasus-helikon.
# Single source of truth for type + scope allowlists.
# See SMA-335 design doc and CONTRIBUTING.md "Conventional Commits".
# convco auto-discovers this file at the git repo root.
types:
- {type: feat,     increment: Minor, section: Features,       hidden: false}
- {type: fix,      increment: Patch,  section: Fixes,          hidden: false}
- {type: build,    increment: None,   section: Other,          hidden: true}
- {type: chore,    increment: None,   section: Other,          hidden: true}
- {type: ci,       increment: None,   section: Other,          hidden: true}
- {type: docs,     increment: None,   section: Documentation,  hidden: true}
- {type: style,    increment: None,   section: Other,          hidden: true}
- {type: refactor, increment: None,   section: Other,          hidden: true}
- {type: perf,     increment: None,   section: Other,          hidden: true}
- {type: test,     increment: None,   section: Other,          hidden: true}
- {type: revert,   increment: None,   section: Other,          hidden: true}
scopeRegex: '^(core|cli|facade|macros|mcp|tools|evals|providers|providers-openai|providers-anthropic|runtime|runtime-tokio|runtime-axum|runtime-temporal|runtime-agentcore|workspace|workflows|ci|deps|release|repo|docs|contributing|readme|claude|spec|specs|plan|lints)$'
description:
  length:
    min: 1
```

The implementation verification step (`convco check` on fixtures of known-bad messages) confirmed enforcement fires before merge.

## 6. The SMA-### body convention

Current `CONTRIBUTING.md` formalizes `<type>(<scope>): SMA-### <message>`. convco cannot enforce the SMA-### portion — its grammar applies to type+scope+description structure, not the interior of the description.

Three options were considered:

- **(A) Drop SMA-### from the documented format.** Cleanest, but loses an inline Linear backlink convention developers find useful when scanning history.
- **(B) Keep it documented as "recommended, with bot exceptions", no CI gate.** **Selected.** Bots (Dependabot `chore(deps):`, release-plz `chore: release v…`) don't include SMA-### either, so non-enforcement matches the reality we already accept. Branch name still carries the ticket ID, Linear's GitHub integration still backlinks via branch and PR.
- **(C) Add a separate `grep -P 'SMA-\d+'` CI step**, with bot-author bypass. Possible but adds maintenance for marginal benefit; deferred until/unless we feel the pain.

`CONTRIBUTING.md`'s wording shifts from "Use the … prefix with the Linear ticket ID" (absolute) to "Include `SMA-###` in the subject when the change is tied to a Linear ticket. Bot-authored commits (Dependabot, release-plz) are exempt." (recommended-with-exceptions).

## 7. Bot interaction

Every legitimate bot already produces valid output under the §3 allowlist. No bot bypass list is required.

| Actor | Produces | Passes? | Reason |
|---|---|---|---|
| `dependabot[bot]` cargo group | `chore(deps): bump tokio from 1.40 to 1.41` | Yes | Type `chore`, scope `deps` both in allowlist |
| `dependabot[bot]` gh-actions | `chore(deps): bump actions/checkout from 5 to 6` | Yes | Same |
| `release-plz[bot]` PR title | `chore: release v0.1.0` | Yes | Type `chore`, scope optional |
| `release-plz[bot]` PR commits | Same as title | Yes | Same |
| Human ticket PR | `feat(core): SMA-304 add Model trait` | Yes | `feat`/`core` in allowlist; SMA-### prefix accepted by §5.2's subjectPattern extension |
| Bootstrap edits | `chore(workspace): SMA-307 bump versions` | Yes | `chore`/`workspace` both in allowlist |

This is a property the spec deliberately asserts; if a future bot is introduced (e.g., a docs auto-formatter), the spec is amended *before* the bot is enabled to confirm its output passes.

## 8. Cross-ticket touchpoints

### 8.1 SMA-309 (branch protection + CODEOWNERS) — blocked by this ticket

SMA-309 adds branch protection rules that require specific CI checks. This ticket contributes **two new required-status-check IDs**:

- `ci / commits`
- `pr-title / pr-title`

SMA-309's spec reserves slots for SMA-335; this doc names them concretely. The bypass actors list in SMA-309 (release-plz[bot], dependabot[bot]) does **not** need to bypass these checks — the bots produce valid output (§7).

### 8.2 SMA-310 (CONTRIBUTING.md) — already landed

CONTRIBUTING.md exists. This ticket replaces the current "Commit messages" stub (currently ~10 lines) with a full "Conventional Commits" section containing:

- Allowed types and their semver effect (`feat` → minor, `fix` → patch, `feat!` / `BREAKING CHANGE:` footer → major, others non-bumping).
- Scope allowlist with the §3.1 categorization (crate scopes vs cross-cutting scopes), each cross-cutting scope's purpose.
- 3–5 good examples and 3–5 bad examples with reasons.
- Local hook activation (`cargo test -p paigasus-helikon --no-run`) and bypass (`git commit --no-verify`).
- Bot exceptions (no SMA-### in bot commits; bots produce valid CC without bypass).
- Pointer to `.versionrc` as canonical source.

### 8.3 SMA-307 (release-plz) — no file change

The `BREAKING CHANGE:` footer acceptance criterion is exercised in the next real release cycle that includes such a commit; no synthetic test is created in this PR.

### 8.4 SMA-308 (Dependabot) — no file change

SMA-308 §11.1's switch to the clean `chore(deps):` form is already merged. SMA-335 locks it in by gating it.

## 9. Verification

### Pre-merge (in this PR)

1. `convco check <base>..HEAD` passes locally on the SMA-335 feature branch's own commits.
2. CI `ci / commits` job is green on the SMA-335 PR.
3. CI `pr-title / pr-title` job is green on the SMA-335 PR.
4. After a fresh clone, `cargo test -p paigasus-helikon --no-run` installs `.git/hooks/commit-msg` (verified by `ls .git/hooks/commit-msg`).
5. Negative test (manual, pre-PR): `git commit -m wip` is rejected by the local hook with a convco error.
6. Negative test (manual, pre-PR): `convco check` rejects a fixture commit with type `frobnicate` and accepts a fixture with type `feat`.
7. Fixture confirms scope enforcement actually fires: `convco check` rejects `feat(notascope): foo` and accepts `feat(core): foo`. If this fails, §5.3's `scope_regex` key name was wrong and the spec is amended.

### Post-merge (the ticket's acceptance criteria as observational checks)

- A test PR titled `Some change` is blocked by the `pr-title` check.
- A test PR with commit `fix typo` (no type) is blocked by `ci / commits`.
- Valid forms (`feat(core): add Model trait`, `fix(providers-openai): handle 429`, `chore(deps): bump tokio`) all pass both gates.
- A `BREAKING CHANGE:` footer in a future `feat:` commit triggers a major bump in the next release-plz PR (deferred — exercised in normal release flow, per SMA-307 acceptance criterion).

## 10. Decisions log

| Decision | Choice | Why |
|---|---|---|
| Tool | **convco** | Rust-native single binary; no Node toolchain; release-plz already covers bumps/changelogs so a lighter linter suffices. |
| Scope allowlist breadth | **Hybrid (crate scopes + cross-cutting scopes)** | Strict crate-names-only would break Dependabot and ~half of historical commits. Hybrid keeps signal without forcing contortions. |
| cargo-husky placement | **`.cargo-husky/hooks/` at workspace root** | cargo-husky's build script walks up to `.git` and reads `.cargo-husky/hooks/` from there — independent of which crate declares the dev-dep. The original spec said the opposite; corrected at implementation time after `InvalidUserHooksDir` failure. |
| cargo-husky activation note | **Explicit `cargo test -p paigasus-helikon --no-run`** | `cargo build --workspace` without `--tests` does not reliably trigger the build script. The ticket's "first cargo build" wording is slightly optimistic. |
| SMA-### body enforcement | **Documented but not gated** | convco cannot enforce body content. Bots are already exempt in practice. A separate regex gate is deferrable. |
| PR title casing | **subjectPattern `^([A-Z]{2,4}-\d+ )?[^A-Z].+$`** | Rejects `feat: Add foo` (the most common mistake) while allowing `feat(core): SMA-304 add foo`. |
| PR title trigger | **`pull_request_target`** | Required so fork PRs can write commit statuses. Used safely — no checkout, minimal permissions, no PR-controlled code executed. |
| Action version pinning | **Pin to SHA, not `@v5`** | Moving tags silently change behavior; Dependabot keeps SHAs updated weekly. |
| convco install in CI | **`taiki-e/install-action@v2`** | Matches the install mechanism used by `audit.yml`/`deny.yml` — consistency over novelty. |
| Bot bypass list | **None** | Every legitimate bot already produces valid output; bypass would weaken the gate without benefit. |

## 11. Open follow-ups (not blocking this ticket)

- If a future bot is introduced (e.g., docs auto-formatter), amend §7 *before* enabling it to confirm its output passes both gates.
- If convco's scope enforcement turns out to be regex-name- or location-different from §5.3's proposed shape, amend §5.3 with the verified form when discovered.
- If cargo-husky's activation footgun (§4.2) bites enough contributors to be a recurring support burden, replace it with a `scripts/install-hooks.sh` invoked from a CONTRIBUTING.md one-liner. Track as a fresh ticket; do not retrofit into this one.
