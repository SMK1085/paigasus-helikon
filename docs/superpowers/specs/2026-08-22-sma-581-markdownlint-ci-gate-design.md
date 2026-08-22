# SMA-581 — markdownlint-cli2 as a required CI gate

**Status:** approved (2026-08-22)
**Ticket:** [SMA-581](https://linear.app/smaschek/issue/SMA-581/adopt-markdownlint-cli2-as-a-ci-gate-for-the-published-markdown)
**Branch:** `feature/sma-581-adopt-markdownlint-cli2-as-a-ci-gate-for-the-published`

## Problem

Markdown is entirely ungated in this repo. `grep -rl markdownlint .github/` returns
nothing, yet several Markdown surfaces are published artifacts:

- `docs/book/` — the public mdBook, deployed from `main` by `docs.yml`.
- `crates/*/README.md` — each crate's crates.io and docs.rs landing page.
- The root `README.md`, `CONTRIBUTING.md`, `BENCHMARKS.md`, `SECURITY.md`.

`mdbook build` with `[output.linkcheck] warning-policy = "error"` catches broken
links and nothing else. A missing fence language, a skipped heading level, or a
malformed list renders badly on crates.io and nobody finds out.

The trigger was PR #216, where CodeRabbit — not CI — caught an `MD040` violation.
CodeRabbit runs `markdownlint-cli2` as part of its own advisory tooling, so the
repo has been getting Markdown review by accident rather than by policy.

## Correcting the ticket's premise

The ticket claims *"gating the published surfaces costs one fence fix, not a
275-item cleanup."* That is **false**. It counted only `MD040`.

Measured with `markdownlint-cli2@0.23.2` against the gated set defined below:

| Rule | Findings | Note |
| --- | --- | --- |
| `MD013` line-length | 818 | anticipated by the ticket; disabled as policy |
| `MD060` table-column-style | 77 | **not anticipated**; parameterized, fully auto-fixable |
| `MD032` blanks-around-lists | 10 | auto-fixable |
| `MD031` blanks-around-fences | 4 | auto-fixable |
| `MD049` emphasis-style | 4 | auto-fixable |
| `MD025` single-title | 4 | `docs/book/src/SUMMARY.md`; rule does not apply |
| `MD036` no-emphasis-as-heading | 4 | genuine content fix |
| `MD001`/`MD012`/`MD028`/`MD034`/`MD038`/`MD040`/`MD041` | 7 | one each |

Two further corrections to the ticket:

1. **`MD033` does not need relaxing.** The ticket asserts it "would fire on the
   mdBook's `<!-- tracing-components:start -->` marker comments". It does not —
   markdownlint exempts HTML comments from `MD033`. Verified: `MD033` reports zero
   findings across the gated set. The rule stays enabled.
2. **`crates/*/CHANGELOG.md` must be excluded.** Not mentioned in the ticket. These
   are release-plz output; gating generated files means a bot-authored release PR
   can go red for Markdown no human wrote.

## Decisions

Three judgment calls were settled by the author before design:

| Decision | Choice | Rationale |
| --- | --- | --- |
| `MD060` style | `compact` | Matches the idiom the repo already writes. 77 findings, **all** cleared by `--fix`. The alternative `aligned` yields 912 findings and forces re-padding every row on every future table edit. |
| `docs/runbooks/` | **included** | These are the highest-stakes Markdown in the repo — they are executed from, not just read. A mangled command block costs real debugging time. |
| Gate strength | **required immediately** | Not signal-first. |

### Why not signal-first

The ticket proposes landing this as a signal-only job, citing the `temporal-it`
precedent. That precedent does not transfer. `temporal-it` is signal-only because
it is *flaky* — it aborts a real worker against wall-clock activity timeouts — and
the signal-only phase exists to **measure a flake rate**. markdownlint is
deterministic. There is no flake rate to measure, so the phase would measure
nothing and simply delay the gate.

The one genuine argument for staging — that a new required context blocks open PRs
whose head predates the workflow — is real but small: PR #218 is the only open PR,
and it need only rebase. That cost is paid once and is cheaper than a promotion
ceremony that gathers no evidence.

## Design

### Gated file set — denylist, not allowlist

`.markdownlint-cli2.jsonc` at the repo root owns the file set, so a bare
`npx markdownlint-cli2` locally lints exactly what CI lints. No globs are passed on
the CI command line; a second source of truth would be free to drift.

```jsonc
{
  "globs": [
    "**/*.md",
    "!docs/superpowers/**",
    "!crates/*/CHANGELOG.md",
    "!target/**",
    "!node_modules/**"
  ]
}
```

A **denylist** rather than the ticket's enumerated allowlist (`docs/book/`,
`crates/*/README.md`, root, `.github/`). A new crate README or a new book page is
then gated automatically. An allowlist silently misses new surfaces — which is how
the book drifted to 13 stub pages through all of Stage 1 before anyone noticed.

