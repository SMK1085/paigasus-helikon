# SMA-574 — Remove the hand-maintained version column from the mdBook crate roster

- **Issue**: [SMA-574](https://linear.app/smaschek/issue/SMA-574/the-mdbook-crate-roster-version-column-is-hand-maintained-and-19-of-20)
- **Date**: 2026-09-04
- **Status**: revised after adversarial challenge; pending approval
- **Scope**: documentation only — no Rust source, no CI workflow, no `Cargo.toml` touched

## Problem

`docs/book/src/reference/crates.md` carries a per-crate **Version** column that nothing keeps in
sync with the workspace. It is mirrored by hand, release-plz does not touch it, and no CI gate
compares it against the `Cargo.toml`s. It therefore drifts a little further with every release and
silently misinforms readers of the published book.

Measured against this branch's base (`47b955b`), **19 of the 21 rows are wrong**. Two are right:
`paigasus-helikon-macros` (`0.2.4`) by accident, because it has not been released since the numbers
were last mirrored, and `paigasus-helikon-sessions-testkit` (`0.0.0`) permanently, because it is
`publish = false` and never gets a version.

| Crate | Book says | Actually | Behind by |
| --- | --- | --- | --- |
| `paigasus-helikon` (facade) | `0.5.5` | `0.5.17` | 12 |
| `paigasus-helikon-cli` | `0.1.13` | `0.1.22` | 9 |
| `paigasus-helikon-providers-openai` | `0.2.21` | `0.2.26` | 5 |
| `paigasus-helikon-providers-litellm` | `0.1.0` | `0.1.5` | 5 |
| `paigasus-helikon-runtime-agentcore` | `0.2.4` | `0.2.9` | 5 |
| `paigasus-helikon-evals` | `0.1.6` | `0.1.11` | 5 |
| `paigasus-helikon-runtime-temporal` | `0.4.1` | `0.4.5` | 4 |
| `paigasus-helikon-runtime-actix` | `0.2.2` | `0.2.6` | 4 |
| `paigasus-helikon-runtime-axum` | `0.2.2` | `0.2.6` | 4 |
| `paigasus-helikon-providers-anthropic` | `0.1.21` | `0.1.24` | 3 |
| `paigasus-helikon-runtime-tokio` | `0.1.20` | `0.1.23` | 3 |
| `paigasus-helikon-tools` | `0.2.13` | `0.2.16` | 3 |
| `paigasus-helikon-core` | `0.5.17` | `0.5.19` | 2 |
| `paigasus-helikon-providers-bedrock` | `0.1.5` | `0.1.7` | 2 |
| `paigasus-helikon-providers-gemini` | `0.1.5` | `0.1.7` | 2 |
| `paigasus-helikon-mcp` | `0.1.19` | `0.1.21` | 2 |
| `paigasus-helikon-sessions-sqlite` | `0.1.23` | `0.1.25` | 2 |
| `paigasus-helikon-sessions-postgres` | `0.1.4` | `0.1.6` | 2 |
| `paigasus-helikon-sessions-redis` | `0.1.4` | `0.1.6` | 2 |
| `paigasus-helikon-macros` | `0.2.4` | `0.2.4` | correct (coincidence) |
| `paigasus-helikon-sessions-testkit` | `0.0.0` | `0.0.0` | correct (never published) |

The drift is structural, not an oversight. A PR cannot know its own post-merge version — release-plz
assigns it after the merge — so every hand-edit is a guess that is stale again by the next release.

The same defect appears twice more in the book, in a worse form. `getting-started/quickstart.md:9`
tells readers to depend on `paigasus-helikon = { version = "0.3" }` and
`getting-started/workspace-layout.md:92` on `{ version = "0.4" }`, while the facade is at `0.5.17`.
Those are not merely stale — they are wrong install instructions that a reader will copy.

Following that trail with an actual build revealed that the quickstart does not work at all
(evidence in [Appendix A](#appendix-a-quickstart-build-evidence)):

- Its dependency block omits `serde_json`, which the `#[tool]` expansion emits as absolute
  `::serde_json::` paths into the consumer's crate and the facade does not re-export. A reader
  copying the page gets `E0433: cannot find serde_json in the crate root`, twice.
- Its step-4 run command, `cargo run --features openai,macros`, names features of the *reader's own*
  package, which do not exist. Cargo answers `error: the package 'qs' does not contain these
  features: macros, openai`.

Both are the same failure the issue is about — copy-pasteable instructions the repo never verified —
in the same blocks this change is already rewriting.

## Decision

**Drop the version column** (the issue's option 1), and convert the drifting install snippets to
`cargo add` invocations that pin third-party companions but not Helikon crates.

Rationale:

- The column duplicates information the reader can get from an authoritative, always-current source
  that is *already linked in the same row*: each crate name links to its docs.rs page.
- A freshly-patched table looks maintained, which misleads more than a visibly stale one.
- It is self-maintaining. No new machinery, no new failure mode, nothing to remember at release time.
- `cargo add` is the pattern this repo already chose for the same problem elsewhere: the crate
  `README.md` install snippets "deliberately use drift-free `cargo add` (no hardcoded versions)"
  (CLAUDE.md), and root `README.md:26`, facade `README.md:10`, `-macros/README.md:13` and
  `concepts/axum-server.md:8` already use it. The book's getting-started pages never received it.

### The pinning rule

`cargo add <crate>` resolves the *latest* major. That is what we want for Helikon crates — it is
exactly the drift being removed — but it is wrong for third-party companions, because `schemars` is
**public API** of `paigasus-helikon-core`:

- `crates/paigasus-helikon-core/src/agent.rs:131` — `pub schema: schemars::Schema`
- `crates/paigasus-helikon-core/src/agent_builder.rs:482` and `src/runner.rs:250` — public
  `schemars::JsonSchema` bounds

The day `schemars` 2.0 ships, an unpinned `cargo add schemars` would give a reader two incompatible
`schemars` in the graph and a `expected trait schemars::JsonSchema, found a different
schemars::JsonSchema` error. That trades a stale-but-resolvable pin for a future hard break, on a
dependency that was never the problem.

**Rule, to be stated in the spec and honoured by every snippet: Helikon crates go unpinned; every
third-party companion keeps its major (`@1`).**

### Options rejected

**Generate the column at build time** (issue option 2). `docs.yml:28-30`'s `book-build` job installs
only prebuilt `mdbook@0.4.43` and `mdbook-linkcheck@0.7.7` via `taiki-e/install-action`. A
preprocessor would add a Rust build step and a new failure mode to a **required** status check, to
restore a column whose content is one click away on docs.rs. The cost is real and the benefit is not.

**Live crates.io badges** (not in the issue). Replacing each number with a
`img.shields.io/crates/v/<crate>.svg` badge would keep the at-a-glance view and stay current with no
build machinery — root `README.md:8` already uses that exact badge, and `book.toml:20`
`follow-web-links = false` means it adds no CI risk. Rejected as heavier than the value it returns:
21 external images in one table is visually noisy, degrades offline and in print, and makes the book
depend on a third-party image host for a fact docs.rs already carries.

**Gate the hand-maintained column** (issue option 3). Rejected in the issue and here: it would make
every release PR require a matching doc edit, and release-plz's bot PR would have to carry it.

## Changes

### 1. `docs/book/src/reference/crates.md`

- Drop the `Version` column from the crate table: the header, the delimiter row, and the trailing
  cell of all 21 data rows. The `Crate`, `Concern`, and `State` columns are unchanged, so no row
  loses its docs.rs link, its description, or its published/internal status.
- Replace the "Versions below are **current as of 2026-08-16** …" paragraph with a sentence that
  points at the authoritative sources instead of asserting numbers: docs.rs (linked per row) for the
  published version, each crate's `Cargo.toml` for the in-tree version.
- In the opening paragraph, change "This page is the version-bearing map" — the page no longer bears
  versions. It stays the ownership-and-dependency map.
- Leave the three `version = "0.0.0"` mentions in the prose about the non-published `tests/*`
  members (`crates.md:47-49`). Those are not drifting numbers; `0.0.0` is a permanent property of a
  `publish = false` crate and is the point being made.

### 2. Cross-references that promise version content

Removing the column orphans four sentences elsewhere that send readers to the roster *for versions*.
Each is corrected in this PR; leaving any of them re-creates the same silent-drift defect in a new
place.

| File:line | Current promise | Fix |
| --- | --- | --- |
| `README.md:29` | "…the full feature → crate map and **current published versions**." | drop the version clause |
| `README.md:52` | "…each crate's concern, published state, **and current version**." | drop the version clause |
| `docs/book/src/reference/api-docs.md:22` | "Crate versions move every release — **see Crate overview for the current numbers**." | point at docs.rs instead |
| `docs/book/src/concepts/mcp-integration.md:125` | "…the crate reference for **version** and feature details." | "…for ownership and feature details" |

The three per-crate READMEs that mention the roster (`crates/paigasus-helikon/README.md:87`,
`-cli/README.md:52`, `-evals/README.md:59`) only link it in a "see also" list without promising
version content. They are correctly left untouched.

**Adjacent rot in `api-docs.md`, fixed in the same pass** (approved as an explicit scope call, since
this PR already has to edit `:22`). The page is stale in two further ways, both the same
roster-drift defect:

- `:26` — "All **18** non-internal crates now publish to crates.io". It is **20**: 21 crates, minus
  `-sessions-testkit`. Root `README.md:33`, `workspace-layout.md:50` and `introduction.md:19`
  already say twenty-one/twenty correctly, so `api-docs.md` is the lone dissenter.
- `:7-20` — the "Published crates" list has **14 of 20** entries. Missing:
  `-providers-bedrock`, `-providers-gemini`, `-providers-litellm`, `-sessions-postgres`,
  `-sessions-redis`, `-runtime-actix`. All six are added, each with its docs.rs link and a one-line
  concern matching the wording already used for it in `crates.md`.

**CLAUDE.md compliance.** The rule "update the root `README.md` whenever the crate roster changes"
*is* triggered — `README.md:29` and `:52` are direct promises about roster content this PR removes.
This is a conscious, documented edit, not a silent skip.

### 3. `docs/book/src/getting-started/workspace-layout.md`

- `:8` cross-reference — "For the per-crate **version** and ownership table" → drop "version".
- `:119` Next-steps entry — "per-crate versions and ownership" → "per-crate ownership".
- Convert the two `[dependencies]` TOML blocks under **`## Picking your surface`** (heading at
  `:77`; blocks at `:82-85` and `:90-97`) to `cargo add`. That is `paigasus-helikon-core = "0.5"`
  (accurate today) and `paigasus-helikon = { version = "0.4", … }` (wrong today). Both are converted
  so one section does not mix two install idioms; add `serde_json@1` to the second, which has the
  same omission as the quickstart.

### 4. `docs/book/src/getting-started/quickstart.md`

- Convert the step-1 `[dependencies]` block (`:7-14`) to `cargo add`, dropping the `version = "0.3"`
  pin from the facade and keeping `@1` on `tokio`, `anyhow`, `serde`, and `schemars`.
- **Add `cargo add serde_json@1`.** Required by the `#[tool]` expansion
  (`crates/paigasus-helikon-macros/src/expand.rs` emits `::serde_json::` in 8 places; the facade's
  own `Cargo.toml:65-66` documents exactly this, and `crates/paigasus-helikon/src/lib.rs` does not
  re-export it). Verified: the page fails to compile without it and compiles with it.
- **Fix step 4 (`:159`)** from `cargo run --features openai,macros` to `cargo run`. The features are
  already on the dependency from step 1; naming them here refers to the reader's own package and
  errors. `:165`'s `cargo run -p paigasus-helikon --features … --example …` is correct as written —
  that one runs inside this workspace — and is left alone.
- Retitle `## 1. Add the dependency` → `## 1. Add the dependencies`, now that it adds six.
- Fenced-block language for every new shell block is **`bash`**, matching `quickstart.md:22,158` and
  the READMEs. (The book is inconsistent — `axum-server.md` uses `sh` — so this is stated rather
  than left to chance.)

## Non-goals

- **`docs/book/src/concepts/model-providers.md` is not touched.** It carries two *live* hardcoded
  Helikon pins — `:402` `paigasus-helikon = { version = "0.5", … }` and `:282`
  `paigasus-helikon-providers-gemini = { version = "0.1", … }` — plus four commented alternatives at
  `:404,406,408,410`. Both live pins are accurate today and neither is an install instruction a
  reader follows to a broken build; the block's job is to show *feature selection* across providers,
  which the commented-alternatives form conveys and `cargo add` would not. Deferred deliberately, and
  recorded here so the next editor sees it was considered. (An earlier draft of this spec described
  all five lines as commented out. That was wrong; `:402` is live.)
- No CI workflow change. `docs.yml` is untouched.
- No new script, preprocessor, or lint. Nothing is added that could redden `book-build`.
- No Rust source change, so no crate version bump, no CHANGELOG entry, and no facade bump.

## Verification

| Check | Command | Expectation |
| --- | --- | --- |
| Required `book-build` gate | `cd docs/book && mdbook build` | exit 0, linkcheck clean (`warning-policy = "error"`) |
| Table column count (MD056) | `awk -F'\|' '/^\| / {print NF-2}' docs/book/src/reference/crates.md \| sort -u` | a single value of `3` for the roster table |
| Row count preserved | count of roster data rows | 21, matching `ls -d crates/*/ \| wc -l` |
| No Helikon pin left in getting-started | `grep -rnE '^\s*paigasus-helikon[a-z-]*\s*=' docs/book/src/` | matches only in `concepts/model-providers.md` |
| Quickstart actually builds | `cargo add` the documented deps into a scratch crate, paste `quickstart.md:31-145`, `cargo check` | compiles clean |
| Markdown lint | `npx markdownlint-cli2` | clean — see caveat |

Baseline on this branch before any edit: `mdbook build` exits 0, linkcheck clean, using
`mdbook 0.4.43` + `mdbook-linkcheck 0.7.7` — the exact versions `docs.yml:16-17` pins.

**Local caveat:** `markdownlint-cli2@0.23.2` requires Node 20+ and CI runs Node 24
(`ci.yml:270`); this machine's newest is 18.13.0, so the required `markdown-lint` gate cannot be
reproduced locally and will first run in CI. Pre-existing environment limitation, not a consequence
of this change. Mitigation: `.markdownlint-cli2.jsonc` sets `"default": true` (so MD056
table-column-count is live) and `MD060: compact` (so table delimiter rows stay `| --- |`); the
column-count check above targets MD056 directly, which is the single most likely way to get this
edit wrong — one missed trailing cell out of 22 lines. `MD013` (line length) is the one rule disabled
repo-wide, so re-flowed prose is safe.

No Rust test suite is run. The change touches zero Rust; `cargo test --workspace --all-features`
across 21 crates would return no signal about it.

## Risks

- **Low — a reader loses the at-a-glance version comparison.** Accepted deliberately: the numbers
  they were comparing were wrong 19 times out of 21, so the view was worse than nothing. docs.rs is
  one click away per row.
- **Low — the `markdown-lint` required gate has no local substitute on this machine.** Mitigated by
  the targeted MD056 column-count assertion above; the residual failure is a fast, obvious CI red
  that costs one push to fix.
- **Low — `cargo add` snippets are less copy-pasteable into an existing `Cargo.toml`.** They are more
  copy-pasteable into a terminal, which is how a reader following a quickstart works, and they match
  the crate READMEs.
- **None to `book-build`.** Nothing is added to it; the change can only reduce what it renders.

## Acceptance criteria (from the issue)

- `crates.md` no longer carries a version column that can silently disagree with the workspace —
  **removed**.
- Generator failure mode — **n/a**, no generator is added.
- No other roster content is lost — the `Concern` and `State` columns stay, as do all 21 rows, their
  docs.rs links, and the surrounding prose.

## Appendix A: quickstart build evidence

Run in a scratch crate against the published `paigasus-helikon 0.5.17`, with the quickstart's own
program (`quickstart.md:31-145`) as `src/main.rs`.

With the dependency set exactly as `quickstart.md:7-14` documents it:

```text
error[E0433]: cannot find `serde_json` in the crate root
  --> src/main.rs:25:1
   |
25 | #[tool]
   | ^^^^^^^ could not find `serde_json` in the list of imported crates
   = note: this error originates in the attribute macro `tool`
```

(twice — once per `#[tool]` function). After `cargo add serde_json@1`:

```text
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.76s
```

And the documented step-4 command:

```text
$ cargo run --features openai,macros
error: the package 'qs' does not contain these features: macros, openai
```
