# SMA-557 — Document the `paigasus::*` tracing target namespace for operators

**Linear:** [SMA-557](https://linear.app/smaschek/issue/SMA-557/document-the-paigasus-tracing-target-namespace-for-operators)
**Branch:** `feature/sma-557-document-the-paigasus-tracing-target-namespace-for-operators`
**Predecessor:** SMA-543 (PR #209, merged). Its own spec, at
`docs/superpowers/specs/2026-08-19-sma-543-tracing-target-design.md:379`, named
this gap as follow-up.

> **Revision note.** This spec was substantially rewritten after an adversarial
> review. The first draft's central operator claim — that
> `RUST_LOG='paigasus=debug'` reaches provider events only — was **inverted**.
> `EnvFilter` matches targets by raw string prefix, so that directive reaches
> *everything*. §1.2 records the executable evidence. The inventory numbers were
> also wrong (a doc comment counted as a call site; span macros omitted
> entirely). Every number below has been re-measured.

---

## 1. Problem

Provider crates emit `tracing` events on hand-chosen targets in a `paigasus::*`
namespace, independent of the Rust module path the event lives in. That is a
designed-in filtering surface — the whole value of a per-subsystem target is
being able to select it.

Nothing tells an operator the surface exists. `paigasus::`, `RUST_LOG` and
`EnvFilter` appear nowhere in `docs/book/src/` and in no crate `README.md`; the
only hits anywhere in the repo are under `docs/superpowers/` (both `plans/` and
`specs/`), which are internal design artifacts, not user documentation. The
namespace is discoverable only by reading provider source.

This is also what let SMA-543 hide: seven call sites emitted on the wrong target
from the day they were written, and no user could have noticed, because no user
was ever told to filter on them.

### 1.1 Inventory, re-measured against source

On `main@a155ee3f`, counting only real call sites — the naive grep is wrong,
because `crates/paigasus-helikon-providers-litellm/src/translate/request.rs:497`
is a `///` doc comment containing `` `target: "paigasus::openai::translate"` ``
and is not a call site at all. (SMA-543's own guard already knows this hazard:
`tests/workspace-lints/src/lib.rs`'s `mask_trivia` blanks comments precisely so
comment text cannot be mistaken for code.)

**56 call sites** across **20 files**, resolving to **16 distinct target
strings** under **6 components**:

| Component | Crate | Subsystems present | Sites |
|---|---|---|---|
| `paigasus::openai` | `paigasus-helikon-providers-openai` | `translate` 5, `chat` 4, `responses` 3 | 12 |
| `paigasus::anthropic` | `paigasus-helikon-providers-anthropic` | `translate` 4, `stream` 3, `sse` 1 | 8 |
| `paigasus::bedrock` | `paigasus-helikon-providers-bedrock` | `translate` 11, `stream` 2, `builder` 1 | 14 |
| `paigasus::gemini` | `paigasus-helikon-providers-gemini` | `translate` 2, `sse` 1 | 3 |
| `paigasus::litellm` | `paigasus-helikon-providers-litellm` | `stream` 9, `translate` 6, `http` 2, `sse` 1 | 18 |
| `paigasus::temporal` | `paigasus-helikon-runtime-temporal` | `activities` 1 | 1 |

### 1.2 `EnvFilter` matches by raw prefix, not by segment

**This is the fact the whole document turns on, and it was verified by execution,
not by reading tracing-subscriber's source.** A probe program emitted real
`tracing` events on every target shape present in the workspace, through a real
`Registry` + `EnvFilter`, and recorded which survived:

| Directive | `paigasus::openai::chat` | `paigasus::temporal::activities` | `paigasus_helikon_core::session` | `paigasus_helikon_runtime_axum::registry` | `paigasus::openai_compat::chat` | `hyper::client` |
|---|---|---|---|---|---|---|
| `paigasus=debug` | ✅ | ✅ | **✅** | **✅** | ✅ | — |
| `paigasus::=debug` | ✅ | ✅ | — | — | ✅ | — |
| `paigasus::openai=debug` | ✅ | — | — | — | **✅** | — |
| `off,paigasus::litellm::stream=trace` | — | — | — | — | — | — |
| `paigasus_helikon_core=debug` | — | — | ✅ | — | — | — |

The `off,paigasus::litellm::stream=trace` row shows `—` in every column only
because the probe program carried no `paigasus::litellm::stream` column to
test against — that row is absence of evidence, not evidence of absence. The
directive is valid and reaches real call sites, e.g.
`crates/paigasus-helikon-providers-litellm/src/stream.rs:188`.

Three consequences, all load-bearing:

1. **`paigasus=debug` is not a curated-namespace filter.** It is a raw prefix
   that also catches every `paigasus_helikon_*` module-path target. It reaches
   *everything*, which is useful but must not be presented as selecting the
   hand-chosen namespace.
2. **`paigasus::=debug` — with the trailing `::` — is the curated filter.** It
   selects exactly the hand-chosen namespace and excludes the module-path
   targets. This form is what makes D1's "`paigasus::` is a stable surface"
   claim operationally real, and it must appear in the recipes.
3. **Component names collide by prefix.** `paigasus::openai=debug` also matches
   a hypothetical `paigasus::openai_compat::chat`. D1 therefore needs a naming
   constraint, not just a stability promise.

### 1.3 Two namespaces, and the trace tree the page already teaches

`paigasus::*` is in practice a **provider** namespace. Measured over crate
`src/` (excluding comment lines and integration tests):

- **All five provider crates are 100% targeted** — every `tracing` call site in
  their `src/` passes an explicit `target:`, with no exception. Two untargeted
  `tracing::info!` calls exist in
  `crates/paigasus-helikon-providers-anthropic/tests/live.rs:15` and `:158`;
  those are integration tests, outside the claim's scope and outside the guard's
  walk of `src/`.
- **`core` and the runtime crates are 41-of-42 untargeted.** They emit on
  module-path targets — `paigasus_helikon_core::agent`,
  `paigasus_helikon_runtime_axum::registry`, and so on. The single exception is
  `paigasus::temporal::activities`, one call site in `runtime-temporal`, whose
  other three sites are untargeted.

Breakdown of the 41: `core` 12, `runtime-agentcore` 11, `runtime-axum` 7,
`runtime-actix` 6, `runtime-temporal` 3, `runtime-tokio` 2.

**The `core` count includes five `tracing::info_span!` sites, and they matter
more than any event.** They are the only span macros in the workspace, and they
create exactly the trace tree the page being edited already documents at
`docs/book/src/concepts/observability-evaluation.md:85-87`:

| Span | Site | Module-path target |
|---|---|---|
| `agent.run` | `core/src/agent.rs:729`, `core/src/workflow.rs:51` | `paigasus_helikon_core::agent`, `…::workflow` |
| `agent.turn` | `core/src/agent.rs:853` | `paigasus_helikon_core::agent` |
| `gen_ai.chat` | `core/src/agent.rs:913` | `paigasus_helikon_core::agent` |
| `tool.execute` | `core/src/agent.rs:542` | `paigasus_helikon_core::agent` |

`EnvFilter` filters spans by target identically to events. A "Filtering by
target" section on the very page that teaches
`invoke_agent → agent.turn → chat / execute_tool` **must** say how to select
that tree — and the answer lives in `paigasus_helikon_core`, not in
`paigasus::*` at all. Omitting it would be the largest completeness gap
available.

For completeness, `mcp`, `tools`, `sessions-*`, `evals`, `cli`, `macros` and the
facade contain **zero** `tracing` call sites, and there is no
`#[tracing::instrument]` anywhere in `crates/`. "core and the runtime crates" is
therefore an exhaustive list of the untargeted emitters, not a partial one.

### 1.4 The undecided question

The ticket calls out that the stability expectation for these strings is
undecided, and that answering it is the most valuable part of the work. SMA-543
changed four targets *in effect* — they previously resolved to module paths —
which would have been a breaking change had anyone been relying on them.
`docs/book/src/decisions/index.md:5` says a formal ADR section is "the planned
next step", and `CONTRIBUTING.md` carries only a commit-type → semver table.

---

## 2. Decisions

### D1 — Two-tier stability

**`paigasus::` and `paigasus::<component>` are a stable filtering surface.**
Renaming or removing a component is a breaking change. **The `::<subsystem>`
leaf is an implementation detail** and may change in any release.

Three things make this operational rather than aspirational:

**(a) The segment rule, corrected for prefix matching.** State it as **exactly
two segments** for durable filters:

| Form | Segments | Status |
|---|---|---|
| `paigasus` | 1 | Raw prefix. Also catches `paigasus_helikon_*`. Documented over-match, not a namespace selector. |
| `paigasus::` | 1 + `::` | Curated namespace, exactly. Stable. |
| `paigasus::openai` | 2 | Stable. The durable form for alerting and dashboards. |
| `paigasus::openai::chat` | 3 | Debugging only. Leaf may change in any release. |

The first draft's "≤ 2 segments" rule was wrong at its lower bound: one segment
is ≤ 2 and is precisely the over-matching form.

**(b) A naming constraint.** Because matching is prefix-based, **no component
name may be a prefix of another** — `openai` and `openai_compat` cannot coexist,
because a saved `paigasus::openai` filter would silently widen to include the
new component. This constraint is part of the stability promise, not a separate
style rule.

**(c) A mechanism the repo can actually execute.** Renaming a component requires
a commit carrying a `BREAKING CHANGE:` footer. release-plz maps that to a
**minor** bump on a 0.x crate and surfaces it in the generated CHANGELOG, so the
break is recorded where a consumer will see it. This uses machinery that already
exists; no new process is invented, and nothing here depends on the ADR section
that does not yet exist.

The book will also state that the D1 guarantee **begins with this document** and
is not retroactive, so SMA-543's four in-effect changes are not read as a broken
promise.

Rejected alternatives:

- *Fully unstable.* Defensible pre-1.0, but it documents a surface while telling
  operators not to depend on it — leaving the ticket's value latent, the exact
  condition SMA-557 exists to end.
- *Full public contract on all 16 strings.* Best for operators building
  alerting, but freezes 16 strings across 20 files, taxes every provider
  refactor, and retroactively reclassifies SMA-543 as a breaking change.

### D2 — Docs-only scope

Document the namespace **as it is today**, including both namespaces and the
prefix semantics, honestly. Do **not** normalize the 41 untargeted call sites in
this ticket.

Rationale: SMA-557 is labelled `area:docs`; normalizing is a workspace-wide
behaviour change across six crates that reads as its own feature ticket, and it
would trigger release bumps on `core` plus four runtimes plus the facade
cascade. The ticket's own "Note on enforcement" already says convention
enforcement is a separate decision.

### D3 — Guard the component list, not the target list

A test asserts that the set of `paigasus::<component>` prefixes in source equals
the set documented in the book.

It fires on a new provider being added without a doc update, and on a component
being renamed — which under D1 is exactly the breaking change a human must
consciously approve. It is deliberately **silent on leaf renames**, because D1
declares those free; guarding the full 16-string list would redden CI on
legitimate refactors and pressure the docs toward freezing what D1 called free.

**The guard asserts presence, never guarantee.** The component table carries a
**Status** column (`stable` / `provisional`) that the guard ignores entirely.
Without this, `temporal` would hold three contradictory positions at once — in
the table, explicitly un-guaranteed in prose, and breaking-change-protected by
the guard — and acting on §6's own follow-up by removing the lone
`paigasus::temporal::activities` site would redden CI exactly as if `openai` had
been deleted. The Status column resolves that: `temporal` is listed, is
`provisional`, and the guard tracks only whether the row exists.

Relying on convention alone was rejected: CLAUDE.md already requires updating
the book in the same PR as any user-facing change, and that is precisely the
mechanism that failed here — 13 of 17 book pages sat as stubs through all of
Stage 1 until the SMA-423 catch-up.

---

## 3. The book section

One new subsection in `docs/book/src/concepts/observability-evaluation.md`,
titled **"Filtering by target"**, placed under `## Observability` after
`### Exporting to an OTel backend` and before `## Evaluation`. The existing prose
moves from what is emitted (`TracerHandle`) to where it goes (OTel export);
selecting a subset is the natural third beat, and it sits directly below the
paragraph that introduces the `invoke_agent → agent.turn → chat` tree §1.3
requires it to address.

### 3.1 Two namespaces, and how matching works

Lead with the mechanism, because every recipe depends on it:

- Helikon events carry targets from **two** namespaces: the hand-chosen
  `paigasus::<component>::<subsystem>` (providers), and ordinary Rust
  module paths `paigasus_helikon_*::…` (core and the runtimes).
- `EnvFilter` matches a directive against a target by **raw string prefix**, not
  by `::`-segment. Spell out the consequence with the §1.2 table's key rows:
  `paigasus` catches both namespaces; `paigasus::` catches only the curated one.

This ordering matters. A reader who meets the component table first will assume
segment matching and write filters that quietly over-match.

### 3.2 The component table

Six rows: Component, Crate, Subsystems seen today, **Status**. The subsystem
column is explicitly labelled as examples, not a frozen list. The Status column
is `stable` for the five providers and `provisional` for `temporal`.

The table sits inside the marked region described in §4.1.

### 3.3 What is not in the namespace

- `core` and the runtime crates emit on module paths; name the concrete strings
  an operator will actually type (`paigasus_helikon_core`,
  `paigasus_helikon_runtime_axum`, …).
- **The run/turn/chat/tool span tree documented earlier on this page comes from
  `paigasus_helikon_core`** — filtering it has nothing to do with `paigasus::*`.
  Cross-reference the earlier section explicitly. Be precise about *which*
  module, per §1.3's table: most of the tree is raised in
  `paigasus_helikon_core::agent`, but `agent.run` has a second creation site in
  `paigasus_helikon_core::workflow` (`workflow_run_span`), reached by the
  sequential, parallel and loop workflows and by the graph and swarm agents.
  Filtering on the bare crate prefix catches both; a narrower
  `paigasus_helikon_core::agent` silently misses a multi-agent run's top-level
  span. An earlier draft of this section said the whole tree came from
  `::agent`, contradicting §1.3 — that wording reached the book and was caught
  in review.
- `paigasus::temporal` is a single call site in an otherwise-untargeted crate:
  listed, `provisional`, and not covered by the D1 guarantee.
- The gap is known and tracked as a follow-up (§6), stated in prose. **No Linear
  URL**: `linear.app` appears nowhere in `docs/book/` today, the workspace is
  private, and `docs/book/book.toml:20` sets `follow-web-links = false`, so
  linkcheck could not catch a wrong link anyway.

### 3.4 The stability statement

D1 in prose: the four-row form table from D1(a), the no-prefix-collision
constraint from D1(b), the `BREAKING CHANGE:` mechanism from D1(c), and the
not-retroactive note.

### 3.5 Recipes

Four `RUST_LOG` examples, each showing something the others cannot:

1. **Curated namespace only** — `warn,paigasus::=debug`. The trailing `::` is
   the point; annotate it.
2. **One provider, durable** — `warn,paigasus::openai=debug`. The two-segment
   form D1 blesses for alerting.
3. **One subsystem, debugging** — `off,paigasus::litellm::stream=trace`.
   Annotated as leaf-level: **this example may go stale by design**, since D1
   declares leaves free to change. Saying so in the book means a reader who
   finds it dead is not confused, and it is why no guard covers it.
4. **The agent trace tree** — `warn,paigasus_helikon_core=debug`, selecting the
   `agent.run` / `agent.turn` / `gen_ai.chat` / `tool.execute` spans.

The first draft's "full coverage" recipe
(`paigasus=debug,paigasus_helikon_core=debug,…`) is **dropped**: per §1.2 the
trailing directives are no-ops, since `paigasus=debug` already prefix-matches
them. Recipe 1 plus recipe 4 cover the same ground correctly. Every target named
must exist in source at the time of writing.

---

## 4. The drift guard

### 4.1 Marked region

The table is wrapped in HTML comments naming the test that depends on them:

```
<!-- tracing-components:start — keep in sync; asserted by
     tests/workspace-lints/tests/tracing_target_docs.rs -->
… table …
<!-- tracing-components:end -->
```

Markers were chosen over parsing the table's header row (couples the test to
column layout) and over scraping every `paigasus::x` on the page (§3.1 and §3.3
deliberately mention `paigasus`, `paigasus::` and `paigasus_helikon_*` nearby, so
a page-wide scrape would collide with prose).

**The convention line `paigasus::<component>::<subsystem>` from §3.1 lives
outside the markers.** Its `<component>` placeholder would otherwise be parsed as
a component.

### 4.2 Source side — resolving the lexer conflict

The obvious design does not work, and the reason must be stated so it is not
rediscovered during implementation. `mask_trivia`
(`tests/workspace-lints/src/lib.rs:308`) blanks **string literals** as well as
comments — but the literal contents are exactly the bytes a target scan must
read. The masked buffer cannot supply them.

Reintroducing a second, unmasked lexer is explicitly warned against at
`tests/workspace-lints/src/lib.rs:206-208` ("a second lexer disagreeing with the
first about what counts as a comment").

**Resolution — one lexer, two outputs.** Extend `mask_trivia` to additionally
return the byte ranges of **string literals**, exactly as it already returns
line-comment ranges for `collect_allow_marker_lines`. Then:

```rust
/// Distinct `<component>` segments from every `target: "paigasus::…"`
/// literal in one file's source.
pub fn scan_targets(src: &str) -> BTreeSet<String>
```

works as: find `target:` **in the masked buffer** (so it is real code, never
comment or literal text) → take the literal span that starts at the next
non-space byte → read that span from the **original** source → if it starts
`paigasus::`, take the segment after the first `::`.

**This also makes the self-scan safe, without any path exclusion.** The crate's
own unit-test fixtures are written as `let src = "…target: \"paigasus::…\"…"` —
one *outer* literal. The lexer sees a single literal span and blanks the whole
thing, so no `target:` token is ever visible inside it and no phantom component
is produced. That is the same property `src/lib.rs:846-848` already relies on,
extended to literals; the existing deliberate absence of self-exclusion is
preserved rather than contradicted.

`BTreeSet` for deterministic assertion messages. A literal with no second `::`
(a bare `paigasus::foo`) yields `foo`; this shape does not occur today and must
not panic. The crate carries `[lints] workspace = true`, so `missing_docs`
applies to the new public function and any new public type.

Lib unit tests cover: a normal two-segment target; a bare one-segment target; a
non-`paigasus` target ignored; `target =` (the SMA-543 form) ignored; a
`paigasus::` mention in a `//` comment, a `///` doc comment and a nested string
literal all ignored. The last is not hypothetical — §1.1's miscount was caused by
exactly that doc comment.

### 4.3 Doc side and the assertion

The test:

1. Derives the repo root from `CARGO_MANIFEST_DIR`, not the process CWD, matching
   `tracing_target_syntax.rs`.
2. Walks **`crates/` and `tests/` only** — deliberately *not* the repo root.
   `.claude/worktrees/` can hold full checkouts of other branches, and scanning
   those would make the verdict depend on which unrelated worktrees a developer
   happens to have. Not hypothetical: this ticket was developed in
   `.claude/worktrees/sma-557-tracing-targets`.
3. Reuses the symlink-safe `collect_rs` walker shape (`symlink_metadata`, not
   `is_dir`, so a symlink to an ancestor cannot recurse unboundedly).
4. Unions `scan_targets` over every file into the source set.
5. Parses the marked region into the documented set by an **exactly specified**
   rule: the **first cell of each table body row** between the markers, which
   must match ``^`paigasus::([a-z0-9_]+)`$``. A non-matching, non-separator,
   non-header cell in the region is a **hard failure**, not a skip — so a stray
   placeholder becomes a loud error instead of a phantom component. Without this
   rule pinned down, three plausible parsers (regex scrape / first-cell /
   split-on-`|`) all pass §4.5's mutation check while behaving differently.
6. A directional `assert!` (not `assert_eq!`) over the two set differences
   (`in_source.difference(&in_docs)`, `in_docs.difference(&in_source)`),
   computed ahead of the assertion and named in the panic message via inlined
   `{undocumented:?}` / `{stale:?}` — "in source but not documented: X;
   documented but not in source: Y". A bare two-set dump across six components
   is not actionable. There is no separate formatting helper; the message is
   inlined at the assertion site.

### 4.4 Failability and anti-vacuity

Four ways this test could pass while proving nothing:

- **Empty source set.** A wrong root or moved directory yields zero components
  and compares equal to an empty doc set. Assert the source set is non-empty.
- **Empty documented set.** Missing or renamed markers must fail loudly. Assert
  both markers were found and that the region yielded at least one component.
- **Walk truncation.** Assert a floor on files scanned, well below the repo's
  actual count so it does not couple to workspace size.
- **A dead source scanner.** The first draft copied the sibling test's "assert a
  path outside `crates/` was reached" guard. That guard is **inert here**: the
  path it uses, `tests/runtime-http-conformance/src/lib.rs`, contains zero
  `paigasus::` targets, and `tests/` contributes no components at all today.
  Replace it with a **value** assertion — `scan_targets` over
  `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` must equal
  exactly `{"openai"}` — which proves the scanner reads real source rather than
  returning a constant.

### 4.5 Mutation checks — both directions

Required before the work is called done, and recorded as explicit plan steps. A
sync test that passes in both directions is worthless, and mutating only one side
leaves the other unproven:

1. **Doc side.** Delete one row from the marked region → the test must fail
   naming that component as source-only. Restore → passes.
2. **Source side.** Change one `target: "paigasus::openai::chat"` literal to
   `"paigasus::zzz::chat"` → the test must fail naming `zzz` as source-only.
   Restore → passes.

The first draft specified only (1), under which a `scan_targets` that returned a
hardcoded set would still look proven.

---

## 5. Verification

| Gate | Command | Expectation |
|---|---|---|
| New guard | `cargo test -p paigasus-helikon-workspace-lints` | green, including the new test and the lib unit tests |
| Mutation ×2 | §4.5 by hand | each mutation fails with the right name; both restore green |
| Existing guard | same command | `tracing_target_syntax.rs` still green — `mask_trivia`'s signature changes |
| Book | `mdbook build docs/book` | clean under `warning-policy = "error"` |
| Format | `cargo fmt --all -- --check` | clean |
| Lint | `cargo clippy --workspace --all-features --all-targets -- -D warnings` | clean |
| Workspace | `cargo test --workspace --all-features` | green — this change **adds a test**, so the gate CI actually requires (`test (ubuntu-latest, stable)`) must run |

`mdbook build docs/book` was confirmed clean on `main@a155ee3f` before any edit,
so a failure is attributable to this change.

Note that `mask_trivia` gains a third return value, so the existing caller in
`scan`/`try_scan` changes. That is an internal, unpublished crate — no API
commitment — but it means the existing test is a regression gate here, not a
bystander.

---

## 6. Non-goals and follow-up

**Not in this ticket:**

- Normalizing the 41 untargeted `core`/runtime call sites (D2).
- Resolving `runtime-temporal`'s split personality — 1 targeted site, 3 not.
- Enforcing the naming *convention* on target strings. SMA-543's `scan()` guards
  syntax; D3's guard checks doc-source agreement. Neither is convention
  enforcement, and the ticket says that is a separate decision.
- Adding a row to CONTRIBUTING's semver table for component renames. D1(c) uses
  the existing `BREAKING CHANGE:` mechanism, so nothing is blocked; promoting it
  into CONTRIBUTING is a reasonable follow-up, not a prerequisite.
- Crate `README.md` edits. No crate's public API, usage example, install story,
  or published status changes, so the CLAUDE.md README rule does not fire. A
  conscious call, not a silent skip.

**Follow-up to file** in the `Paigasus Helikon` Linear project, **before the book
page is written**, since §3.3 refers to it in prose: decide whether `core` and
the runtime crates should adopt `paigasus::<component>::<subsystem>` targets. It
should carry D1's two-tier rule, the §1.2 prefix-matching evidence, the §1.3
inventory, and the observation that the five `info_span!` sites in `core` are the
highest-value candidates, being the trace tree the book already teaches.

---

## 7. Commit and PR

Two commits, both semver-neutral — no published crate is touched, so release-plz
attributes no bump:

- `docs(docs): SMA-557 …` for the book page.
- `test(lints): SMA-557 …` for `tests/workspace-lints`.

Both scopes are verified present in `.versionrc:18`'s `scopeRegex`. There is **no
`book` scope** — do not invent one; the allowlist is read from `main` for the PR
title, so a new scope could not be used in this PR's own title anyway.

PR title must satisfy both `pr-title.yml` rules: a full Conventional Commits
`type(scope):` prefix, and a subject starting lowercase after the `SMA-557`
token.
