# SMA-557 — Document the `paigasus::*` tracing target namespace for operators

**Linear:** [SMA-557](https://linear.app/smaschek/issue/SMA-557/document-the-paigasus-tracing-target-namespace-for-operators)
**Branch:** `feature/sma-557-document-the-paigasus-tracing-target-namespace-for-operators`
**Predecessor:** SMA-543 (PR #209, merged). Its own spec, at
`docs/superpowers/specs/2026-08-19-sma-543-tracing-target-design.md:379`, named
this gap as follow-up.

---

## 1. Problem

Provider crates emit `tracing` events on hand-chosen targets in a `paigasus::*`
namespace, independent of the Rust module path the event lives in. That is a
designed-in filtering surface — the whole value of a per-subsystem target is
being able to select it:

```
RUST_LOG='warn,paigasus::openai=debug'
RUST_LOG='off,paigasus::litellm::stream=trace'
```

Nothing tells an operator the surface exists. `paigasus::`, `RUST_LOG` and
`EnvFilter` appear nowhere in `docs/book/src/` and in no crate `README.md`; the
only hits anywhere in the repo are inside `docs/superpowers/plans/`, which are
internal design artifacts, not user documentation. The namespace is therefore
discoverable only by reading provider source.

This is also what let SMA-543 hide: seven call sites emitted on the wrong target
from the day they were written, and no user could have noticed, because no user
was ever told to filter on them.

### 1.1 Inventory, confirmed against source

Measured on `main@a155ee3f` with `grep -rn 'target: *"paigasus::' crates --include='*.rs'`.

**57 call sites** across **20 files**, resolving to **16 distinct target strings**
under **6 components**:

| Component | Crate | Subsystems present | Sites |
|---|---|---|---|
| `paigasus::openai` | `paigasus-helikon-providers-openai` | `translate`, `chat`, `responses` | 13 |
| `paigasus::anthropic` | `paigasus-helikon-providers-anthropic` | `translate`, `stream`, `sse` | 8 |
| `paigasus::bedrock` | `paigasus-helikon-providers-bedrock` | `translate`, `stream`, `builder` | 14 |
| `paigasus::gemini` | `paigasus-helikon-providers-gemini` | `translate`, `sse` | 3 |
| `paigasus::litellm` | `paigasus-helikon-providers-litellm` | `translate`, `stream`, `sse`, `http` | 18 |
| `paigasus::temporal` | `paigasus-helikon-runtime-temporal` | `activities` | 1 |

### 1.2 The ticket's premise is wrong for runtimes

SMA-557 states that "every provider and runtime crate emits `tracing` events on
a deliberate, hand-chosen target". Source does not support the runtime half:

- **All five provider crates are 100% targeted.** Every `tracing` call site in
  them passes an explicit `target:`. There is no exception. (`providers-bedrock/src/family.rs`
  looked like one on a first pass; it contains only a doc comment mentioning
  `` `debug!` ``, no call.)
- **`core` and the runtime crates are 97% untargeted.** Of 37 call sites there,
  **36** pass no target at all and therefore land on the crate's module path —
  `paigasus_helikon_runtime_axum::registry`, `paigasus_helikon_core::session`,
  and so on. The single exception is `paigasus::temporal::activities`, one call
  site in `runtime-temporal/src/activities.rs`, in a crate whose other three
  call sites are untargeted.

Breakdown of the 36: `core` 7, `runtime-agentcore` 11, `runtime-axum` 7,
`runtime-actix` 6, `runtime-temporal` 3, `runtime-tokio` 2.

The operator consequence is the fact this document most needs to state:
**`RUST_LOG='paigasus=debug'` reaches provider events only.** It silently misses
every core and runtime event. An operator who trusts the namespace to mean "all
Helikon events" gets a partial picture with no error and no warning.

### 1.3 The undecided question

The ticket calls out that the stability expectation for these strings is
undecided, and that answering it is the most valuable part of the work. SMA-543
changed four targets *in effect* — they previously resolved to module paths —
which would have been a breaking change had anyone been relying on them. There
is no stability-policy surface in the repo to answer it from:
`docs/book/src/decisions/index.md` says a formal ADR section is "the planned
next step", and `CONTRIBUTING.md` carries only a commit-type → semver table.

---

## 2. Decisions

Three decisions were taken during brainstorming. They are the spec's spine.

### D1 — Two-tier stability

**`paigasus::` and `paigasus::<component>` are a stable filtering surface.**
Renaming or removing a component is a breaking change. **The `::<subsystem>`
leaf is an implementation detail** and may change in any release.

Operator rule stated in the docs: **≤ 2 segments for anything durable**
(alerting, dashboards, saved queries); **3 segments for interactive debugging**.

Rejected alternatives:

- *Fully unstable.* Cheapest and defensible pre-1.0, but it documents a surface
  while telling operators not to depend on it — which leaves the ticket's value
  latent, the exact condition SMA-557 exists to end.
- *Full public contract on all 16 strings.* Best for operators building
  alerting, but freezes 16 strings across 20 files, taxes every provider
  refactor, and retroactively reclassifies SMA-543 as a breaking change.

Two-tier keeps SMA-543 correctly classified as a `fix`: it changed only leaf
resolution, never a component.

### D2 — Docs-only scope

Document the namespace **as it is today**, including the runtime gap, honestly.
Do **not** normalize the 36 untargeted call sites in this ticket.

Rationale: SMA-557 is labelled `area:docs`; normalizing is a workspace-wide
behaviour change across six crates that reads as its own feature ticket, and it
would trigger release bumps on `core` plus four runtimes plus the facade
cascade. The ticket's own "Note on enforcement" already says convention
enforcement is a separate decision.

Consequence for D1: because `paigasus::temporal` is one call site in an
otherwise-untargeted crate, the docs must state that it does **not** yet carry
the component-level guarantee the five provider components do. Promising a
stable `paigasus::temporal` prefix that reaches 1 of 4 temporal events would be
a doc that misleads.

### D3 — Guard the component list, not the target list

A test asserts that the set of `paigasus::<component>` prefixes in source equals
the set documented in the book.

It fires on a new provider being added without a doc update, and on a component
being renamed — which under D1 is exactly the breaking change a human must
consciously approve. It is deliberately **silent on leaf renames**, because D1
declares those free; guarding the full 16-string list would redden CI on
legitimate refactors and pressure the docs toward freezing what D1 called free.

Relying on convention alone was rejected: CLAUDE.md already requires updating
the book in the same PR as any user-facing change, and that is precisely the
mechanism that failed here — 13 of 17 book pages sat as stubs through all of
Stage 1 until the SMA-423 catch-up.

---

## 3. The book section

One new subsection in `docs/book/src/concepts/observability-evaluation.md`,
titled **"Filtering by target"**, placed under `## Observability` after
`### Exporting to an OTel backend` and before `## Evaluation`. Rationale for the
position: the existing observability prose moves from what is emitted
(`TracerHandle`) to where it goes (OTel export); selecting a subset is the
natural third beat, and it stays inside the observability half of the chapter.

Four parts, in order.

### 3.1 The convention

`paigasus::<component>::<subsystem>`. State that the target is hand-chosen and
independent of the Rust module path the event lives in — that distinction is
what makes the namespace worth documenting at all, and it is not obvious to a
reader who assumes `tracing` targets default to module paths (they do, which is
the point).

### 3.2 The component table

Six rows: component, crate, and subsystems **seen today**. The subsystem column
is explicitly labelled as examples, not a frozen list, so it cannot be read as
contradicting D1.

The table sits inside the marked region described in §4.1.

### 3.3 The caveat

Stated plainly, not buried:

- `paigasus::*` is today a **provider** namespace.
- `core` and the runtime crates emit on **module-path** targets instead
  (`paigasus_helikon_core::session`, `paigasus_helikon_runtime_axum::registry`,
  …), so `RUST_LOG='paigasus=debug'` reaches provider events only.
- `paigasus::temporal` is a single call site in an otherwise-untargeted crate.
  It is listed for completeness and does **not** carry the component-level
  guarantee.
- A link to the follow-up issue (§6) so a reader can see the gap is known and
  tracked rather than accidental.

### 3.4 The stability statement

D1, in prose, with the ≤ 2 / 3 segment operator rule.

### 3.5 Recipes

Three `RUST_LOG` examples, each earning its place by showing something the
others do not:

1. **One provider, verbose** — `warn,paigasus::openai=debug`. The two-segment
   durable form.
2. **One subsystem only** — `off,paigasus::litellm::stream=trace`. The
   three-segment debugging form, annotated as leaf-level and therefore not
   durable.
3. **Full coverage across both namespaces** —
   `warn,paigasus=debug,paigasus_helikon_core=debug,paigasus_helikon_runtime_axum=debug`.
   This one exists because it is the only way to actually get everything, and
   without it §3.3's caveat would name a problem the docs never solve.

All three must be real: every target named must exist in source at the time of
writing.

---

## 4. The drift guard

### 4.1 Marked region

The table is wrapped in HTML comments:

```
<!-- tracing-components:start — keep in sync; asserted by
     tests/workspace-lints/tests/tracing_target_docs.rs -->
… table …
<!-- tracing-components:end -->
```

The comment names the test file, so someone editing or deleting the markers
learns what depends on them. Markers were chosen over parsing the table's header
row (couples the test to column layout) and over scraping every `paigasus::x`
occurrence on the page (a placeholder such as `paigasus::<provider>` in a recipe
would break it, and §3.3 deliberately mentions `paigasus_helikon_*` strings
nearby).

### 4.2 Source side — `scan_targets`

New public function in `tests/workspace-lints/src/lib.rs`:

```rust
pub fn scan_targets(src: &str) -> BTreeSet<String>
```

Returns the distinct `<component>` segments from every `target: "paigasus::…"`
literal in one file's source. `BTreeSet` for deterministic assertion messages.

It lives in the lib rather than the test for symmetry with the existing
`scan`/`try_scan`, so the extraction rule is documented and unit-testable
independently of the workspace walk. The crate carries `[lints] workspace = true`,
so `missing_docs` applies — the function and any new public type need `///` docs.

Extraction rule: match `target:` followed by a string literal beginning
`paigasus::`; take the segment between the first and second `::`. A literal with
no second `::` (a bare `paigasus::foo`) yields `foo`; this shape does not occur
today but must not panic.

Unit tests in the lib cover: a normal two-segment target, a bare one-segment
target, a non-`paigasus` target being ignored, `target =` (the SMA-543 form)
being ignored since it is not a target at all, and a `paigasus::` mention inside
a comment or doc comment. The last matters because §1.2's `family.rs` false
positive proves comment text does reach naive matchers.

### 4.3 Doc side and the assertion

The test:

1. Derives the repo root from `CARGO_MANIFEST_DIR`, not the process CWD, matching
   `tracing_target_syntax.rs`.
2. Walks **`crates/` and `tests/` only** — deliberately *not* the repo root.
   This is the existing test's reasoning and it applies unchanged:
   `.claude/worktrees/` can hold full checkouts of other branches, and scanning
   those would make the verdict depend on which unrelated worktrees a developer
   happens to have. That is not hypothetical — this ticket was itself developed
   in `.claude/worktrees/sma-557-tracing-targets`.
3. Reuses the symlink-safe `collect_rs` walker shape (`symlink_metadata`, not
   `is_dir`, so a symlink to an ancestor cannot recurse unboundedly).
4. Unions `scan_targets` over every file into the source set.
5. Parses the marked region of the book page into the documented set.
6. `assert_eq!`, with a message naming **which side is missing what** — a bare
   set-inequality dump across six components is not actionable.

### 4.4 Failability and anti-vacuity

Three ways this test could pass while proving nothing, each guarded:

- **Empty source set.** A wrong root or a moved directory yields zero components
  and would compare equal to an empty doc set. Assert the source set is
  non-empty before comparing.
- **Empty documented set.** Missing or renamed markers must fail loudly, not
  silently yield an empty set. Assert both markers were found, and that the
  region between them parsed at least one component.
- **Walk truncation.** Reuse the existing test's tripwire: assert a floor on
  files scanned, and assert that specific known paths were reached. At least one
  must sit outside `crates/` to prove the non-`crates/` root is live.

### 4.5 Mutation check

Required before the work is called done, and recorded in the plan as an explicit
step: delete one row from the marked region, confirm the test **fails**, restore
it, confirm it passes. A sync test that passes in both directions is worthless —
the same reasoning SMA-543's spec applied to its own guard at §4.5 there.

---

## 5. Verification

| Gate | Command | Expectation |
|---|---|---|
| New guard | `cargo test -p paigasus-helikon-workspace-lints` | green, including the new test |
| Mutation | §4.5 by hand | fails on a deleted row, passes when restored |
| Book | `mdbook build docs/book` | clean under `warning-policy = "error"` |
| Format | `cargo fmt --all -- --check` | clean |
| Lint | `cargo clippy --workspace --all-features --all-targets -- -D warnings` | clean |

`mdbook build docs/book` was confirmed clean on the branch point before any
edit, so a failure is attributable to this change.

Full `cargo test --workspace --all-features` is not a gate for this change: no
published crate is touched. CI runs it regardless.

---

## 6. Non-goals and follow-up

**Not in this ticket:**

- Normalizing the 36 untargeted `core`/runtime call sites (D2).
- Resolving `runtime-temporal`'s split personality — 1 targeted call site, 3 not.
- Enforcing the naming *convention* on target strings. SMA-543's `scan()` guards
  syntax only, deliberately; D3's guard checks doc-source agreement, not that a
  target matches a naming rule. Neither is convention enforcement, and the
  ticket says that is a separate decision.
- Crate `README.md` edits. No crate's public API, usage example, install story,
  or published status changes, so the CLAUDE.md README rule does not fire. This
  is a conscious call, not a silent skip.

**Follow-up to file** in the `Paigasus Helikon` Linear project: decide whether
`core` and the runtime crates should adopt `paigasus::<component>::<subsystem>`
targets, making `paigasus=debug` mean "all Helikon events". It should carry the
D1 two-tier rule, the §1.2 inventory, and the observation that adopting the
namespace in `core` would make the first segment reachable from every crate that
depends on it. The book caveat (§3.3) links to it.

---

## 7. Commit and PR

Single `docs` commit type — no published crate is touched, so release-plz
attributes no bump. Scope must come from the `.versionrc` allowlist; verify
before committing rather than assuming.

PR title must satisfy both `pr-title.yml` rules: a full Conventional Commits
`type(scope):` prefix, and a subject starting lowercase after the `SMA-557`
token.