Exclusions:

- `docs/superpowers/**` — 161 internal per-ticket design artifacts, never published
  and never read by a consumer. They hold 274 of the repo's 275 bare fences. This is
  the ticket's own call and is correct.
- `crates/*/CHANGELOG.md` — release-plz generated (see above).
- `target/**`, `node_modules/**` — build output; belt-and-braces, neither is tracked.

Measured scope: **51 files linted, 110 findings across 16 files.**

### Rule policy

```jsonc
{
  "config": {
    "default": true,
    "MD013": false,
    "MD060": { "style": "compact" }
  }
}
```

Exactly **one** rule is disabled repo-wide. `MD013` (line length) accounts for 818
of the 928 raw findings, and line length is a source-formatting preference with no
effect on how a page renders. Everything else in the default set stays on.

`MD060` is parameterized, not disabled: `compact` is the `| --- | --- |`
single-space form the repo already predominantly writes.

No rule is disabled repo-wide in order to dodge a finding. The two rules that
genuinely do not apply to one file each get a **file-scoped** disable (below).

### CI job

A `markdown-lint` job in **`.github/workflows/docs.yml`**, alongside the existing
required `book-build`. `docs.yml` is the Markdown/docs workflow and already carries
a required check, so the rule that keeps signal-only jobs out of required-bearing
workflows does not apply — this job is required too.

```yaml
markdown-lint:
  runs-on: ubuntu-latest
  steps:
    # actions/checkout v7.0.1
    - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
      with:
        persist-credentials: false
    # markdownlint-cli2-action v24.2.0 (bundles markdownlint-cli2 0.23.2)
    - uses: DavidAnson/markdownlint-cli2-action@21c1be1b93ad9ed58fa840aacc3f279cde2a72ff
```

**No path filter**, at job or workflow level. A path-filtered *required* check never
reports on a PR that touches no Markdown, and blocks it forever — the trap
`sessions-it` avoids with step-level rather than job-level guards. The job takes
seconds; filtering buys nothing and risks everything.

**No `with: globs:` input — but not because the action defaults to the config.**
The action's `globs` input defaults to `*.{md,markdown}`, i.e. the repository root
only. Omitting it is nevertheless correct here: markdownlint-cli2 *unions* the
command-line globs with the `globs` property in `.markdownlint-cli2.jsonc`, and the
config's `**/*.md` plus its negations then determine the set. Verified — a bare run,
a run with the action's default `*.{md,markdown}`, and a run with an explicit
`**/*.md` all lint the identical 51 files.

This is correct but **not legible from the workflow file**, and it is one
`--no-globs` away from silently collapsing to six root files while still reporting
green. The self-test below asserts against exactly that.

**Version pinning.** The action bundles `markdownlint-cli2` **0.23.2** — byte-identical
to the version CodeRabbit runs, satisfying the ticket's alignment requirement with
no second pin to hand-maintain. A git-SHA pin also gives stronger integrity than
`npx markdownlint-cli2@0.23.2`, which has no checksum at all. The coupling is worth
recording: **the action version is the linter version**, so an action major bump
silently changes the ruleset. Dependabot's `github-actions` group is patch+minor
only, so a v24→v25 bump never arrives unattended — the same property that left
`actions/checkout` on v6 until SMA-486. Bumping it is a human act.

`{ "context": "markdown-lint" }` is added to
`.github/rulesets/main-protection-checks.json`.

### The fix set

`markdownlint-cli2 --fix` resolves **98 of 110** findings: a **+33/−20 diff across
12 files**, entirely whitespace and table-pipe padding. The remaining 12 are
dispositioned individually:

