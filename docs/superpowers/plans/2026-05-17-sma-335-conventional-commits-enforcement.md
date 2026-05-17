# SMA-335 — Conventional Commits Enforcement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce Conventional Commits at three layers — local commit-msg hook, in-PR commit lint, PR-title check — so SMA-307 (release-plz) and SMA-308 (Dependabot prefix) gain a real gate instead of an assumed convention.

**Architecture:** `.versionrc` (YAML, auto-discovered by convco at the git repo root) is the single source of truth for allowed types and scope regex. `convco check` runs (a) from a cargo-husky-installed `commit-msg` hook locally and (b) in a new `ci / commits` GitHub Actions job. A separate `pr-title` workflow uses `amannn/action-semantic-pull-request` against the same allowlist (mirrored, comment-linked to `.versionrc`). The whole change ships behind the SMA-335 feature branch; nothing lands on `main` outside a PR.

> **Config format correction (verified 2026-05-17):** convco 0.6.3 uses YAML (not TOML) and auto-discovers the config file as `.versionrc` at the git repo root. The config key for scope enforcement is `scopeRegex` (camelCase, top-level). Types are full objects (`{type, increment, section, hidden}`). The original plan proposed `convco.toml` with TOML syntax and `scope_regex` — both incorrect. Steps below reflect the verified form.

**Tech Stack:** convco (Rust binary), cargo-husky (v1, dev-dep on facade crate), `amannn/action-semantic-pull-request` (pinned to SHA), `taiki-e/install-action` for the CI convco install (matches existing audit/deny pattern).

**Branch:** `feature/sma-335-enforce-conventional-commits-ci-pr-title-local-hook` (already checked out; spec already committed there as `94df409`).

**Spec:** `docs/superpowers/specs/2026-05-17-sma-335-conventional-commits-enforcement-design.md`.

---

## Pre-flight (one-time per machine)

- [ ] **Step P1: Install convco locally**

  Run:
  ```bash
  cargo install convco --locked
  # OR (faster, prebuilt): cargo binstall convco
  ```
  Expected: `convco` is on `$PATH`. Verify:
  ```bash
  convco --version
  ```
  Expected output (version may differ; record the exact value for use in Task 3 — at implementation time this was `0.6.3`):
  ```
  convco 0.6.3
  ```

- [ ] **Step P2: Confirm working tree state**

  Run:
  ```bash
  git status && git branch --show-current
  ```
  Expected:
  ```
  On branch feature/sma-335-enforce-conventional-commits-ci-pr-title-local-hook
  Your branch is ahead of 'main' by 1 commit.
  nothing to commit, working tree clean
  feature/sma-335-enforce-conventional-commits-ci-pr-title-local-hook
  ```
  If branch is `main` or differs, stop and switch to the SMA-335 feature branch.

---

## Task 1: `.versionrc` + verified enforcement

**Files:**
- Create: `.versionrc` (YAML; convco auto-discovers this file name at git repo root)
- (None modified.)
- Test: ad-hoc shell fixtures (no test file committed)

**Why:** The config is the single source of truth referenced by the local hook, CI job, and (mirrored) the PR-title workflow. Verify the config actually rejects bad input and accepts good input before building anything else on top of it.

- [ ] **Step 1.1: Write the "failing test" — a known-bad commit message that *should* be rejected**

  No file yet. Just record the negative-fixture inputs we expect convco to reject after Step 1.3 lands the config:
  | Input | Why it should fail |
  |---|---|
  | `wip` | No type |
  | `frobnicate(core): foo` | Type not in allowlist |
  | `feat(notascope): foo` | Scope not in allowlist |
  | `feat: Foo` | Description starts uppercase (note: convco does NOT enforce subject case; this fixture exists only for the `pr-title` job in Task 4 — convco passes it) |

  And the positive fixtures we expect to pass:
  | Input | Why it should pass |
  |---|---|
  | `feat(core): add Model trait` | Type+scope+desc all valid |
  | `chore(deps): bump tokio from 1.40 to 1.41` | Dependabot's actual output |
  | `chore: release v0.1.0` | release-plz's actual output; scope optional |
  | `feat(facade)!: SMA-304 add breaking change` | Type+scope+breaking+SMA-### prefix all valid |

