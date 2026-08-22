# SMA-581 — markdownlint-cli2 as a required CI gate

**Status:** approved 2026-08-22; revised after adversarial challenge
**Ticket:** [SMA-581](https://linear.app/smaschek/issue/SMA-581/adopt-markdownlint-cli2-as-a-ci-gate-for-the-published-markdown)
**Branch:** `feature/sma-581-adopt-markdownlint-cli2-as-a-ci-gate-for-the-published`

## Problem

Markdown is entirely ungated in this repo. `grep -rl markdownlint .github/` returns
nothing, yet several Markdown surfaces are published artifacts:

- `docs/book/` — the public mdBook, deployed from `main` by `docs.yml`.
- `crates/*/README.md` — each crate's crates.io and docs.rs landing page.
- Root `README.md`, `CONTRIBUTING.md`, `BENCHMARKS.md`, `SECURITY.md`,
  `CODE_OF_CONDUCT.md`, `CLAUDE.md`.
- `docs/runbooks/` — executed from, not merely read.
- `docker/forkd/README.md`, `.github/PULL_REQUEST_TEMPLATE.md`.

`mdbook build` with `[output.linkcheck] warning-policy = "error"` catches broken
links and nothing else. A missing fence language, a skipped heading level, or a
malformed list renders badly on crates.io and nobody finds out.

The trigger was PR #216, where CodeRabbit — not CI — caught an `MD040` violation.

## Correcting the ticket's premise

The ticket claims *"gating the published surfaces costs one fence fix."* That is
**false**; it counted only `MD040`. Measured with `markdownlint-cli2@0.23.2`:

| Rule | Findings | Note |
| --- | --- | --- |
| `MD013` line-length | 818 | disabled as policy |
| `MD060` table-column-style | 77 | not anticipated; parameterized, auto-fixable |
| `MD032` blanks-around-lists | 10 | auto-fixable |
| `MD031` / `MD049` | 8 | auto-fixable |
| `MD025` single-title | 4 | rule does not apply to `SUMMARY.md` |
| `MD036` no-emphasis-as-heading | 4 | genuine content fix |
| `MD001` / `MD012` / `MD028` / `MD034` / `MD038` / `MD040` / `MD041` | 7 | one each |

Total 928 raw; 818 `MD013` disabled; **110 gated findings across 16 files**.

Two further corrections to the ticket:

1. **`MD033` does not need relaxing.** The ticket asserts it "would fire on the
   mdBook's `<!-- tracing-components:start -->` marker comments". It does not —
   markdownlint exempts HTML comments from `MD033`. Verified: zero `MD033` findings.
2. **`crates/*/CHANGELOG.md` must be excluded.** Not mentioned in the ticket. These
   are release-plz output; gating generated files means a bot-authored release PR
   can go red for Markdown no human wrote.

## Decisions

| Decision | Choice | Rationale |
| --- | --- | --- |
| `MD060` style | `compact` | Matches the idiom the repo already writes; all 77 findings clear via `--fix`. `aligned` yields 912 and forces re-padding on every future table edit. Note `compact` is **not** the default — the default is `any`, which still yields 66. |
| `docs/runbooks/` | included | Highest-stakes Markdown in the repo: it is executed from. |
| Gate strength | required immediately | See below. |
| Job location | `ci.yml` | See below. |
| Toolchain | committed lockfile, not the action | See below. |

### Why not signal-first

The ticket proposes signal-only first, citing `temporal-it`. That precedent does not
transfer: `temporal-it` is signal-only because it is *flaky*, and the phase exists to
**measure a flake rate**. A linter has none, so the phase would measure nothing.

This argument is only sound if the **job** is deterministic, not merely the tool.
The design below therefore removes every non-determinism source it would otherwise
introduce — an unpinned `npx` fetch and an action whose minor bumps change the
ruleset. Without those removals the argument fails and the gate would have to be
staged.

## Design

### Gated file set

`.markdownlint-cli2.jsonc` at the repo root owns the file set, so a bare
`npx markdownlint-cli2` locally lints exactly what CI lints:

```jsonc
{
  "gitignore": true,
  "globs": [
    "**/*.md",
    "!docs/superpowers/**",
    "!crates/*/CHANGELOG.md",
    "!**/target/**",
    "!**/node_modules/**",
    "!**/.claude/**",
    "!**/.superpowers/**"
  ]
}
```

A **denylist**, not the ticket's enumerated allowlist. A new crate README or book
page is then gated automatically; an allowlist silently misses new surfaces — which
is how the book drifted to 13 stub pages before anyone noticed. It also means
`docker/forkd/README.md`, a surface the ticket never mentions, is gated anyway.

**`"gitignore": true` is load-bearing, and so are the explicit negations.** Without
it, a bare local run in the maintainer's own checkout lints **121 files / 616
issues** instead of 51/110 — untracked `.superpowers/` SDD reports and other ignored
trees leak in, and the documented local gate becomes unusable, destroying the
"local == CI" property the config exists to provide. The negations are kept
alongside it, unanchored (`!**/target/**`, not `!target/**`), because:

- Root-anchored negations do not exclude nested copies. `target/package/` (produced
  by `cargo package` and by release-plz verification) contains full crate copies
  including `README.md`.
- `"gitignore": true` delegates to globby, which honours only **committed**
  `**/.gitignore` files — it never reads `.git/info/exclude`, and it reads a
  user-global gitignore only under an opt-in option this repo's `.markdownlint-cli2.jsonc`
  does not set. `.superpowers/` is excluded on this machine only via
  `.git/info/exclude`, which is **machine-local and uncommitted**, so
  `"gitignore": true` alone would not exclude it — and would behave differently on a
  contributor's machine whose exclusion, if any, lives somewhere else again. The
  explicit negation is what makes the file set reproducible across machines.

Measured with this config: **51 files, 110 findings, 16 files dirty** — identical in
the main checkout and in a clean worktree.

### Rule policy

```jsonc
"config": { "default": true, "MD013": false, "MD060": { "style": "compact" } }
```

Exactly **one** rule is disabled repo-wide. `MD013` is 818 of 928 raw findings and
line length does not affect rendering. Everything else in the default set stays on.
No rule is disabled repo-wide to dodge a finding; the two rules that genuinely do
not apply to one file each get a file-scoped disable.

### Toolchain — committed lockfile, not `markdownlint-cli2-action`

The obvious choice, `DavidAnson/markdownlint-cli2-action` (v24.2.0, which bundles
markdownlint-cli2 0.23.2), was **rejected**. Three reasons, in order of weight:

1. **The action version *is* the linter version.** markdownlint ships new rules in
   *minor* releases (`MD056`, `MD058`, `MD059`, `MD060` all arrived that way), and
   under `"default": true` a new rule is live the moment it lands. Dependabot's
   `github-actions` group takes patch **and minor**, so a routine grouped
   `chore(deps)` PR could redden a *required* gate for reasons nobody chose — on
   `main` as well as on the PR. The `branch-names` ruleset blocks humans pushing to
   `dependabot/**`, so such a PR cannot be fixed in place; it needs a `feature/**`
   takeover.
2. **The self-test needs the same binary the gate uses.** The action runs
   `dist/index.mjs` and puts no `markdownlint-cli2` on `PATH`, so a self-test step
   would have to `npx` its own copy — resolving *latest*, and thereby certifying the
   config against a linter the gate does not run.
3. **Version alignment with CodeRabbit is not what the action buys.** See below.

Instead: a committed `package.json` + `package-lock.json` pinning
`markdownlint-cli2` to **0.23.2**, installed with `npm ci`. The lockfile carries
integrity hashes, so this is a *checksummed* pin — stronger than either the action
or a bare `npx markdownlint-cli2@0.23.2`. Contributors, the gate, and the self-test
all run one identical binary.

`npm` is **not** in `.github/dependabot.yml` (cargo + github-actions only), so the
linter version becomes a hand-bumped pin alongside `PROTOC_VERSION`,
`TEMPORAL_CLI_VERSION`, and `NIGHTLY_TOOLCHAIN`. That is the intended outcome, and
it must be recorded in CLAUDE.md's list of untracked pins.

`node_modules/` must be added to `.gitignore` — it is currently **not** ignored.

### CI job

A `markdown-lint` job in **`.github/workflows/ci.yml`**, making it nine jobs.

Not `docs.yml`, despite that being the Markdown/docs workflow. `docs.yml`'s
`book-deploy` job only `needs: book-build`, so a `markdown-lint` failure on a push
to `main` would redden the `docs` **workflow run** while the Pages deploy still
succeeded — making "the docs workflow is red" ambiguous between "the site failed to
publish" and "a heading was wrong". That is the same signal-legibility problem
CLAUDE.md's signal-only-vs-required reasoning exists to prevent. `ci.yml` is where
every other required non-book gate lives and its run conclusion has one meaning.

```yaml
markdown-lint:
  runs-on: ubuntu-latest
  timeout-minutes: 10
  steps:
    # actions/checkout v7.0.1
    - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
      with:
        persist-credentials: false
    # actions/setup-node v7.0.0
    - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020
      with:
        node-version: '24'
        cache: npm
    - run: npm ci
    - name: Lint Markdown
      run: npx markdownlint-cli2
    - name: Verify the lint config is actually in force
      run: bash scripts/check-markdownlint-config.sh
```

**No path filter**, at job or workflow level. A path-filtered *required* check never
reports on a PR that touches no Markdown and blocks it forever — the trap
`sessions-it` avoids with step-level rather than job-level guards. The job takes
seconds; filtering buys nothing and risks everything.

**`npx markdownlint-cli2` takes no glob arguments.** With no command-line globs the
config's `globs` property is the sole source of truth, and CI and local are
identical by construction rather than by a union that happens to coincide.

`timeout-minutes: 10` bounds a hung `npm ci` — a required check that sits for six
hours is worse than one that fails.

### Rollout — the ruleset edit is inert on its own

Adding `{ "context": "markdown-lint" }` to
`.github/rulesets/main-protection-checks.json` **does not make the check required.**
Per CONTRIBUTING.md ("Repo configuration"), that JSON is applied by
`scripts/apply-repo-config.sh`, and there is no drift-check job — divergence is
found only when a maintainer next runs the script. Getting the order wrong produces
either a documented-but-unenforced gate, or a gate enforced while `main` has no such
job (blocking every PR with an unreportable context).

Ordered rollout:

1. Merge this PR to `main`. The job now exists on `main` and has run on this PR.
2. Maintainer runs `bash scripts/apply-repo-config.sh`.
3. Verify enforcement, not listing:
   `gh api repos/SMK1085/paigasus-helikon/rulesets --jq '...'` shows `markdown-lint`
   among the required contexts.

Rollback: remove the context from the JSON, re-run the script, then remove the job.

**PR #218** is the only open PR and will need the new context to report before it
can merge. The mechanism is *not* staleness — `strict_required_status_checks_policy`
is `false`, so being behind `main` is fine. It is that no `pull_request` event fires
when `main` moves. `ci.yml` uses a bare `pull_request:` trigger, so the default
types apply: an empty commit, or simply closing and reopening the PR (`reopened` is
active), re-triggers it. A rebase is not required.

### The fix set

`markdownlint-cli2 --fix` resolves 98 of 110 findings. The diff is **not** "entirely
whitespace" — two auto-fixes are content edits, and one of them is **wrong**:

| Auto-fix | File | Verdict |
| --- | --- | --- |
| `MD034` bare-URL → `<dev@paigasus.com>` | `CODE_OF_CONDUCT.md` | **Accept.** Renders as a mailto link. This is a filled-in Contributor Covenant placeholder, not verbatim upstream text. |
| `MD038` deletes the space in `` `Bearer ` `` | `crates/paigasus-helikon-providers-litellm/README.md:85` | **Reject.** The sentence is *about* a malformed `Bearer ` with a trailing space; `--fix` deletes the space and inverts the meaning. |

**Procedure:** run `--fix`, then revert the `MD038` hunk and hand-fix by rewording
so the prose does not depend on a trailing space inside a code span (e.g. "…rather
than sent as a `Bearer` header with an empty credential"), then apply the remaining
hand-fixes.

The 12 residual findings:

| Finding | File | Disposition |
| --- | --- | --- |
| 4× `MD025` | `docs/book/src/SUMMARY.md` | scoped disable — mdBook *requires* `# Part` separators |
| 1× `MD041` | `.github/PULL_REQUEST_TEMPLATE.md` | scoped disable — PR templates have no H1 by convention |
| 4× `MD036` | `docs/runbooks/forkd-live-validation.md` | fix — see below |
| 1× `MD040` | `docs/runbooks/forkd-live-validation.md:323` | fix — add a `text` language |
| 1× `MD028` | `docs/runbooks/agentcore-image-check.md:24` | fix — see below |
| 1× `MD001` | `crates/paigasus-helikon-tools/README.md:9` | fix — `###` → `##` (an h1→h3 skip) |

**`MD036` — convert to `###`, and convert all eight.** `####` would be a two-level
skip below `## Troubleshooting` and introduces four fresh `MD001` violations. Also,
the section contains **eight** bold pseudo-headings but only four trip `MD036`;
converting only those four would leave the section half-converted. Convert all eight
to `###`. Verified: the file is then clean but for the known `MD040` fence.

**Standing rule for the implementer:** if a heading-level fix produces a new
`MD001`, adjust the level — never disable `MD001`.

**`MD028`** — `docs/runbooks/agentcore-image-check.md` lines 3–23 and 25–40 are two
*deliberately separate* blockquotes (a 2026-07-06 local measurement and a 2026-08-07
CI measurement). Do not merge them: insert an `<!-- -->` separator between them,
which preserves the authorial intent and renders as two blockquotes.

Scoped disables use an inline `<!-- markdownlint-disable-file MD0xx -->` at the top
of the one file where the rule is wrong, never a repo-wide `false`.

**`SUMMARY.md` — verified, no fallback needed.** With the comment prepended,
`mdbook build docs/book` succeeds and the generated `book/html/toc.js` is
**byte-identical** to baseline. The previously-considered
`docs/book/src/.markdownlint.jsonc` fallback is dropped: it would disable `MD025`
for all 20 book pages, violating the file-scoped principle on the most-read
published surface.

### Line endings

Add `*.md text eol=lf` to `.gitattributes`, which currently pins only the Anthropic
SSE fixtures. CI is ubuntu-only, but CONTRIBUTING will instruct contributors to run
`--fix` locally, and a Windows `--fix` rewrites files with CRLF — whole-file diffs
and a local/CI mismatch that looks like a linter bug. This is the same failure the
repo already learned once (CLAUDE.md, "Fixture line endings").

### Config self-test

`scripts/check-markdownlint-config.sh`, run as a step in the `markdown-lint` job,
guards silent-success modes found both during the design of this ticket and during
a later whole-branch review (SMA-581 fix wave):

1. **An invalid rule-option value silently disables the rule.** A typo'd
   `"MD060": { "style": "consistent" }` — not a documented value; the set is
   `aligned` / `any` / `compact` / `tight` — yields `Summary: 0 issues in 0 files`
   with no error. The gate reports green while enforcing nothing.
2. **The gated file set can silently collapse, or narrow to a subtree.** A
   `--no-globs` flag, an edit to `globs`, or a lost `gitignore` setting can affect
   the whole set or just one area (e.g. `"!crates/**"` ungates every crate README
   while leaving the book tree, and the lint step, green). A single probe under
   `docs/book/**` cannot distinguish the two, so the script probes multiple gated
   areas independently.
3. **`"default": false` silently disables every rule except the ones explicitly
   configured** (`MD013`, `MD060` here). No file's *membership* in the linted set
   changes, so a membership-only check (does the probe path appear at all?) cannot
   catch it — the probe still appears via the explicitly-configured `MD060` line.
4. **A bare-substring membership check is unsound once there is more than one
   probe.** Found in a second review round on the fix wave above: the repo-root
   probe's filename (`__mdlint_probe.md`) was a plain substring of every other
   probe's path (e.g. `docs/book/src/__mdlint_probe.md` contains it), so a
   `[[ "$output" == *"$probe"* ]]`-style check for the root probe was silently
   satisfied by the BOOK or CRATE probe's own output lines. The root leg never
   proved anything — excluding just the root file from `globs` left the script
   printing "ok: all gated probes ... are linted" and exiting `0` while genuinely
   no longer linting the root.

Mechanism — markdownlint-cli2 has no `--list-files`, and per-file lines appear only
for files *with* findings, so the script uses **probe files that deliberately
violate three rules at once**: `MD012` (multiple consecutive blank lines) and
`MD040` (fenced code block with no language tag), both default-on and otherwise
untouched by this repo's config; and `MD060` in `"compact"` style (an unpadded
table row), the one rule this repo explicitly configures.

Every probe path is matched **anchored**, not by bare substring: output lines are
selected by `path + ":"` at the START of the line (markdownlint-cli2 emits
`path:line[:col] error MDxxx/rule description`), via
`awk -v p="${path}:" 'index($0, p) == 1'`. This is what actually fixes failure mode
4 above, and it is applied to every leg (all three gated probes and the excluded
probe), not just the root one, so a future rename or an added probe cannot
reintroduce the same class of bug. The repo-root probe is additionally renamed to
`__mdlint_probe_root.md` — not a substring of any sibling path — as a redundant,
belt-and-braces defense that would catch the collision even if the anchoring were
ever weakened.

```text
set -euo pipefail; trap cleanup EXIT

0. Assert the banner line reports v0.23.2 — the self-test must certify the
   same binary the gate runs.
1. Write the same MD012+MD040+MD060 violation into three gated probes:
   docs/book/src/__mdlint_probe.md (deep book path), a crate README location
   (crates/paigasus-helikon-core/__mdlint_probe.md), and the repo root
   (__mdlint_probe_root.md — collision-proof: not a substring of any sibling
   probe path).
   For each: extract only the output lines anchored to that exact path (see
   "Mechanism" above); assert that set is non-empty (the path was linted at
   all); assert MD012 fires in it (a rule ID other than MD060, so this check
   cannot be satisfied by an MD060 line alone); assert MD040 fires in it
   (proves "default": true is in force, independent of file membership);
   assert MD060 fires in it (proves the configured style value is honoured).
   -> proves each gated area is linted, independently of the others (both in
      the sense of separate globs AND separate, non-colliding output-line
      matching), under both default-on and explicitly-configured rules.
2. Write the same violation into docs/superpowers/__mdlint_probe.md.
   Assert its anchored output-line set is empty.
   -> proves the exclusion is in force.
3. Remove all four probes.
```

Assertions are **positive markers** (grep for an expected rule-ID string), never
absence-of-findings — per the repo's own lesson in `audit.yml`'s `scheduled-audit`
commentary. Set membership rather than a file **count**, because a count is brittle
against ordinary additions and trains people to update it reflexively. The
membership check for each gated probe is itself tied to a named rule (`MD012`) that
is not `MD060`, so an `MD060`-only match can never stand in for genuine membership
— and, since SMA-581's second fix wave, tied to output lines anchored to that
probe's exact path, so no probe's match can be satisfied by a sibling probe's
output either.

Not guarded: an unknown *rule name* (`"MD6O": false`) is also silently ignored, but
that is fail-**safe** — the real rule stays enabled and a violation still fails the
build. Verified; no third assertion needed.

Same genre as `scripts/check-advisory-ignore-sync.sh` and
`.github/actions/setup-protoc/selftest.sh`.

### CodeRabbit interaction

CodeRabbit runs markdownlint-cli2 0.23.2 today, and `.coderabbit.yaml` configures
only the Linear integration — no `tools.markdownlint` block — so CodeRabbit
auto-discovers the repo-root config.

The pin is **not** justified by version parity with CodeRabbit. That would be a
claim about a third party's internal version that this repo cannot observe or hold
stable, and it will silently become false. What actually aligns the two is **config
discovery**: once `.markdownlint-cli2.jsonc` exists, CodeRabbit honours `MD013:
false` and `MD060: compact`, so its advisory comments stop diverging from CI. That
is a real benefit and it does not depend on versions matching.

**Post-merge check:** on the first PR after merge, confirm CodeRabbit's markdown
comments are still scoped to the diff. If the config's `**/*.md` glob causes it to
report across all 51 files, add an explicit `tools.markdownlint` block to
`.coderabbit.yaml`.

## Consequences

- **Five crates get a patch bump for mechanical fixes** — `providers-litellm`,
  `runtime-actix`, `runtime-axum`, `runtime-temporal` (from `--fix`) and `tools`
  (the `MD001` hand-fix). `README.md` is a packaged crate file and release-plz
  attributes bumps by path regardless of commit type. This is the **pure-auto**
  release path: release-plz performs the bumps itself, so `dependencies_update`
  cascades to the facade correctly and **no** manual `core`/facade bump is needed —
  CLAUDE.md's same-PR-manual-bump caveat does not apply.
  *Splitting into two PRs was considered and rejected*: a gate-only PR would merge
  while the READMEs are still dirty, putting `main` red.
- **PR #218** needs a re-trigger (empty commit, or close/reopen) — not a rebase.
- **Local reproduction requires Node.** `npm ci && npx markdownlint-cli2` joins
  CONTRIBUTING's local-gates list, which currently assumes only a Rust toolchain.
- **The facade README is `include_str!`'d into rustdoc**, so `MD040` pushes authors
  toward tagging fences there. Note in CONTRIBUTING: in
  `crates/paigasus-helikon/README.md` use ` ```text ` or ` ```ignore `, never a bare
  ` ```rust ` for a snippet needing network or keys. (Verified: `--fix` does not
  touch the facade README, so no action is needed in this PR.)

## Out of scope

- Cleaning up `docs/superpowers/` — 274 findings in internal throwaway artifacts.
- Prose linting (Vale, `write-good`).
- Auto-fixing in CI. The job reports; humans fix.

## Verification

1. `npm ci && npx markdownlint-cli2` at the repo root exits 0 with `0 issues`, and
   reports the identical file count in the main checkout and in a clean worktree.
2. `bash scripts/check-markdownlint-config.sh` exits 0; and exits non-zero under
   each mutation — `MD060.style` set to an invalid value, and the gated set narrowed
   to the repo root.
3. `mdbook build docs/book` succeeds and `book/html/toc.js` is byte-identical to
   its pre-change output.
4. `cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs`
   passes. Reasoned about, not merely run: the only *gated* file containing
   `<!-- tracing-components:… -->` is `docs/book/src/concepts/observability-evaluation.md`,
   and it is **not** among the 12 files `--fix` touches — so no `MD058`
   blanks-around-tables edit can land next to a marker. The other four marker files
   are in the excluded `docs/superpowers/`.
5. The `markdown-lint` context reports on this PR, and after
   `scripts/apply-repo-config.sh` runs, the GitHub API shows it **enforced** as a
   required context (not merely listed in the JSON).
6. `git diff --stat` on the crate READMEs shows only the enumerated mechanical
   changes; the `MD038` hunk is a reword, not the `--fix` deletion.

## Docs to update in the same PR

- `CONTRIBUTING.md` — required-contexts table; local-gates list (`npm ci && npx
  markdownlint-cli2`); the facade-README fence note.
- `CLAUDE.md` — the CI section (`ci.yml` becomes nine jobs); add `markdownlint-cli2`
  to the list of hand-bumped, Dependabot-untracked pins.
- `.gitignore` — add `node_modules/`.
- `.gitattributes` — add `*.md text eol=lf`.

No documentation *prose* is rewritten. The only README edits are the mechanical
fixes enumerated above; the mdBook content pages are a conscious skip, since this is
CI plumbing.
