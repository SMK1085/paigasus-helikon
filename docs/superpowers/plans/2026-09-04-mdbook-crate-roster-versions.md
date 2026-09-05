# mdBook crate-roster version column removal — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the hand-maintained per-crate Version column from the mdBook crate roster, fix every
cross-reference that promised version content, and convert the book's stale hardcoded install
snippets to drift-free `cargo add` invocations.

**Architecture:** Pure documentation edit across six Markdown files. Nothing is generated, gated, or
added to CI — the fix is that the drifting content stops existing. Version facts are delegated to the
docs.rs links each roster row already carries. Install snippets follow one rule: Helikon crates
unpinned, third-party companions pinned to their major.

**Tech Stack:** Markdown, mdBook 0.4.43 + mdbook-linkcheck 0.7.7, markdownlint-cli2 0.23.2, `cargo add`.

**Spec:** `docs/superpowers/specs/2026-09-04-mdbook-crate-roster-versions-design.md`

## Global Constraints

- **No Rust source, `Cargo.toml`, or workflow file may be modified.** This PR is documentation-only.
  It therefore triggers no crate version bump, no CHANGELOG entry, and no facade bump.
- **`mdbook build` must stay clean.** `docs/book/book.toml` sets `[output.linkcheck] warning-policy =
  "error"`, so a single broken relative link fails the required `book-build` check. Only link to
  pages that exist: `concepts/` has `agent-loop, axum-server, core-primitives, mcp-integration,
  model-providers, multi-agent-patterns, observability-evaluation, permissions-guardrails-hooks,
  runtimes, sessions, structured-output-builder, tools`; `reference/` has `api-docs, cli, crates`;
  `getting-started/` has `quickstart, workspace-layout`. **There is no actix concepts page** — do not
  invent one.
- **Markdown lint rules.** `.markdownlint-cli2.jsonc` sets `"default": true` (so **MD056**
  table-column-count is live) and `"MD060": {"style": "compact"}` (delimiter rows stay `| --- |`,
  single-spaced, never re-padded to aligned). `MD013` (line length) is the only rule disabled
  repo-wide, so long prose lines are fine. **The gate cannot be run on this machine** —
  `markdownlint-cli2@0.23.2` needs Node 20+, CI uses Node 24 (`ci.yml:270`), this host has 18.13.0.
  Verify tables with the MD056 proxy in each task instead.
- **Snippet pinning rule, applied to every `cargo add` line written by this plan:** Helikon crates
  (`paigasus-helikon*`) take **no** version; every third-party companion takes `@1`. Rationale:
  `schemars` is public API of `paigasus-helikon-core` (`src/agent.rs:131` `pub schema:
  schemars::Schema`), so an unpinned `cargo add schemars` breaks the day schemars 2.0 ships.
- **Fenced-block language for new shell blocks is `bash`**, matching `quickstart.md:22,158` and the
  crate READMEs. (The book also uses `sh` in `axum-server.md`; do not follow that here.)
- **Commit prefix:** `docs(book): SMA-574 <lowercase message>`.
- **Do not touch `docs/book/src/concepts/model-providers.md`.** Its live pins at `:282` and `:402`
  are a deliberate, recorded non-goal.

---

### Task 1: Drop the Version column from the crate roster

**Files:**

- Modify: `docs/book/src/reference/crates.md:3` (opening paragraph), `:17` (version preamble),
  `:19-41` (table header, delimiter, 21 data rows)

**Interfaces:**

- Consumes: nothing.
- Produces: a 3-column roster table (`Crate | Concern | State`). Tasks 2 and 3 rewrite prose that
  points at this table, and rely on it no longer carrying versions.

- [ ] **Step 1: Record the "before" shape so the edit is verifiable**

```bash
sed -n '19,41p' docs/book/src/reference/crates.md | awk -F'|' '{print NF}' | sort -u
```

Expected: a single line, `6` (4 columns plus the two empty edge fields). 23 lines are in range:
header, delimiter, 21 data rows.