- [ ] **Step 1.2: Run convco against fixtures with no config — confirm there's no enforcement yet**

  Run:
  ```bash
  echo "frobnicate(core): foo" | convco check --from-stdin
  ```
  Expected (with no `.versionrc` present): convco 0.6.3 has a built-in default type allowlist that already rejects unknown types. However, scope enforcement is absent without a config — `feat(notascope): foo` exits 0. This confirms the config is what adds scope enforcement.

  If `frobnicate(core): foo` exits 1 without a config file, that is expected for convco 0.6.3 (it has a built-in type allowlist). The important baseline check is that scope violations pass without a config.

- [ ] **Step 1.3: Write `.versionrc`**

  Create `.versionrc` at the workspace root (YAML format — convco auto-discovers this file name):
  ```yaml
  # Conventional Commits enforcement for paigasus-helikon.
  # Single source of truth for allowed types and scopes.
  # See docs/superpowers/specs/2026-05-17-sma-335-…-design.md
  # and CONTRIBUTING.md "Conventional Commits".
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

- [ ] **Step 1.4: Run negative fixtures — they should now fail**

  Run each and confirm exit code is non-zero with a useful error:
  ```bash
  echo "wip"                       | convco check --from-stdin ; echo "exit=$?"
  echo "frobnicate(core): foo"     | convco check --from-stdin ; echo "exit=$?"
  echo "feat(notascope): foo"      | convco check --from-stdin ; echo "exit=$?"
  ```
  Expected: each prints a convco error and `exit=1` (or any non-zero).

  **If `feat(notascope): foo` exits 0**, the `.versionrc` is not being auto-discovered. Check:
  - File is named exactly `.versionrc` (with leading dot) at the git repo root.
  - The `scopeRegex` key is spelled exactly as shown (camelCase, no hyphens).
  - convco is being run from within the git repo (or with `-C /path/to/repo`).

- [ ] **Step 1.5: Run positive fixtures — they should now pass**

  Run each and confirm exit code 0:
  ```bash
  echo "feat(core): add Model trait"                         | convco check --from-stdin ; echo "exit=$?"
  echo "chore(deps): bump tokio from 1.40 to 1.41"           | convco check --from-stdin ; echo "exit=$?"
  echo "chore: release v0.1.0"                               | convco check --from-stdin ; echo "exit=$?"
  printf "feat(facade)!: SMA-304 add breaking change\n"      | convco check --from-stdin ; echo "exit=$?"
  ```
  Expected: every line ends with `exit=0`.

- [ ] **Step 1.6: Run convco against the branch's existing commits**

  Run:
  ```bash
  convco check origin/main..HEAD
  ```
  Expected: passes. (At this point the only commit on the branch is `94df409 docs(spec): SMA-335 add Conventional Commits enforcement design`, which uses type `docs` and scope `spec` — both in the allowlist.)

- [ ] **Step 1.7: Commit**

  Run:
  ```bash
  git add .versionrc
  git commit -m "$(cat <<'EOF'
  chore(workspace): SMA-335 add convco config for commit linting

  Declares allowed Conventional Commit types and scope regex used by
  the local commit-msg hook (Task 2) and the ci/commits CI job
  (Task 3). The allowlist is hybrid per the spec: crate scopes plus a
  small set of cross-cutting scopes already in active use so
  Dependabot and historical commits pass without bypass.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```
  Expected: commit created. (The local hook does not exist yet; commit succeeds without enforcement.)

---

## Task 2: cargo-husky dev-dep + commit-msg hook

**Files:**
- Modify: `crates/paigasus-helikon/Cargo.toml` (add `[dev-dependencies]` block)
- Create: `.cargo-husky/hooks/commit-msg` (workspace root — cargo-husky resolves the hook source dir relative to `.git`, not the dev-dep crate)
- Create: `.cargo-husky/hooks/pre-commit` (a do-nothing override — see Step 2.3)
- Test: manual `git commit` attempts

**Why:** Local enforcement is the fastest feedback loop. The hook is installed by cargo-husky's build script when the facade's dev-deps are realized.

> **Location correction (verified 2026-05-17):** The plan originally put hooks under `crates/paigasus-helikon/.cargo-husky/`, based on the (mistaken) reading that cargo-husky's source dir is relative to the consumer crate. Reality: cargo-husky's `build.rs` walks up to `.git` and uses `.cargo-husky/hooks/` at the git root. The first `cargo test` attempt under the crate dir failed with `InvalidUserHooksDir`. Hooks live at the workspace root; Steps 2.3, 2.4, 2.7 reflect the verified location.

- [ ] **Step 2.1: Confirm the negative test currently passes (i.e., no hook is enforcing)**

  Run (without committing):
  ```bash
  echo "wip" > /tmp/sma335-msg
  git hook run --ignore-missing commit-msg /tmp/sma335-msg ; echo "exit=$?"
  ```
  Expected: `exit=0` (no commit-msg hook in `.git/hooks/`). If `commit-msg` already exists in `.git/hooks/`, inspect it — likely a leftover from another tool. Decide whether to back it up and remove before proceeding.

- [ ] **Step 2.2: Add cargo-husky as a dev-dependency on the facade crate**

  Read `crates/paigasus-helikon/Cargo.toml` first to see its current shape, then add (or append to) a `[dev-dependencies]` block:
  ```toml
  [dev-dependencies]
  # Installs git hooks from .cargo-husky/hooks/ when this crate's
  # dev-deps are realized (e.g. `cargo test -p paigasus-helikon --no-run`).
  # See SMA-335 design doc §4.
  cargo-husky = { version = "1", default-features = false, features = ["user-hooks"] }
  ```
  Notes on the feature/version choice:
  - `default-features = false` disables cargo-husky's prepackaged hooks (we want only our own).
  - `features = ["user-hooks"]` enables reading from `.cargo-husky/hooks/`.
  - Pin to the `1` major series. cargo-husky 2.x (if/when it ships) may change semantics — don't auto-upgrade across major.

- [ ] **Step 2.3: Create the `commit-msg` hook**

  Create `.cargo-husky/hooks/commit-msg` at the **workspace root**:
  ```sh
  #!/usr/bin/env sh
  # .cargo-husky-managed commit-msg hook for paigasus-helikon.
  # Enforces Conventional Commits via convco (see .versionrc).
  # Bypass for emergencies: git commit --no-verify

  if ! command -v convco >/dev/null 2>&1; then
    echo "commit-msg hook: convco not on PATH." >&2
    echo "Install: cargo install convco --locked" >&2
    echo "  alternates: cargo binstall convco   |   brew install convco" >&2
    exit 1
  fi

  exec convco check --from-stdin < "$1"
  ```

  Also create `.cargo-husky/hooks/pre-commit` (workspace root) as a deliberate no-op:
  ```sh
  #!/usr/bin/env sh
  # Intentional no-op. cargo-husky with user-hooks feature installs every
  # file in .cargo-husky/hooks/ into .git/hooks/. We declare pre-commit
  # explicitly so an empty/missing file doesn't get filled in later by
  # accident and surprise contributors with new pre-commit behavior.
  exit 0
  ```

  Make both executable:
  ```bash
  chmod +x .cargo-husky/hooks/commit-msg
  chmod +x .cargo-husky/hooks/pre-commit
  ```

- [ ] **Step 2.4: Trigger cargo-husky's build script to install the hooks**

  Run:
  ```bash
  cargo test -p paigasus-helikon --no-run
  ```
  Expected: cargo compiles cargo-husky's build script, which copies hooks into `.git/hooks/`. The first few lines of output should mention `cargo-husky`.

  Verify:
  ```bash
  ls -la .git/hooks/commit-msg .git/hooks/pre-commit
  ```
  Expected: both files exist and are executable.

  Diff against the source:
  ```bash
  diff .git/hooks/commit-msg .cargo-husky/hooks/commit-msg
  ```
  Expected: nearly identical (cargo-husky 1.5.x prepends two lines: a blank `#` line and `# This hook was set by cargo-husky v1.5.0: …`). The body should match exactly.