| Finding | File | Disposition |
| --- | --- | --- |
| 4× `MD025` | `docs/book/src/SUMMARY.md` | scoped disable — mdBook *requires* `# Part` separators |
| 1× `MD041` | `.github/PULL_REQUEST_TEMPLATE.md` | scoped disable — PR templates have no H1 by convention |
| 4× `MD036` | `docs/runbooks/forkd-live-validation.md` | fix — `**Bold**` pseudo-headings become `####` |
| 1× `MD040` | `docs/runbooks/forkd-live-validation.md:323` | fix — the fence the ticket is named after |
| 1× `MD028` | `docs/runbooks/agentcore-image-check.md:24` | fix — blank line inside a blockquote |
| 1× `MD001` | `crates/paigasus-helikon-tools/README.md:9` | fix — h1→h3 skip on a live crates.io page |

Scoped disables use an inline `<!-- markdownlint-disable-file MD0xx -->` comment at
the top of the one file where the rule is wrong, never a repo-wide `false`.

**Open risk — `SUMMARY.md`.** mdBook's SUMMARY parser is structural, and it is *not
yet confirmed* that it tolerates a leading HTML comment. Implementation must run
`mdbook build docs/book` and confirm the book still builds and the sidebar is
unchanged. If mdBook rejects it, fall back to a scoped `docs/book/src/.markdownlint.jsonc`
containing `{ "MD025": false }`. Do **not** fall back to disabling `MD025` repo-wide.

### Config self-test

This tool has **two** failure modes that report success, both hit during the design
of this ticket. `scripts/check-markdownlint-config.sh` asserts against both, and
runs as a step in the `markdown-lint` job.

1. **An invalid rule-option value silently disables the rule.** A typo'd
   `"MD060": { "style": "consistent" }` — not a documented value; the set is
   `aligned` / `any` / `compact` / `tight` — produces `Summary: 0 issues in 0 files`
   with no error or warning. The gate reports green while enforcing nothing.
   *Assertion:* lint a fixture containing a known `MD060`-compact violation and fail
   if it is **not** reported.

2. **The gated file set can silently collapse to the repo root.** The linted set
   depends on the config's `globs` unioning with the command line; a `--no-globs`
   flag, a future action default change, or an edit to the `globs` property would
   narrow it to six root files, and the job would still pass.
   *Assertions:* a deep path (`docs/book/src/SUMMARY.md`) **is** in the linted set,
   and a `docs/superpowers/` path **is not**. The first catches silent narrowing;
   the second catches a lost exclusion.

Deliberately assertions about set membership rather than a file **count** — a count
is brittle against ordinary additions and would train people to update it reflexively.

Same genre as `scripts/check-advisory-ignore-sync.sh` and
`.github/actions/setup-protoc/selftest.sh` — a cheap assertion that a
silently-degrading configuration has not degraded.

## Consequences

- **Four crates get a patch bump for whitespace.** `--fix` touches the
  `providers-litellm`, `runtime-actix`, `runtime-axum`, and `runtime-temporal`
  READMEs. `README.md` is a packaged crate file and release-plz attributes bumps by
  path regardless of commit type, so expect a release PR bumping those four plus the
  facade cascade. Harmless, but it should not be a surprise.
- **PR #218 must rebase before it can merge.** It is the only open PR; its head
  predates the workflow, so the new required context can never report on it.
- **Local reproduction requires Node.** `npx markdownlint-cli2` is added to
  CONTRIBUTING.md's local-gates list, which currently assumes only a Rust toolchain.

## Out of scope

- Cleaning up `docs/superpowers/` — 274 findings in internal throwaway artifacts.
- Prose linting (Vale, `write-good`) — a much larger style surface, a separate call.
- Auto-fixing in CI (`--fix` then fail-if-dirty). The job reports; humans fix.

## Verification

1. `npx markdownlint-cli2` at the repo root exits 0 with `0 issues`.
2. `bash scripts/check-markdownlint-config.sh` exits 0; and exits non-zero under
   each of its two mutations — `MD060.style` set to an invalid value, and the
   gated set narrowed to the repo root.
3. `mdbook build docs/book` still succeeds and the rendered sidebar is unchanged.
4. `cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs`
   still passes — `tests/workspace-lints/tests/tracing_target_docs.rs` parses the
   `<!-- tracing-components:start -->` markers, and no `--fix` edit may disturb them.
5. The `markdown-lint` context reports on the PR and is listed in
   `.github/rulesets/main-protection-checks.json`.

## Docs to update in the same PR

- `CONTRIBUTING.md` — required-contexts table, and the local-gates command list.
- `CLAUDE.md` — the CI section.

No mdBook or crate-README *content* changes. This is CI plumbing; per the repo rule
that is a conscious skip, not a silent one.