- [ ] **Step 2: Strip the trailing column from all 23 table lines**

The cells contain no escaped pipes (verified: `grep -c '\\|'` over the range returns 0), so removing
the final `| … |` pair is safe and mechanical:

```bash
# macOS / BSD sed (what this change was executed with):
sed -i '' -E '19,41 s/[[:space:]]*\|[^|]*\|[[:space:]]*$/ |/' docs/book/src/reference/crates.md

# GNU sed (Linux) takes no argument after -i:
# sed -i -E '19,41 s/[[:space:]]*\|[^|]*\|[[:space:]]*$/ |/' docs/book/src/reference/crates.md
```

`sed -i ''` is BSD/macOS syntax; GNU sed rejects the empty argument. Use whichever line matches your
platform, or run the substitution through `perl -i -pe` if you want one form that works on both.

This turns `| … | published | \`0.5.17\` |` into `| … | published |`, the header
`| Crate | Concern | State | Version |` into `| Crate | Concern | State |`, and the delimiter
`| --- | --- | --- | --- |` into `| --- | --- | --- |`.

- [ ] **Step 3: Verify the table is now uniformly 3 columns (MD056 proxy)**

```bash
sed -n '19,41p' docs/book/src/reference/crates.md | awk -F'|' '{print NF}' | sort -u
wc -l < <(sed -n '19,41p' docs/book/src/reference/crates.md)
grep -c '`0\.' docs/book/src/reference/crates.md
```

Expected: a single value `5`; `23` lines; and `3` remaining backticked-version matches — which must
be exactly the three `version = "0.0.0"` prose mentions at `:47-49` about the non-published `tests/*`
members. **Leave those alone**; `0.0.0` is a permanent property of a `publish = false` crate, not
drift.

- [ ] **Step 4: Replace the version preamble at `:17`**

Replace this entire line:

```text
Versions below are **current as of 2026-08-16** and move every release — read each crate's `Cargo.toml` (or the root `[workspace.dependencies]` pins) for the live numbers, and the [crates.io page](https://crates.io/crates/paigasus-helikon) / docs.rs for what is actually published.
```

with:

```text
This table deliberately carries no version numbers. Each crate name links to its [docs.rs](https://docs.rs) page, which always shows the current published version; for the in-tree version, read that crate's `Cargo.toml`. Versions move every release, so any number mirrored here would be wrong within days.
```

- [ ] **Step 5: Fix the opening paragraph at `:3`**

The paragraph currently calls the page "the version-bearing map". It no longer bears versions.
Replace only that clause — the rest of the sentence and the surrounding paragraph stay:

```text
This page is the version-bearing map: one row per crate, what it owns, whether it is published, and how the crates depend on each other.
```

becomes:

```text
This page is the ownership map: one row per crate, what it owns, whether it is published, and how the crates depend on each other.
```

- [ ] **Step 6: Build the book**

```bash
cd docs/book && mdbook build
```

Expected: exit 0, with the `linkcheck` backend running and reporting nothing.

- [ ] **Step 7: Commit**

```bash
git add docs/book/src/reference/crates.md
git commit -m "docs(book): SMA-574 drop the hand-maintained version column from the crate roster"
```

---

### Task 2: Fix the cross-references that promised version content

**Files:**

- Modify: `README.md:29`, `README.md:52`
- Modify: `docs/book/src/concepts/mcp-integration.md:125`

**Interfaces:**

- Consumes: Task 1's 3-column roster.
- Produces: nothing further tasks depend on.

Both `README.md` edits are required by CLAUDE.md's rule that the root README is brought into line
whenever the crate roster changes. This is the conscious call that rule demands, not a skip.

- [ ] **Step 1: Confirm all three sites still read as expected**

```bash
grep -n "current published versions\|and current version" README.md
grep -n "for version and feature details" docs/book/src/concepts/mcp-integration.md
```

Expected: `README.md` lines 29 and 52, `mcp-integration.md` line 125. If a line number has moved,
match on the text, not the number.

- [ ] **Step 2: Edit `README.md:29`**

```text
See the [crate roster](https://smk1085.github.io/paigasus-helikon/reference/crates.html) for the full feature → crate map and current published versions.
```

becomes:

```text
See the [crate roster](https://smk1085.github.io/paigasus-helikon/reference/crates.html) for the full feature → crate map, and [docs.rs](https://docs.rs/paigasus-helikon) for the current published version.
```

- [ ] **Step 3: Edit `README.md:52`**

```text
See the [crate roster](https://smk1085.github.io/paigasus-helikon/reference/crates.html) for each crate's concern, published state, and current version.
```

becomes:

```text
See the [crate roster](https://smk1085.github.io/paigasus-helikon/reference/crates.html) for each crate's concern and published state; each row links to its docs.rs page for the current version.
```

- [ ] **Step 4: Edit `docs/book/src/concepts/mcp-integration.md:125`**

```text
See [Tools](./tools.md) for the `Tool<Ctx>` trait these adapters implement, and the [crate reference](../reference/crates.md) for version and feature details.
```

becomes:

```text
See [Tools](./tools.md) for the `Tool<Ctx>` trait these adapters implement, and the [crate reference](../reference/crates.md) for ownership and feature details.
```

- [ ] **Step 5: Verify no version promise survives**

```bash
grep -rn "crate roster\|reference/crates\|crates.html" README.md docs/book/src/ crates/*/README.md \
  | grep -i "version"