- [ ] **Step 2.5: Negative test — the hook should now reject bad messages**

  Without staging anything:
  ```bash
  echo "wip" > /tmp/sma335-msg
  .git/hooks/commit-msg /tmp/sma335-msg ; echo "exit=$?"
  ```
  Expected: convco error printed, `exit=1`.

  Repeat for type and scope violations:
  ```bash
  echo "frobnicate(core): foo" > /tmp/sma335-msg && .git/hooks/commit-msg /tmp/sma335-msg ; echo "exit=$?"
  echo "feat(notascope): foo"  > /tmp/sma335-msg && .git/hooks/commit-msg /tmp/sma335-msg ; echo "exit=$?"
  ```
  Expected: both `exit=1`.

- [ ] **Step 2.6: Positive test — the hook should accept good messages**

  ```bash
  echo "feat(core): add Model trait" > /tmp/sma335-msg && .git/hooks/commit-msg /tmp/sma335-msg ; echo "exit=$?"
  ```
  Expected: `exit=0`.

- [ ] **Step 2.7: Commit**

  ```bash
  git add crates/paigasus-helikon/Cargo.toml .cargo-husky/
  git commit -m "$(cat <<'EOF'
  chore(facade): SMA-335 add cargo-husky dev-dep and commit-msg hook

  cargo-husky installs hooks via build script when the facade's
  dev-deps are realized (cargo test -p paigasus-helikon --no-run).
  The commit-msg hook execs `convco check --from-stdin`, enforcing
  the allowlist from .versionrc. An empty pre-commit hook is
  declared explicitly so accidental additions later don't surprise
  contributors.

  Bypass: git commit --no-verify.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```
  Expected: commit succeeds (the hook fires on its own message; subject `SMA-335 add cargo-husky dev-dep and commit-msg hook` is valid per Step 1.5's positive fixture pattern).

  If `Cargo.lock` is modified by the cargo-husky add, stage that too in the same commit.

---

## Task 3: `ci / commits` job in `.github/workflows/ci.yml`

**Files:**
- Modify: `.github/workflows/ci.yml` (append new `commits:` job)
- Test: pushed CI run on the PR (verified in Task 6)

**Why:** Forks can't run the local hook. CI catches what the hook misses and the per-commit history.

- [ ] **Step 3.1: Read current `ci.yml`**

  Read `.github/workflows/ci.yml` to see the existing job structure. Confirm the `env:` block declares `CARGO_TERM_COLOR` and `NIGHTLY_TOOLCHAIN`. The new job will sit beside the existing five (`fmt`, `clippy`, `test`, `docs`, `doc-coverage`).

- [ ] **Step 3.2: Resolve the convco version pin**

  Record the convco version installed locally in Step P1:
  ```bash
  convco --version
  ```
  Use that exact version (e.g., `0.6.2`) in the workflow below. Pinning a specific version (not `latest`) matches the audit/deny pattern.

- [ ] **Step 3.3: Append the `commits` job**

  Per the CLAUDE.md "Workflow conventions" rule, resolve the latest commit SHA for each action and pin to it. The values resolved at implementation time were `actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd` (v6.0.2) and `taiki-e/install-action@7be9fd86bd1707236395105d6e9329dd1511a7e1` (v2.79.0). If re-running this plan, re-resolve via:
  ```bash
  gh api repos/actions/checkout/releases/latest | jq -r '.tag_name'
  gh api repos/actions/checkout/git/ref/tags/<tag> | jq -r '.object.sha'
  gh api repos/taiki-e/install-action/releases/latest | jq -r '.tag_name'
  gh api repos/taiki-e/install-action/git/ref/tags/<tag> | jq -r '.object.sha'
  ```

  Add this job to `.github/workflows/ci.yml` after `doc-coverage:` (preserve the existing indentation style — 2-space, jobs at top of file):
  ```yaml
    commits:
      runs-on: ubuntu-latest
      if: github.event_name == 'pull_request'
      steps:
        # actions/checkout v6.0.2
        - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd
          with:
            fetch-depth: 0
        # taiki-e/install-action v2.79.0
        - uses: taiki-e/install-action@7be9fd86bd1707236395105d6e9329dd1511a7e1
          with:
            tool: convco@0.6.3
        - run: convco check ${{ github.event.pull_request.base.sha }}..HEAD
  ```
  Replace `0.6.3` with the version recorded in Step 3.2 if different.

- [ ] **Step 3.4: Sanity-check the YAML is parseable**

  Run:
  ```bash
  python3 -c 'import yaml,sys; yaml.safe_load(open(".github/workflows/ci.yml"))' && echo OK
  ```
  Expected: `OK`. If `yaml` isn't available, use any YAML linter that's on `$PATH` (e.g., `yamllint .github/workflows/ci.yml`).

- [ ] **Step 3.5: Local convco repro against the PR range**

  Run:
  ```bash
  convco check origin/main..HEAD
  ```
  Expected: pass. (Three commits at this point — the spec commit from `94df409`, Task 1's `.versionrc` commit, and Task 2's cargo-husky commit. All use valid types/scopes per the allowlist.)

- [ ] **Step 3.6: Commit**

  ```bash
  git add .github/workflows/ci.yml
  git commit -m "$(cat <<'EOF'
  ci(workflows): SMA-335 add ci/commits job for commit linting

  New CI job runs convco against the PR's commit range on every PR
  open and synchronize. Uses base.sha (not origin/main) so the
  range is stable even if main moves while the PR is open. The job
  is gated to pull_request events because there's nothing to lint
  on push-to-main (the squashed commit was already gated by
  pr-title before merge).

  Becomes a required status check in SMA-309.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 4: `.github/workflows/pr-title.yml`

**Files:**
- Create: `.github/workflows/pr-title.yml`
- Test: PR-title gate run on the SMA-335 PR (verified in Task 6)

**Why:** Squash-merge makes the PR title *the* main-branch commit message. release-plz parses that. The PR-title gate is what makes the assumption real.

- [ ] **Step 4.1: Resolve the action SHA pin**

  Per the CLAUDE.md "Workflow conventions" rule, resolve the latest release of `amannn/action-semantic-pull-request` and pin to its commit SHA. Use the `releases/latest` endpoint (which targets the current stable major automatically):
  ```bash
  TAG=$(gh api repos/amannn/action-semantic-pull-request/releases/latest | jq -r '.tag_name')
  gh api repos/amannn/action-semantic-pull-request/git/ref/tags/$TAG \
    | jq -r '.object | "type=\(.type) sha=\(.sha)"'
  ```
  Expected output looks like: `type=tag sha=<40-hex>` (annotated tag) **or** `type=commit sha=<40-hex>` (lightweight tag).
  - If `type=commit`: use that SHA directly.
  - If `type=tag`: dereference to the commit SHA:
    ```bash
    gh api repos/amannn/action-semantic-pull-request/git/tags/<sha-from-above> \
      | jq -r '.object.sha'
    ```
    Use the dereferenced SHA.

  At implementation time (2026-05-17), `releases/latest` resolved to **v6.1.1** with commit SHA `48f256284bd46cdaab1048c3721360e808335d50` — the values embedded in Step 4.2 below.

  Record the SHA — it goes into Step 4.2. Also record the human-readable version tag (e.g., `v5.5.3`) for the comment in the workflow.

- [ ] **Step 4.2: Create `.github/workflows/pr-title.yml`**

  The values resolved at implementation time were **v6.1.1** and SHA `48f256284bd46cdaab1048c3721360e808335d50`. If you're re-running this plan against a newer release, replace both with the values from Step 4.1.
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
        # amannn/action-semantic-pull-request v6.1.1
        # Pinned to SHA; Dependabot's gh-actions group keeps this updated.
        - uses: amannn/action-semantic-pull-request@48f256284bd46cdaab1048c3721360e808335d50
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

- [ ] **Step 4.3: Sanity-check the YAML is parseable**

  ```bash
  python3 -c 'import yaml,sys; yaml.safe_load(open(".github/workflows/pr-title.yml"))' && echo OK
  ```
  Expected: `OK`.

- [ ] **Step 4.4: Commit**

  ```bash
  git add .github/workflows/pr-title.yml
  git commit -m "$(cat <<'EOF'
  ci(workflows): SMA-335 add pr-title workflow for conventional commit linting

  New workflow runs amannn/action-semantic-pull-request on PR
  open/edit/sync. Uses pull_request_target so fork PRs can get
  their status updated; permissions are minimal (pull-requests:
  read, statuses: write) and no checkout step runs PR-controlled
  code. Action is pinned to a SHA; Dependabot's gh-actions group
  keeps it updated.

  The scope list mirrors `.versionrc` — keep them in sync via the
  `keep-in-sync-with` comment. subjectPattern rejects uppercase
  message starts while accepting an optional SMA-### prefix.

  Becomes a required status check in SMA-309.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 5: `CONTRIBUTING.md` "Conventional Commits" section

**Files:**
- Modify: `CONTRIBUTING.md` (replace the existing "Commit messages" section)
- Test: rendered review (manual)

**Why:** Per spec §8.2, the docs need to cover allowed types + semver, the hybrid scope allowlist, examples, hook activation and bypass, bot exceptions, and the pointer to `.versionrc` as canonical.

- [ ] **Step 5.1: Read the current "Commit messages" section**

  Read `CONTRIBUTING.md` to confirm the current section is the ~10-line stub described in the spec (around the "## Commit messages" heading). It currently says:
  ```
  Use the Conventional-Commits-style prefix with the Linear ticket ID:

  ```text
  <type>(<scope>): SMA-### <message>
  ```

  `<type>` is one of `feat`, `fix`, `docs`, `ci`, `chore`, `refactor`, `test`. `<scope>` is the affected area (`workspace`, `facade`, `workflows`, `lints`, …). Once SMA-335 lands, a GitHub Action enforces this in PR titles too.
  ```

- [ ] **Step 5.2: Replace the section**

  Replace the entire `## Commit messages` section (heading line and body, through the blank line before `## MSRV`) with:
  ```markdown
  ## Conventional Commits

  Every commit message **and** every PR title must conform to
  [Conventional Commits 1.0](https://www.conventionalcommits.org/en/v1.0.0/),
  with the type and scope constrained as below. Three layers enforce
  this:

  | Layer | Fires when | Bypass |
  |---|---|---|
  | Local `commit-msg` hook | `git commit` | `git commit --no-verify` |
  | `ci / commits` job | PR open + sync | none — fix the message |
  | `pr-title / pr-title` job | PR open/edit/sync | none — fix the title |

  ### Allowed types and semver effect

  Mapping below applies to post-1.0 versions; release-plz adjusts the
  effective bump for pre-1.0 (`0.x.y`) automatically.

  | Type | Semver effect | Use for |
  |---|---|---|
  | `feat` | minor | New user-visible capability |
  | `fix` | patch | Bug fix |
  | `feat!` or any type with `BREAKING CHANGE:` footer | major | API break |
  | `chore`, `docs`, `refactor`, `test`, `perf`, `style`, `build`, `ci`, `revert` | none | Everything else |

  ### Scope allowlist

  Scope is optional. If present, must match one of:

  - **Crate scopes** (one per workspace member, facade collapsed to `facade`):
    `core`, `cli`, `facade`, `macros`, `mcp`, `tools`, `evals`,
    `providers`, `providers-openai`, `providers-anthropic`,
    `runtime`, `runtime-tokio`, `runtime-axum`, `runtime-temporal`, `runtime-agentcore`
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

  ### Local commit-msg hook

  The hook is installed by `cargo-husky` when the facade's dev-deps
  are realized. After cloning, run once:

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
  ```

- [ ] **Step 5.3: Confirm the replacement is self-contained**

  Read the modified `CONTRIBUTING.md` and check:
  - No dangling reference to the old `<type>(<scope>): SMA-### <message>` format with the now-removed type list (`feat, fix, docs, ci, chore, refactor, test`) elsewhere in the file.
  - The "Local pre-PR checklist" section further down (currently lists `cargo fmt`, `cargo clippy`, etc.) does not need to change — convco isn't in the local-pre-PR checklist because the commit-msg hook covers it interactively.
  - Confirm the surrounding markdown headings still flow (`## Conventional Commits` → `## MSRV` etc.).

- [ ] **Step 5.4: Commit**

  ```bash
  git add CONTRIBUTING.md
  git commit -m "$(cat <<'EOF'
  docs(contributing): SMA-335 document Conventional Commits enforcement

  Replaces the placeholder Commit messages section with the full
  Conventional Commits policy: allowed types + semver mapping,
  hybrid scope allowlist (crate scopes + cross-cutting scopes),
  good/bad examples, local hook activation + bypass, and bot
  exceptions. Points to `.versionrc` as the canonical allowlist
  source.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  EOF
  )"
  ```

---

## Task 6: Local CI repro, push, open PR, verify

**Files:** none modified.

**Why:** Reproduce every CI gate locally before pushing (per CLAUDE.md), then validate the new gates fire correctly on the PR.

- [ ] **Step 6.1: Run every existing CI gate locally**

  Per CLAUDE.md, reproduce job-for-job:
  ```bash
  cargo fmt --all -- --check
  cargo clippy --workspace --all-features --all-targets -- -D warnings
  cargo test --workspace --all-features
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
  DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
    bash scripts/check-doc-coverage.sh
  ```
  Expected: all pass. Adding `cargo-husky` as a dev-dep shouldn't trip clippy or doc coverage, but if it does (e.g., a transitive warning), resolve in a fresh `chore(facade): SMA-335 …` commit before pushing.

- [ ] **Step 6.2: Run convco against the full PR commit range one more time**

  ```bash
  convco check origin/main..HEAD
  ```
  Expected: pass against the six+ commits on the branch — spec doc, plan amendment, `.versionrc`, cargo-husky + hook, `ci/commits` job, `pr-title.yml`, `CONTRIBUTING.md`.

  If convco rejects any commit, fix locally before pushing. (The hook would have caught it on commit; this is belt-and-braces.)

- [ ] **Step 6.3: Push the branch**

  ```bash
  git push -u origin feature/sma-335-enforce-conventional-commits-ci-pr-title-local-hook
  ```
  Expected: push succeeds. No `--force` needed (this is a fresh branch).

- [ ] **Step 6.4: Open the PR**

  ```bash
  gh pr create \
    --title "chore(workspace): SMA-335 enforce Conventional Commits (CI + PR title + local hook)" \
    --body "$(cat <<'EOF'
  ## Summary

  - Adds `.versionrc` (YAML) with the allowed types and hybrid scope allowlist (crate scopes + cross-cutting scopes already in active use).
  - Installs a local `commit-msg` hook via cargo-husky on the facade crate; the hook execs `convco check --from-stdin`.
  - Adds the `ci / commits` job to `.github/workflows/ci.yml`, running `convco check` against the PR's commit range.
  - Adds `.github/workflows/pr-title.yml` using `amannn/action-semantic-pull-request` (SHA-pinned), mirroring the allowlist.
  - Rewrites `CONTRIBUTING.md`'s commit-messages section with the full policy.

  Spec: `docs/superpowers/specs/2026-05-17-sma-335-conventional-commits-enforcement-design.md`
  Plan: `docs/superpowers/plans/2026-05-17-sma-335-conventional-commits-enforcement.md`

  Becomes blocking once SMA-309 adds the two new IDs (`ci / commits`, `pr-title / pr-title`) to required status checks.

  ## Test plan

  - [ ] `ci / commits` is green on this PR.
  - [ ] **`pr-title / pr-title` cannot self-validate on this PR.** `pull_request_target` only fires for workflow files that already exist on the default branch. Since `pr-title.yml` is *introduced* by this PR, the gate doesn't run here. It will fire on the next PR opened after this one merges to `main`. The PR title above is constructed as a positive fixture so it'll pass on first run.
  - [ ] After merging, the squashed commit on `main` matches the PR title verbatim and parses cleanly under `convco check`.
  - [ ] **Post-merge negative test** (on the next PR opened after merge): a PR titled `Some change` should be rejected by `pr-title`. A PR titled `feat(unknown-scope): foo` should be rejected. A PR with a valid title should pass.

  Linear: SMA-335
  EOF
  )"
  ```
  Expected: PR URL is printed.

  **The PR title above intentionally uses type `chore` and scope `workspace`** — per the CLAUDE.md bootstrap rule, release-infrastructure changes use `chore(...)` (or `docs(...)`), never `feat`/`fix`. A `feat(workspace):` here would mis-attribute a workspace-wide minor bump on the next release-plz run. `workspace` is the right scope because the policy is workspace-wide. This title is also a positive fixture for the new gate — but the gate only fires on the *next* PR, see below.

- [ ] **Step 6.5: Verify `ci / commits` goes green**

  Watch the PR's checks:
  ```bash
  gh pr checks --watch
  ```
  Expected: every check eventually green. Specifically:
  - `ci / fmt`, `ci / clippy`, `ci / test (...)`, `ci / docs`, `ci / doc-coverage` — pre-existing; should pass.
  - `ci / commits` — NEW; should pass against the commits on the branch.
  - `pr-title / pr-title` — **does not run on this PR** (see Step 6.6 below). The check will simply not appear in the list.

  If `ci / commits` fails:
  - Read its log. If the error is the convco install (`taiki-e/install-action` couldn't find the tool), verify the pin in `ci.yml` matches an existing convco release. Fix in a fresh commit (`ci(workflows): SMA-335 correct convco version pin`).
  - If the error is a real commit rejection, the local hook should have caught it — re-run `convco check origin/main..HEAD` locally and reconcile.

- [ ] **Step 6.6: Document the `pr-title` self-validation limitation in the PR body**

  GitHub's `pull_request_target` runs workflow files from the **default branch**. Since `pr-title.yml` is introduced by *this* PR, it doesn't run on this PR — it'll fire on the next PR after merge. There is nothing to negative-test here.

  Update the PR description to flag this explicitly (so reviewers don't expect a `pr-title` check that won't appear):
  ```bash
  gh pr edit <PR_NUMBER> --body "$(cat <<'EOF'
  ... (existing summary) ...

  ## Test plan
  - [x] `ci / commits` green on this PR
  - [ ] **`pr-title / pr-title` cannot self-validate on this PR** (pull_request_target reads workflows from main; this PR introduces the file). Will fire on the next PR after merge.
  - [ ] Post-merge negative test (next PR): `Some change` title rejected; valid title passes.
  EOF
  )"
  ```

  The negative test (editing the title to an invalid form to confirm `pr-title` rejects it) must happen on a **subsequent PR** opened after this one merges — not on this PR itself.

- [ ] **Step 6.7: Report PR URL**

  Print and hand off:
  ```bash
  gh pr view --json url -q .url
  ```
  Expected: PR URL printed.

  Mark the Linear issue as ready-for-review out-of-band if needed (Linear auto-closes on merge — no manual status push required per memory).

---

## Verification summary (mirrors spec §9)

When all six tasks are complete, you should be able to confirm:

| # | Check | How |
|---|---|---|
| 1 | convco accepts every commit on the branch | `convco check origin/main..HEAD` exits 0 locally |
| 2 | `ci / commits` is green on the PR | `gh pr checks` |
| 3 | `pr-title / pr-title` starts validating on the next PR after merge | Next PR's `gh pr checks` (this PR introduces the workflow; `pull_request_target` reads from the default branch — see Task 6 / Step 6.6) |
| 4 | Fresh-clone hook install works | `cargo test -p paigasus-helikon --no-run && ls .git/hooks/commit-msg` |
| 5 | Local hook rejects `wip` | `echo wip \| .git/hooks/commit-msg /dev/stdin; echo "exit=$?"` → exit=1 |
| 6 | convco rejects type/scope violations | Step 1.4 fixtures |
| 7 | convco accepts bot prefixes | Step 1.5 fixtures |
| 8 | `pr-title` rejects bad titles | Post-merge negative test on the next PR (per Step 6.6) |
| 9 | The PR's squashed commit on `main` matches the gated PR title | After merge, `git log -1 --format=%s main` matches the PR title |

The `BREAKING CHANGE:` footer test (spec §9, post-merge item 4) is deferred to the next real release cycle that includes such a commit — synthetic exercise is not part of this PR.

---

## Risk / rollback

- **If cargo-husky misbehaves** (e.g., hook never lands despite `cargo test --no-run`): the hook is just a convenience. CI catches what the hook misses; merging without local-hook coverage is still safe. File a follow-up to replace cargo-husky with a `scripts/install-hooks.sh`.
- **If convco's scope-regex enforcement turns out to be the wrong config key** (Step 1.4 fallback): the spec already covers this; the plan amends the spec inline and re-runs fixtures before moving on.
- **If `pr-title` blocks legitimate work in the next ticket** (e.g., the next contributor proposes a scope we didn't anticipate): amend the allowlist in *both* `.versionrc` and `pr-title.yml` (kept in sync via the comment), under a fresh `chore(workspace): SMA-### add <scope> to convco allowlist` commit.