```

Expected: no output. (`api-docs.md:22` is handled in Task 3 — if it still appears here, that is
expected at this point and is fixed next.)

- [ ] **Step 6: Build the book and commit**

```bash
cd docs/book && mdbook build && cd ../..
git add README.md docs/book/src/concepts/mcp-integration.md
git commit -m "docs(book): SMA-574 stop promising roster versions in the readme and mcp page"
```

---

### Task 3: Correct `api-docs.md` — the roster pointer and its stale publish facts

**Files:**

- Modify: `docs/book/src/reference/api-docs.md:22` (roster pointer), `:26` (publish count),
  `:7-20` (published-crates list — six crates missing)

**Interfaces:**

- Consumes: Task 1's roster.
- Produces: nothing further tasks depend on.

The `:26` and `:7-20` fixes are adjacent rot of exactly the same kind, in a file this PR already has
to edit; they are in scope by an explicit approved scope call recorded in the spec.

- [ ] **Step 1: Edit the roster pointer at `:22`**

```text
Most users depend only on the `paigasus-helikon` facade and enable the features they need; the facade docs link out to each sibling. Crate versions move every release — see [Crate overview](./crates.md) for the current numbers.
```

becomes:

```text
Most users depend only on the `paigasus-helikon` facade and enable the features they need; the facade docs link out to each sibling. Crate versions move every release — each docs.rs link above shows the current published version, and [Crate overview](./crates.md) maps which crate owns which concern.
```

- [ ] **Step 2: Fix the publish count at `:26`**

The workspace has 21 crates; 20 publish, and `-sessions-testkit` does not. Root `README.md:33`,
`workspace-layout.md:50`, and `introduction.md:19` already say this correctly — `api-docs.md` is the
lone dissenter at "18".

```text
All 18 non-internal crates now publish to crates.io
```

becomes:

```text
All 20 non-internal crates now publish to crates.io
```

Leave the rest of that sentence (the SMA-332/SMA-333 ascend history and the `-sessions-testkit`
exception) exactly as it is.

- [ ] **Step 3: Add the six missing crates to the published list**

The list at `:7-20` has 14 of 20 entries. Insert each in the position matching the existing grouping
(providers together, sessions together, runtimes together). Descriptions mirror the wording already
used in `crates.md` so the two pages agree.

After the `paigasus-helikon-providers-anthropic` line, add:

```markdown
- [`paigasus-helikon-providers-bedrock`](https://docs.rs/paigasus-helikon-providers-bedrock) — Amazon Bedrock Converse API model adapter (`BedrockModel`). See [Model providers](../concepts/model-providers.md).
- [`paigasus-helikon-providers-gemini`](https://docs.rs/paigasus-helikon-providers-gemini) — Google Gemini model adapter (`GeminiModel`; Developer API and Vertex AI). See [Model providers](../concepts/model-providers.md).
- [`paigasus-helikon-providers-litellm`](https://docs.rs/paigasus-helikon-providers-litellm) — LiteLLM proxy adapter (`LiteLlmModel`; OpenAI-compatible gateway). See [Model providers](../concepts/model-providers.md).
```

After the `paigasus-helikon-sessions-sqlite` line, add:

```markdown
- [`paigasus-helikon-sessions-postgres`](https://docs.rs/paigasus-helikon-sessions-postgres) — PostgreSQL `Session` backend (`PostgresSession`). See [Sessions](../concepts/sessions.md).
- [`paigasus-helikon-sessions-redis`](https://docs.rs/paigasus-helikon-sessions-redis) — Redis Streams `Session` backend (`RedisSession`). See [Sessions](../concepts/sessions.md).
```

After the `paigasus-helikon-runtime-axum` line, add:

```markdown
- [`paigasus-helikon-runtime-actix`](https://docs.rs/paigasus-helikon-runtime-actix) — actix-web port of `runtime-axum` with the same routes and wire format, for embedding into an existing actix-web service. See [Runtimes](../concepts/runtimes.md).
```

Note: `runtime-actix` points at `runtimes.md`, **not** an actix page — no such page exists, and
`axum-server.md` is axum-specific. Linking a non-existent page fails the required `book-build` gate.

- [ ] **Step 4: Verify the list is complete and the links resolve**

```bash
grep -c '^- \[`paigasus-helikon' docs/book/src/reference/api-docs.md
```

Expected: `20`.

```bash
cd docs/book && mdbook build
```

Expected: exit 0, linkcheck silent. A typo in any of the seven relative links added or edited above
fails here, which is exactly the check that matters.

- [ ] **Step 5: Commit**

```bash
git add docs/book/src/reference/api-docs.md
git commit -m "docs(book): SMA-574 fix the api-docs roster pointer, publish count, and crate list"
```

---

### Task 4: Fix the quickstart's install block and run command

**Files:**

- Modify: `docs/book/src/getting-started/quickstart.md:5` (heading), `:7-14` (dependency block),
  `:158-160` (run command)

**Interfaces:**

- Consumes: the Global Constraints pinning rule and `bash` fence rule.
- Produces: the `cargo add` block shape that Task 5 mirrors in `workspace-layout.md`.

This task fixes two defects proven by building the page's own code (spec Appendix A): the block omits
`serde_json`, and the run command names features the reader's package does not have.

- [ ] **Step 1: Retitle the step at `:5`**

```text
## 1. Add the dependency
```

becomes:

```text
## 1. Add the dependencies
```

- [ ] **Step 2: Replace the TOML block at `:7-14` with `cargo add`**

Replace the whole fenced block, fence lines included:

````text
```toml
[dependencies]
paigasus-helikon = { version = "0.3", features = ["openai", "macros"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
anyhow = "1"
serde = { version = "1", features = ["derive"] }
schemars = "1"
```
````

with:

````text
```bash
cargo add paigasus-helikon --features openai,macros
cargo add tokio@1 --features macros,rt-multi-thread
cargo add serde@1 --features derive
cargo add anyhow@1 schemars@1 serde_json@1
```
````

Three things about this block are load-bearing:

- `paigasus-helikon` carries **no** version, so it never goes stale. The companions keep `@1` because
  `schemars` and `serde` are public API of `paigasus-helikon-core`.
- `serde_json` is **new and required**. The `#[tool]` attribute expands to absolute `::serde_json::`
  paths in the consumer's crate (`crates/paigasus-helikon-macros/src/expand.rs`, 8 sites; documented
  at `crates/paigasus-helikon/Cargo.toml:65-66`) and the facade does not re-export it. Without it the
  page fails with `E0433: cannot find serde_json in the crate root`, once per `#[tool]` function.
- The fence is `bash`, not `toml`.

Leave the paragraph that follows the block (explaining what `openai` and `macros` pull in) unchanged
— it is still accurate.

- [ ] **Step 3: Fix the run command at `:159`**

```text
OPENAI_API_KEY=sk-... cargo run --features openai,macros
```

becomes:

```text
OPENAI_API_KEY=sk-... cargo run
```

The features were enabled on the *dependency* in step 1; repeating them here refers to the reader's
own package and fails with `error: the package '<name>' does not contain these features: macros,
openai`. **Do not touch the second block at `:164-167`** — `cargo run -p paigasus-helikon --features
openai,macros --example budget_assistant_openai` runs inside this workspace and is correct.

- [ ] **Step 4: Prove the page now builds, end to end**

This is the real test for this task. Build the page's own program against the published crate in a
throwaway crate outside the repo:

```bash
SCRATCH=$(mktemp -d)
cargo new --quiet "$SCRATCH/qs"
cd "$SCRATCH/qs"
cargo add paigasus-helikon --features openai,macros
cargo add tokio@1 --features macros,rt-multi-thread
cargo add serde@1 --features derive
cargo add anyhow@1 schemars@1 serde_json@1
```

Then copy the quickstart's Rust program — the single fenced `rust` block, currently
`quickstart.md:31-145` — into this crate's `src/main.rs` (you are already inside `$SCRATCH/qs`), and:

```bash
cargo check
```

Expected: `Finished \`dev\` profile`. No `E0433`. If the line range has shifted after the earlier
edits, re-derive it with `grep -n '^```' docs/book/src/getting-started/quickstart.md` and take the
`rust` block that spans roughly 115 lines. Remove `$SCRATCH` afterwards.

- [ ] **Step 5: Build the book and commit**

```bash
cd docs/book && mdbook build && cd ../..
git add docs/book/src/getting-started/quickstart.md
git commit -m "docs(book): SMA-574 make the quickstart install block drift-free and buildable"
```

---

### Task 5: Fix `workspace-layout.md` — cross-references and install snippets

**Files:**

- Modify: `docs/book/src/getting-started/workspace-layout.md:8` (cross-reference), `:82-85` and
  `:90-97` (the two dependency blocks under `## Picking your surface`, heading at `:77`), `:119`
  (Next-steps entry)

**Interfaces:**

- Consumes: Task 4's `cargo add` block shape and the Global Constraints pinning rule.
- Produces: nothing further tasks depend on.

- [ ] **Step 1: Fix the cross-reference at `:8`**

```text
This page is about **how to depend** on the SDK. For the per-crate version and ownership table,
see [Crates reference](../reference/crates.md).
```

becomes:

```text
This page is about **how to depend** on the SDK. For the per-crate ownership table,
see [Crates reference](../reference/crates.md).
```

- [ ] **Step 2: Fix the Next-steps entry at `:119`**

```text
- [Crates reference](../reference/crates.md) — per-crate versions and ownership.
```

becomes:

```text
- [Crates reference](../reference/crates.md) — per-crate ownership and dependency direction.
```

- [ ] **Step 3: Convert the core-only block at `:82-85`**

````text
```toml
[dependencies]
paigasus-helikon-core = "0.5"
```
````

becomes:

````text
```bash
cargo add paigasus-helikon-core
```
````

- [ ] **Step 4: Convert the facade block at `:90-97`**

````text
```toml
[dependencies]
paigasus-helikon = { version = "0.4", features = ["openai", "macros", "sessions-sqlite"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
anyhow = "1"
serde = { version = "1", features = ["derive"] }
schemars = "1"
```
````

becomes:

````text
```bash
cargo add paigasus-helikon --features openai,macros,sessions-sqlite
cargo add tokio@1 --features macros,rt-multi-thread
cargo add serde@1 --features derive
cargo add anyhow@1 schemars@1 serde_json@1
```
````

`serde_json@1` is added here for the same reason as in Task 4 — this block also feeds a `#[tool]`
example. Both blocks are converted so one section does not mix two install idioms. The prose after
each block is unchanged and stays accurate.

- [ ] **Step 5: Verify no Helikon pin survives in the book outside the deferred file**

```bash
grep -rnE '^[[:space:]]*paigasus-helikon[a-z-]*[[:space:]]*=' docs/book/src/
```

Expected: matches **only** in `docs/book/src/concepts/model-providers.md` (lines around `:282` and
`:402-410`), which is a recorded non-goal. Any match in `getting-started/` or `reference/` means a
snippet was missed. Note this regex deliberately catches the bare-string form
(`paigasus-helikon-core = "0.5"`) that a `version = "0\.` search would not.

- [ ] **Step 6: Build the book and commit**

```bash
cd docs/book && mdbook build && cd ../..
git add docs/book/src/getting-started/workspace-layout.md
git commit -m "docs(book): SMA-574 convert workspace-layout install snippets to cargo add"
```

---

### Task 6: Whole-book verification

**Files:** none modified — this task only runs checks and fixes anything they surface.

- [ ] **Step 1: Full book build**

```bash
cd docs/book && mdbook build && cd ../..
```

Expected: exit 0, linkcheck clean.

- [ ] **Step 2: Roster table integrity (MD056 proxy)**

```bash
awk -F'|' '/^\| \[?`paigasus|^\| Crate|^\| --- /{print NF}' docs/book/src/reference/crates.md | sort -u
```

Expected: a single value, `5` (3 columns plus two edge fields). More than one value means a row lost
or kept a cell and `markdown-lint` will fail in CI, where it cannot be run locally.

- [ ] **Step 3: Row count preserved**

```bash
grep -c '^| \[\?`paigasus-helikon' docs/book/src/reference/crates.md
ls -d crates/*/ | wc -l
```

Expected: both `21`. No crate lost its row.

- [ ] **Step 4: No version promises and no Helikon pins remain**

```bash
grep -rni "crate roster\|reference/crates\|crates.html" README.md docs/book/src/ crates/*/README.md | grep -i version
grep -rnE '^[[:space:]]*paigasus-helikon[a-z-]*[[:space:]]*=' docs/book/src/
```

Expected: first command silent; second matches only `concepts/model-providers.md`.

- [ ] **Step 5: Confirm the diff is documentation-only**

```bash
git diff --stat main...HEAD -- . ':(exclude)*.md'
```

Expected: empty. Any non-Markdown file in the diff violates the Global Constraints and must be
reverted.

- [ ] **Step 6: Attempt the markdown lint, and report honestly if it cannot run**

```bash
npx markdownlint-cli2
```

On this host this is expected to fail with `SyntaxError: Invalid regular expression flags` because
`markdownlint-cli2@0.23.2` requires Node 20+ and the host has 18.13.0. That is a pre-existing
environment limitation. **Record it as unrun — do not report the gate as passing.** Steps 2 and 3
above are the substitute for the rule (MD056) most likely to catch an error in this change.

---

## Self-Review

**Spec coverage.** Every spec section maps to a task: §1 `crates.md` → Task 1; §2 cross-references →
Task 2 (README, mcp-integration) and Task 3 (api-docs, including the approved adjacent-rot fix); §3
`workspace-layout.md` → Task 5; §4 `quickstart.md` → Task 4; the Verification table → Task 6 plus the
per-task checks. The spec's non-goals (`model-providers.md`, no CI change, no Rust change) appear as
Global Constraints and as Task 6 Step 5's guard.

**Placeholders.** None. Every edit is given as exact before/after text; every check is a runnable
command with a stated expected result.

**Consistency.** The `cargo add` block in Task 4 Step 2 and Task 5 Step 4 are identical except for
the feature list, and both follow the Global Constraints pinning rule. The MD056 column-count proxy
is expressed the same way in Task 1 Step 3 and Task 6 Step 2. `serde_json@1` appears in both install
blocks with the same justification.
