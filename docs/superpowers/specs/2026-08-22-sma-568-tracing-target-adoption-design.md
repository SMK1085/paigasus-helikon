# SMA-568 — Adopt `paigasus::*` tracing targets in `core` and the runtime crates

Status: proposed
Ticket: [SMA-568](https://linear.app/smaschek/issue/SMA-568)
Predecessor: [SMA-557](https://linear.app/smaschek/issue/SMA-557) —
`docs/superpowers/specs/2026-08-20-sma-557-tracing-target-namespace-design.md`

SMA-557 documented the `paigasus::<component>::<subsystem>` tracing namespace
and deliberately changed no call site, filing the adoption question as
follow-up. This is that follow-up. It answers **yes**, names the components,
resolves `runtime-temporal`'s split personality, and adds the enforcement that
keeps the answer true.

---

## 1. Problem

### 1.1 Inventory, re-measured

Measured against `main@b6679108` by walking every `tracing` macro invocation
under `crates/*/src/**.rs` with a delimiter-aware parser (not a line regex —
provider call sites put `target:` on the line *after* the macro, which a naive
per-line grep misclassifies as untargeted). Comment-only lines excluded.

| Crate | Targeted | Untargeted |
|---|---:|---:|
| `providers-openai` | 12 | 0 |
| `providers-anthropic` | 8 | 0 |
| `providers-bedrock` | 14 | 0 |
| `providers-gemini` | 3 | 0 |
| `providers-litellm` | 18 | 0 |
| `core` | 0 | 12 |
| `runtime-tokio` | 0 | 2 |
| `runtime-temporal` | 1 | 3 |
| `runtime-axum` | 0 | 7 |
| `runtime-actix` | 0 | 6 |
| `runtime-agentcore` | 0 | 11 |
| **Total** | **56** | **41** |

97 sites. This reproduces SMA-557's numbers exactly.

`mcp`, `tools`, `sessions-*`, `evals`, `cli`, `macros` and the facade contain
zero `tracing` call sites. There is no `#[tracing::instrument]` anywhere under
`crates/`. Four `core` modules import `tracing::Instrument` to attach an
*existing* span to a future (`agent.rs`, `workflow.rs`, `graph.rs`, `swarm.rs`);
that creates no new target.

**Every invocation in the workspace is written `tracing::<macro>!`** — fully
qualified, no bare `warn!` reached through `use tracing::warn;`, no aliases.
Verified by grep for the bare forms, which returns nothing under `crates/*/src`.
This matters for §4: the coverage guard must still *handle* the bare and alias
forms, because the existing walker does and a future contributor may write one,
but nothing in the workspace exercises them today.

The sole non-`src` user of a `tracing` macro anywhere under `crates/` is
`crates/paigasus-helikon-providers-anthropic/tests/live.rs`. §4.2 scopes the
guard so this stays out of it.

### 1.2 The split this creates

Two namespaces describe one SDK:

- `paigasus::<component>::<subsystem>` — hand-chosen, 56 sites, all five
  providers plus one stray site in `runtime-temporal`.
- `paigasus_helikon_*::…` — Rust module paths, the `tracing` default, 41 sites,
  all of `core` and the runtimes.

The consequences the ticket names:

1. **The trace tree is on the wrong side.** The five `tracing::info_span!` sites
   in `core` are the only span macros in the workspace — four in `agent.rs`, one
   in `workflow.rs` — and they produce exactly
   the `agent.run` → `agent.turn` → `gen_ai.chat` / `tool.execute` tree the book
   teaches in `docs/book/src/concepts/observability-evaluation.md`. An operator
   selects them with `paigasus_helikon_core`, a different namespace from
   everything else that chapter documents.
2. **`paigasus::` does not mean "Helikon".** It means "the providers, and one
   line in the Temporal runtime".
3. **`runtime-temporal` is incoherent** — 1 site in the curated namespace, 3 on
   module paths, in the same crate.

### 1.3 The constraint every answer must respect

`EnvFilter` matches a directive against a target by **raw string prefix, not by
`::` segment**. SMA-557 verified this by execution:

- `paigasus=debug` matches `paigasus_helikon_core::session` **and**
  `paigasus::openai::chat`.
- `paigasus::=debug` (trailing `::`) selects the curated namespace only.
- `paigasus::openai=debug` would also match a hypothetical
  `paigasus::openai_compat::chat`.

§7.1 promotes this from a hand-verified REPL fact to a regression test.

### 1.4 What SMA-557 already decided — not relitigated here

- **D1 two-tier stability.** `paigasus::` and `paigasus::<component>` are
  stable for components marked *stable*; renaming one is a breaking change made
  through a `BREAKING CHANGE:` footer. The `::<subsystem>` leaf is an
  implementation detail, free to change in any release.
- **D1(b) no-prefix-collision**, namespace-wide, binding *provisional*
  components too, because a collision silently widens an already-deployed
  filter.
- **D3 doc-drift guard.** `tests/workspace-lints/tests/tracing_target_docs.rs`
  asserts the set of components in source equals the set in the book's marked
  region, and rejects prefix collisions. It ignores the Status column.
- The D1 guarantee **begins with the SMA-557 document** and is not retroactive.

### 1.5 What SMA-557 explicitly left open — decided here

> "Nothing asserts that a target string matches the
> `paigasus::<component>::<subsystem>` shape, that a subsystem is named
> sensibly, or that a component is spelled a particular way — the ticket says
> that remains a separate decision."

§4 is that decision.

---

## 2. Decisions

### D1 — Adopt, fully

**Every `tracing` call site under `crates/*/src` carries an explicit
`target: "paigasus::<component>::<subsystem>"`.** All 41 untargeted sites are
converted. After this ticket, `paigasus::` means *all Helikon events*, with no
exception to document.

`runtime-temporal`'s split personality resolves **by addition** — its other
three sites gain targets — rather than by deleting the one site that already had
a target. That site survives; only its component segment is renamed, from
`paigasus::temporal::activities` to `paigasus::runtime_temporal::activities`
(D2).

Rejected alternatives:

- ***Core only,*** leaving the runtimes on module paths and deleting
  `temporal`'s lone site. Smaller blast radius, and defensible on the grounds
  that runtimes are adapters rather than the SDK proper. Rejected because
  `paigasus::` still would not mean "everything", so the book would still carry
  a "what is not in this namespace" section — the exact shape of the problem
  this ticket exists to remove, merely smaller.
- ***Decline,*** keeping module paths and removing `temporal`'s site.
  Module paths never drift, need no contract, and need no guard. Rejected
  because it leaves the highest-value events — the trace tree the observability
  chapter is *about* — selectable only through a namespace that chapter
  otherwise never mentions.
- ***Adopt everywhere except `core`'s five `info_span!` sites,*** so the
  documented `paigasus_helikon_core=debug` recipe keeps working. Rejected: it
  buys backwards compatibility by putting a deliberate hole in the middle of the
  namespace, and the hole is precisely the part operators most want to select.

### D2 — Component names: bare `core`, `runtime_`-prefixed adapters

Eleven components, **all marked `stable`**:

| Component | Crate | Change |
|---|---|---|
| `openai` | `paigasus-helikon-providers-openai` | unchanged |
| `anthropic` | `paigasus-helikon-providers-anthropic` | unchanged |
| `bedrock` | `paigasus-helikon-providers-bedrock` | unchanged |
| `gemini` | `paigasus-helikon-providers-gemini` | unchanged |
| `litellm` | `paigasus-helikon-providers-litellm` | unchanged |
| `core` | `paigasus-helikon-core` | **new** |
| `runtime_tokio` | `paigasus-helikon-runtime-tokio` | **new** |
| `runtime_axum` | `paigasus-helikon-runtime-axum` | **new** |
| `runtime_actix` | `paigasus-helikon-runtime-actix` | **new** |
| `runtime_agentcore` | `paigasus-helikon-runtime-agentcore` | **new** |
| `runtime_temporal` | `paigasus-helikon-runtime-temporal` | **renamed** from `temporal`; `provisional` → `stable` |

**Prefix-collision check, exhaustive over the eleven.** No name is a prefix of
any other in either direction. The near misses:

- `runtime_axum` / `runtime_actix` / `runtime_agentcore` share the prefix
  `runtime_a`, which is not itself a component. None is a prefix of another:
  all three diverge at 0-based index 9, the character after `runtime_a`.
- `runtime_temporal` / `runtime_tokio` share `runtime_t` and likewise diverge at
  0-based index 9.
- `core` is not a prefix of, nor prefixed by, anything — note in particular
  that it does *not* collide with `runtime_agentcore`, since prefix matching
  anchors at the start of the string.

**Renaming `temporal` → `runtime_temporal` is permitted without a breaking
change** under SMA-557 D1: `temporal` is marked *provisional*, which "carries
none of this" and "may be renamed or removed in any release". It is nonetheless
covered by the announcement in D4, because provisional-ness is a licence, not a
reason to be quiet.

Rejected alternatives:

- ***Bare crate names*** (`core`, `tokio`, `axum`, `actix`, `agentcore`,
  `temporal`). Matches the provider precedent most closely — component = crate,
  one word. Rejected on two counts. First, `agentcore` permanently burns
  `agent` as a future component name, since `agent` would be a prefix of it, and
  `agent` is the most natural name for the loop that is the SDK's centrepiece.
  Second, it forgoes the group selector below.
- ***A single `runtime` component*** with the backend as the subsystem
  (`paigasus::runtime::axum`). Only two new components, the smallest contract
  surface. Rejected because per-runtime selection would drop to a three-segment
  form, which D1 declares free to change in any release — so an operator
  running two backends could not build a durable "all axum events" filter, which
  is a realistic and reasonable thing to want.

### D3 — `paigasus::runtime` is a documented group selector

Because matching is raw-prefix, `paigasus::runtime` selects every runtime
adapter at once. D2's naming makes this true; **this decision makes it
promised**, and documents it in the book alongside the other forms.

**The promise is "includes every runtime adapter", not "matches exactly
those".** The distinction is not pedantry, and an earlier draft of this spec got
it wrong by claiming the guarantee "costs nothing to protect":

- *Under-matching is mechanically impossible.* A component named exactly
  `runtime` would be a prefix of all five `runtime_*` names, and SMA-557's D3
  guard (`tests/workspace-lints/tests/tracing_target_docs.rs:192-206`) rejects
  any component that is a prefix of another. So `paigasus::runtime` cannot stop
  reaching an adapter.
- *Over-matching is **not** guarded.* A future component named `runtimes`,
  `runtime2` or `runtimeconfig` is neither a prefix of nor prefixed by any
  `runtime_*` name, so the guard stays green while `paigasus::runtime` silently
  widens to include it.

D6 closes most of that gap incidentally — a component must correspond to the
crate it is emitted from, and no crate is named `paigasus-helikon-runtimes` —
but the residual is real and the book must state the promise in the inclusive
form rather than the exact one.

It is a **1.5-segment form** — `paigasus::runtime` is a prefix of five
components rather than being one — so the book must present it as its own row
rather than folding it into D1's segment-count table, which counts whole
components.

### D4 — Announce in the CHANGELOG and the book, **without** a breaking marker

**This decision reverses an earlier one in this spec, on evidence that changed
the price.** The reversal is flagged here rather than silently applied.

Strictly, nothing under contract breaks. Module-path targets were never
promised, and `paigasus::temporal` is *provisional*. But the SMA-557 book page
publishes `RUST_LOG='warn,paigasus_helikon_core=debug'` as *the* recipe for the
agent trace tree, and this ticket stops it matching one release later. Operators
must be told. The question is with how big a hammer.

**What a `!` / `BREAKING CHANGE:` marker would actually cost.**
`paigasus-helikon-core` is at **`0.5.18`** (root `Cargo.toml:151`). On a 0.x
crate release-plz maps a breaking marker to a **minor** bump — `0.5.18 → 0.6.0`
— and under Cargo's caret rules `^0.5.18` does **not** admit `0.6.0`. Every one
of the ~19 sibling crates pinning core through `[workspace.dependencies]` must
therefore be re-pinned and re-released. The runtimes compound it:
`runtime-tokio 0.1.21 → 0.2.0`, `runtime-axum 0.2.3 → 0.3.0`,
`runtime-actix 0.2.3 → 0.3.0`, `runtime-temporal 0.4.3 → 0.5.0`,
`runtime-agentcore 0.2.6 → 0.3.0`. The result is a semver-incompatible release
of essentially the entire SDK — `-mcp`, `-tools`, `-evals`, the three
`-sessions-*`, `-cli`, all five providers and the facade — triggered by a change
that alters no Rust API and breaks no consumer's build.

An earlier draft of this spec priced this as "six minor bumps plus the facade
cascade". That was wrong, and it was the basis on which the breaking marker was
originally chosen.

**And the argument for the marker does not survive either.** That draft rejected
the non-breaking option because "an operator who reads CHANGELOGs and not the
book gets no warning at all". False: an ordinary `feat(core):` commit lands in
the CHANGELOG's **Features** section with its subject intact. The marker does
not buy CHANGELOG *visibility* — it buys a version-number *signal*, at the price
of forcing every downstream consumer through an incompatible upgrade to receive
news about log filtering.

`!` on a 0.x crate reads as "your code may not compile". Here everyone's code
compiles unchanged. Spending the strongest signal available on a non-API change
is how a signal stops being read.

**Decision.** Ordinary `feat(core): …` / `feat(runtime): …` commits — patch
bumps, no caret break — carrying the change prominently in each CHANGELOG, plus
the §5.4 migration section in the book, written to be found by someone whose
filters went quiet.

Rejected alternative — ***`!` plus `BREAKING CHANGE:` footer***, repriced above.
It remains defensible for one reason: an operator who upgrades on version
numbers alone, reading neither CHANGELOG nor book, is warned by nothing else.
Rejected because the population it protects is small, the cost is a
workspace-wide incompatible release, and §5.4 plus a CHANGELOG line reaches
everyone who reads anything at all.

Rejected alternative — ***ship the six new components `provisional`*** and
promote later. Maximally cautious. Rejected because it withholds the exact thing
the ticket is meant to deliver — a durable filtering surface — and manufactures
another follow-up to close.

### D5 — Coverage and shape enforcement

A new test asserts, over `crates/*/src/**.rs`:

- **Coverage** — every `tracing` macro invocation carries an explicit
  `target:`.
- **Shape** — every target is exactly `paigasus::<component>::<subsystem>`,
  three `[a-z0-9_]+` segments.

Rationale: without it, the namespace decays on the first bare `tracing::warn!`
a contributor adds. That event lands on a module path and escapes every
`paigasus::` filter — silently, with the build green — which is the precise
failure mode this ticket exists to end, reintroduced one commit at a time.
Coverage is what replaces vigilance with a mechanism: CI fails when a covered
invocation loses its target, instead of the loss going unnoticed until an
operator's filter comes back empty.

Note the bounded phrasing. This is **not** a claim that the namespace is
complete by construction — §5.1 forbids the book from saying that, and the same
discipline binds this document. The guard sees `tracing` macro invocations under
`crates/*/src`; D7 bans the one construct it cannot see, and §4.3 leaves an
escape hatch a future contributor may legitimately use.

It is satisfiable today: the six crates with zero call sites impose no work, and
the 97 sites are all converted by this ticket.

Relying on convention alone was rejected on precedent. CLAUDE.md already
requires updating the book in the same PR as any user-facing change, and that is
exactly the mechanism that failed for the book itself — 13 of 17 pages sat as
stubs through all of Stage 1 until the SMA-423 catch-up.

Rejected alternative: ***shape guard only***, validating existing `paigasus::`
strings without requiring coverage. Catches typos; does not stop a new
untargeted site from escaping. Half the value for nearly all the work, since the
scanner and test harness are the same either way.

### D6 — A component must match the crate it is emitted from

Assertion 4 of §4.4. A hard-coded crate-directory → component map lives in the
guard; a target emitted from `crates/<crate>/src` whose component is not
`<crate>`'s registered component fails.

Nothing else enforces this. The shape guard accepts any well-formed component,
and the docs guard accepts any *documented* one — so without D6, a file in
`runtime-axum` could emit `paigasus::core::agent` on a fully green build. That
would make `paigasus::runtime_axum` miss events and `paigasus::runtime` miss a
whole adapter, silently, which is the exact failure mode D2 and D3 exist to
prevent. The book's component table also carries a **Crate** column that would
quietly become a lie.

The risk is not hypothetical in kind. `providers-litellm` carries a bespoke
`#[cfg(test)] mod tracing_target_tests`
(`crates/paigasus-helikon-providers-litellm/src/translate/request.rs:490-520`)
whose module doc says in as many words that the existing guards would leave "a
copy-paste regression that reinstates `target: "paigasus::openai::translate"`
inside this crate green on every gate". Someone already found this gap worth a
hand-written test in one crate; D6 closes it in all of them.

**There is no live counterexample to allowlist.** That litellm string appears
only inside a doc comment — `mask_trivia` blanks comments before any scan, so it
contributes nothing to any guard, and no crate in the workspace emits another
crate's component today. D6 is green on arrival.

The naming rule the map encodes, stated for future crates: **strip
`paigasus-helikon-`, then strip a leading `providers-`, then replace `-` with
`_`.** That yields every existing name — `openai`, `litellm`, `core`,
`runtime_axum` — and predicts nine of the ten not yet emitting anything:
`macros`, `mcp`, `tools`, `evals`, `cli`, `sessions_sqlite`,
`sessions_postgres`, `sessions_redis`, `sessions_testkit`. None collides with
an existing name or with another prediction. The providers' bare form is a
historical exception preserved because those names are the user-facing vendor
names; everything else uses its full suffix.

The tenth, `facade` (for `paigasus-helikon`), is an exception to the rule
itself rather than a tenth prediction from it: stripping
`paigasus-helikon-` from `paigasus-helikon` leaves nothing, so the rule has no
suffix left to derive from, and `facade` is reserved by fiat instead.

These ten are **reserved, not documented**: the docs guard asserts source and
book agree, so a book row for a component nothing emits would redden CI. The
names live in this spec and in book prose *outside* the marked region, so the
first person to add a log line to `tools` has an answer instead of a 2am
decision that creates a stable contract by accident.

**`paigasus-helikon-cli` is included deliberately.** It is a binary crate with
`missing_docs = "allow"` and no stability guarantee, so putting it inside a
namespace whose two-segment form is a stable contract is arguably odd. It is
still the right call: the guard cannot exempt one crate without a carve-out that
would also exempt it from coverage, and an operator debugging `helikon eval run`
wants the same filter grammar as everywhere else. The contract binds the
*target string*, not the crate's API.

### D7 — `#[tracing::instrument]` is rejected outright under `crates/*/src`

Assertion 5 of §4.4. The attribute is banned, not merely required to carry a
target.

**The guard structurally cannot see it otherwise.** `try_scan` and
`scan_invocations` key on an identifier followed by `!` and a delimiter
(`tests/workspace-lints/src/lib.rs:129-158`); an attribute has no `!`, so an
instrumented function is invisible to every assertion above and lands on its
module path with the build green. Teaching the walker to parse attributes is a
much larger change than rejecting a token.

§8 already makes *adding* `#[tracing::instrument]` a non-goal, and there is none
in the workspace today, so the ban costs nothing now. It converts a silent
namespace hole into a loud one, and the failure message names the follow-up
decision that would be needed to lift it.

Lifting it later is a deliberate act: teach the guard to require
`target = "paigasus::…"` on the attribute — note `=`, not `:`, since attribute
syntax differs from macro syntax, which is itself a trap worth the ban.

(An adversarial review of this spec claimed `paigasus-helikon-macros` already
forwards `#[tracing::instrument]` to a generated helper. It does not: the
reference at `crates/paigasus-helikon-macros/src/signature.rs:210` is a doc
comment explaining that *user-written* attributes are forwarded. Whatever a
downstream user puts on their own `#[tool]` function is outside `crates/*/src`
and outside this guard.)

---

## 3. The target map

The component tier is stable (D2); **the subsystem tier below is not** — D1
declares leaves free to change, so this table is the state at merge, not a
contract. Subsystems mirror the module the site lives in, except where a module
name would be less informative than the concern it serves.

### 3.1 `core` — 12 sites

| Target | File:line | Macro |
|---|---|---|
| `paigasus::core::agent` | `agent.rs:547` | `info_span!("tool.execute")` |
| `paigasus::core::agent` | `agent.rs:734` | `info_span!("agent.run")` |
| `paigasus::core::agent` | `agent.rs:858` | `info_span!("agent.turn")` |
| `paigasus::core::agent` | `agent.rs:918` | `info_span!("gen_ai.chat")` |
| `paigasus::core::workflow` | `workflow.rs:51` | `info_span!("agent.run")` |
| `paigasus::core::session` | `session.rs:368` | `warn!` |
| `paigasus::core::session` | `session.rs:373` | `warn!` |
| `paigasus::core::session` | `session.rs:459` | `debug!` |
| `paigasus::core::compaction` | `compacting_session.rs:204` | `warn!` |
| `paigasus::core::compaction` | `compacting_session.rs:210` | `warn!` |
| `paigasus::core::permissions` | `path_match.rs:144` | `warn!` |
| `paigasus::core::permissions` | `path_match.rs:150` | `warn!` |

`path_match.rs` is documented in its own module header as "lexical path matching
for permission path-rules", so `permissions` names the concern more usefully than
`path_match` names the file.

**A side effect worth stating.** The book currently warns that filtering on
`paigasus_helikon_core::agent` silently misses the multi-agent run's top-level
span, which is raised in `workflow.rs`, and tells the reader to use
`paigasus_helikon_core` to catch both. Under this map the correct filter is
`paigasus::core` — which is also the *stable two-segment form* D1 recommends for
anything durable. The right answer and the durable answer become the same
string.

### 3.2 `runtime_tokio` — 2 sites

| Target | File:line |
|---|---|
| `paigasus::runtime_tokio::runner` | `lib.rs:108` |
| `paigasus::runtime_tokio::retry` | `retry.rs:236` |

### 3.3 `runtime_temporal` — 4 sites

| Target | File:line | Note |
|---|---|---|
| `paigasus::runtime_temporal::activities` | `activities.rs:351` | existing site; component segment renamed |
| `paigasus::runtime_temporal::activity_input` | `activity_input.rs:106` | new |
| `paigasus::runtime_temporal::worker` | `worker.rs:489` | new |
| `paigasus::runtime_temporal::runner` | `runner.rs:356` | new |

### 3.4 `runtime_axum` — 7 sites

| Target | File:line |
|---|---|
| `paigasus::runtime_axum::registry` | `registry.rs:80`, `:187`, `:308`, `:392` |
| `paigasus::runtime_axum::error` | `error.rs:116`, `:125` |
| `paigasus::runtime_axum::runs` | `handlers/runs.rs:349` |

### 3.5 `runtime_actix` — 6 sites

| Target | File:line |
|---|---|
| `paigasus::runtime_actix::registry` | `registry.rs:79`, `:187`, `:308` |
| `paigasus::runtime_actix::error` | `error.rs:112`, `:121` |
| `paigasus::runtime_actix::runs` | `handlers/runs.rs:399` |

### 3.6 `runtime_agentcore` — 11 sites

| Target | File:line |
|---|---|
| `paigasus::runtime_agentcore::server` | `server.rs:405` |
| `paigasus::runtime_agentcore::invoke` | `invoke.rs:247`, `:258` |
| `paigasus::runtime_agentcore::mcp` | `mcp.rs:144` |
| `paigasus::runtime_agentcore::a2a` | `a2a/mod.rs:59`, `a2a/rpc.rs:203`, `:610`, `:623`, `:657`, `a2a/store.rs:331` |
| `paigasus::runtime_agentcore::agui` | `agui/mod.rs:68` |

Six sites under `a2a/` share one subsystem rather than splitting into
`a2a_rpc` / `a2a_store`: the operator question is "what is the A2A surface
doing", and a leaf split would not answer a different one.

### 3.7 Line numbers are a snapshot

The line numbers above are against `main@b6679108` and will drift as edits land
within a file. They are navigation aids for the implementer, not assertions —
nothing in §4 or §7 asserts a line number.

---

## 4. The workspace lint

New test file: `tests/workspace-lints/tests/tracing_target_coverage.rs`, carrying D5, D6 and D7, in the
existing internal `paigasus-helikon-workspace-lints` member (`0.0.0`,
`publish = false`).

### 4.1 Built on the existing walker, not a new regex

`tests/workspace-lints/src/lib.rs` already contains a delimiter-aware argument
walker built for SMA-543: it masks comments and string literals before scanning,
recognises all three macro delimiters (`(`, `[`, `{`), matches a macro by its
**final path segment** so `tracing::warn!`, `crate::obs::warn!` and a bare
`warn!` reached through `use tracing::warn;` all count, resolves
`use tracing::warn as w;` aliases, and surfaces a delimiter mismatch as a
`MismatchedDelimiter` value the caller can attribute to a file.

Reimplementing any of that with a line regex would reproduce the exact defect
§1.1 describes — provider sites put `target:` on a later line, so a per-line
check reads a correctly-targeted site as untargeted.

**One** new public function in that crate, reusing the same masking and walking.
It reports every invocation and classifies its `target:` argument into three
states, because all of §4.4's assertions are questions about that
classification and a pair of narrower functions cannot answer them all:

```rust
/// How a `tracing` macro invocation declared its target.
pub enum TargetArg {
    /// No `target:` argument at all — the event lands on its module path.
    Absent,
    /// `target:` with a non-literal value: a `const`, a path, an expression.
    NonLiteral,
    /// `target: "…"` — the literal's content, delimiters stripped.
    Literal(String),
}

/// One `tracing` macro invocation found in the source.
pub struct Invocation {
    /// 1-based line of the macro name.
    pub line: usize,
    /// Macro name, unqualified (e.g. `warn` for `tracing::warn!`).
    pub macro_name: String,
    /// The invocation's `target:` argument, if any.
    pub target: TargetArg,
}

/// Every `tracing` macro invocation in `src`, with its target classified.
pub fn scan_invocations(src: &str) -> Result<Vec<Invocation>, MismatchedDelimiter>;
```

Returning `Result` rather than panicking matches `try_scan`'s precedent, so a
walk over many files can name the file it was reading.

**`NonLiteral` must be its own state, not folded into either neighbour.** A
computed `target:` satisfies coverage but cannot be shape-checked, so collapsing
it into `Absent` would report a false coverage failure, and treating it as
acceptable would leave a way to route events out of the namespace invisibly.
§4.4 makes it a hard failure in its own right.

An earlier draft specified two narrower functions — one returning untargeted
sites, one returning literal targets — plus an assertion that "the number of
`target:` arguments the walker sees must equal the number of string literals
returned". That assertion is not derivable from either return value. The
`TargetArg` classification is what makes it expressible at all.

The existing `scan_targets` (used by `tracing_target_docs.rs`) is **not** a
usable base. It searches the masked buffer for the bare needle `target:` and
takes the next adjacent string literal, without establishing that the site is a
`tracing` macro invocation at all. That looseness is correct for the question it
answers (*which components exist in source*, for the doc-drift assertion) and
wrong for the question here, which is about invocations — see §4.2's
`command_match.rs` case for a live example a needle-based implementation would
trip over. It is left untouched, and `scan_invocations` must not be folded into
it.

### 4.2 Scope

Walked: files matching `crates/*/src/**/*.rs`. The walk is rooted at
`<repo>/crates` and **filters on the `src/` path segment** — rooting at
`crates/` and taking every `.rs` file would pull in
`crates/paigasus-helikon-providers-anthropic/tests/live.rs`, which the exclusion
list below deliberately excludes. 219 files as measured against `main@b6679108`.

Not walked:

- `crates/*/tests/`, `examples/`, `benches/` — a test may legitimately assert on
  an arbitrary target string, and
  `crates/paigasus-helikon-providers-anthropic/tests/live.rs` uses a `tracing`
  macro today. Requiring the namespace of test scaffolding would be enforcement
  for its own sake.
- `tests/` at the repo root — the workspace-lints member itself, which writes
  `tracing` macros inside string fixtures.
- `.claude/worktrees/` — never reached, because the walk is rooted at
  `<repo>/crates` rather than at the repo root. This mirrors the reasoning in
  the two existing guards: a developer's unrelated worktrees must not change a
  test's verdict.

`#[cfg(test)]` modules *inside* `src/` are walked, with no carve-out — one
would need its own `cfg` detection logic, and a test module is exactly where an
untargeted convenience log gets written. All 97 sites are targeted by this
ticket wherever they sit, so the rule costs nothing today.

**But `src/` is not free of `target:` tokens that are not tracing arguments.**
`crates/paigasus-helikon-core/src/command_match.rs:436`, `:445` and `:454`
contain `Redirect { … target: "/etc/passwd".into() }` — a struct-field
initializer named `target` whose value is an adjacent string literal, inside a
`#[cfg(test)]` module. That is exactly the shape a needle-based scanner mistakes
for a tracing argument, and it would fail the shape assertion on day one.
Non-literal `target:` fields also appear in
`core/src/{agent,handoff,loop_state,permission,swarm}.rs`,
`tools/src/web/fetch.rs`, `tools/src/net/proxy.rs` and
`runtime-temporal/src/error.rs`.

`scan_invocations` is immune by construction — it inspects arguments only
*inside* a recognised macro invocation. §4.5 makes that a required regression
fixture rather than a hoped-for property.

Relatedly, the doc comment at `tests/workspace-lints/src/lib.rs:190-192` states
that no such site exists in this workspace. That was true when written and is
false now. **Correcting it is a required step of this ticket** — a stale comment
claiming a hazard is absent is worse than none, and it sits directly above the
function whose looseness this section depends on understanding.

### 4.3 Escape hatch

`// allow(tracing-target-coverage)`, on the line immediately before the
invocation or trailing the invocation's own line — the same two positions the
existing `// allow(tracing-target-syntax)` marker accepts, and the same
comment-line bookkeeping.

It exists for the case §4.1 flags: a macro whose final path segment collides
with a `tracing` macro name but which is not one (`mycrate::warn!`). The
existing syntax guard already accepts that false positive by design and offers
this hatch; the coverage guard inherits the same exposure and must offer the
same relief.

**The marker suppresses the whole invocation, for every assertion in §4.4** —
not coverage alone. This section's own justification is that a foreign macro
matched by final path segment needs relief; that macro's `target: "foo::bar"`
would fail the *shape* assertion just as surely as its absence fails coverage,
and relief covering half the exposure is not relief. This matches `try_scan`'s
existing per-invocation suppression semantics
(`tests/workspace-lints/src/lib.rs:163-169`).

**No site in this workspace uses it after this ticket.** The guard asserts that:
if the marker count under `crates/*/src` is ever non-zero, that is a fact a
reviewer should see, so §7.2 pins it at zero. A future legitimate use is a
one-line change to that expectation, made deliberately.

### 4.4 The assertions

For every file under `crates/*/src`, over `scan_invocations(src)?`, skipping any
invocation carrying the §4.3 marker:

| # | Assertion | Fails on |
|---|---|---|
| 1 | Coverage | `TargetArg::Absent` |
| 2 | Literal target | `TargetArg::NonLiteral` |
| 3 | Shape | `TargetArg::Literal(t)` where `t` does not match `^paigasus::[a-z0-9_]+::[a-z0-9_]+$` |
| 4 | Component matches crate (D6) | `TargetArg::Literal(t)` whose component is not the one registered for the crate the file belongs to |
| 5 | No `#[tracing::instrument]` (D7) | the attribute appearing in the file's masked source |

Failures report `path:line` and the offending literal, never a byte offset.

Assertion 3 deliberately does **not** check the component against the
*documented* list — `tracing_target_docs.rs` already does exactly that, and
duplicating it would mean two tests reddening for one cause with two different
messages. Assertion 4 asks a different question: not whether the component is
documented, but whether it is the *right* one for this file.

### 4.5 Anti-vacuity and mutation checks

Following the SMA-557 guard's precedent, which this repo has already found
worth the cost:

- **File-count floor.** Assert the walk reaches at least 100 `.rs` files, so a
  truncated walk fails rather than passes. The real population is **219** files
  (§4.2), so the floor is a tripwire with generous headroom, not a figure copied
  from the sibling guard — whose own population is roughly 390.
- **Positive probe.** Assert `scan_invocations` extracts
  `paigasus::openai::chat` from
  `crates/paigasus-helikon-providers-openai/src/backend/chat.rs`, proving the
  scanner reads real source rather than returning a constant. A path-existence
  check would prove nothing.
- **Unit mutation checks, both directions**, over inline fixture strings in the
  test file:
  - a targeted site must classify as `TargetArg::Literal`, never `Absent`;
  - an untargeted site must **be** reported;
  - an untargeted site carrying the allow-marker must not be reported, in both
    marker positions;
  - `paigasus::core::agent` passes the shape check; `paigasus::core`,
    `paigasus::core::agent::extra`, `paigasus::Core::agent` and
    `paigasus_helikon_core::agent` each fail it;
  - a `tracing` macro written inside a comment or a string literal is invisible
    to the scanner;
  - **required regression fixture** — a struct-field initializer
    `Redirect { target: "/etc/passwd".into() }` yields *no* invocation, pinning
    the §4.2 hazard that a needle-based implementation would fail on
    `core/src/command_match.rs:436`;
  - a `target:` whose value is a `const` or path yields `TargetArg::NonLiteral`,
    not `Absent` and not `Literal`.

The both-directions requirement is the point: a scanner that returns an empty
`Vec` unconditionally passes assertions 1–4 on a clean tree, and only a
must-be-reported case catches it.

### 4.6 Crate mechanics

`tests/workspace-lints` today has an **empty `[dependencies]`** and
`[lints] workspace = true`, which means workspace-wide `missing_docs = "warn"`
applies to it. Every item §4.1 adds therefore needs `///` docs — the
`scan_invocations`, `allow_marker_lines` and `instrument_attribute_lines`
functions, the `ALLOW_MARKER_COVERAGE` constant, `pub struct Invocation`
including each of its fields, and `pub enum TargetArg` including each of its
variants — or the required `docs` job fails under `RUSTDOCFLAGS=-D warnings`.

A second rustdoc trap applies to those same doc comments: a `///` on a `pub`
item must not use `[link]` syntax to reference a private item such as
`mask_trivia`, because `rustdoc::private_intra_doc_links` fails the `docs` job
while every test still passes. Use plain backticked prose.

§7.1's `EnvFilter` test needs `tracing` and `tracing-subscriber` added as
**dev-dependencies** (`workspace = true`; both are already pinned in the root
`[workspace.dependencies]`, `tracing-subscriber` with the `env-filter` feature).
Dev-dependencies only — nothing in the crate's `[dependencies]` changes, so no
published crate gains a dependency.

The member is `version = "0.0.0"`, `publish = false`, with a
`release = false` block in `release-plz.toml`, so it attracts no bump and never
publishes.

Adding dev-dependencies changes this member's `[[package]]` entry in
`Cargo.lock`, which is committed (CLAUDE.md: "Committed (workspace contains a
binary)"). The regenerated lock belongs in the same `test(lints)` commit.

### 4.7 CI

The member is already in the workspace, so the new test runs under the existing
required `test` matrix job. No workflow edit.

---

## 5. Documentation

`docs/book/src/concepts/observability-evaluation.md`, the "Filtering by target"
section. `mdbook build docs/book` must stay clean —
`[output.linkcheck] warning-policy = "error"`.

### 5.1 One namespace, not two

The section opens by describing two namespaces. After this ticket there is one.
Rewrite the opening to say every Helikon event and span carries a
`paigasus::<component>::<subsystem>` target, and that a workspace lint is what
keeps that true.

**Do not write "complete by construction".** The guard covers `tracing` macro
invocations under `crates/*/src`; D7 bans the one construct it cannot see, and
§4.3 leaves an escape hatch that a future contributor may legitimately use. The
honest claim is "every Helikon event carries one, and a workspace lint fails CI
if one stops doing so" — which is true, checkable, and does not repeat the
blanket-claim-with-a-hidden-exception defect SMA-557 spent a review wave
removing.

### 5.2 The matching table gains the group selector

| Directive | Reaches |
|---|---|
| `paigasus` | Raw prefix. Also matches any *non-Helikon* target beginning `paigasus` — a consuming application's own, say. See §5.5. |
| `paigasus::` | The whole namespace. |
| `paigasus::runtime` | **New.** Includes every runtime adapter (D3). A prefix of five components, not a component — so it is not promised to match *only* them. |
| `paigasus::core` | One component. |
| `paigasus::core::agent` | One subsystem. Debugging only; the leaf may change. |

### 5.3 The component table

Inside the `tracing-components:start` / `:end` markers, which
`tracing_target_docs.rs` parses. Add **five** rows (`core`, `runtime_tokio`,
`runtime_axum`, `runtime_actix`, `runtime_agentcore`) and rename the `temporal`
row to `runtime_temporal` — five additions plus one rename, so **six new names**
and **eleven rows** in total. Set every Status cell to `stable`.

The ten names D6 reserves (`facade`, `macros`, `mcp`, `tools`, `evals`, `cli`,
`sessions_sqlite`, `sessions_postgres`, `sessions_redis`, `sessions_testkit`)
must **not** get rows: the guard asserts book and source agree, so a row for a
component nothing emits reddens CI. Record them in prose outside the marked
region, together with D6's derivation rule and the `facade` exception to it.

The parser requires each body row's first cell to be exactly
`` `paigasus::<component>` `` with the component matching `[a-z0-9_]+`. All six
new names satisfy that; the underscore is already permitted.

### 5.4 "What is not in this namespace" → "Migrating from module-path filters"

The existing subsection lists the six crates that emit on module paths. Nothing
does any more, so it is replaced by the migration table:

| Was | Now |
|---|---|
| `paigasus_helikon_core` | `paigasus::core` |
| `paigasus_helikon_runtime_tokio` | `paigasus::runtime_tokio` |
| `paigasus_helikon_runtime_temporal` | `paigasus::runtime_temporal` |
| `paigasus_helikon_runtime_axum` | `paigasus::runtime_axum` |
| `paigasus_helikon_runtime_actix` | `paigasus::runtime_actix` |
| `paigasus_helikon_runtime_agentcore` | `paigasus::runtime_agentcore` |
| `paigasus::temporal` | `paigasus::runtime_temporal` |

**A second migration, easy to miss:** the OTel `target` **attribute** changes
too, not just `RUST_LOG`. `tracing-opentelemetry` sets `with_target: true` by
default (`layer.rs:667`) and attaches `KeyValue::new("target", …)` to every
exported span (`:1185`) and event (`:1370-1376`). So a Langfuse/Jaeger/Honeycomb
saved search, sampling rule or dashboard filter keyed on
`target = "paigasus_helikon_core::agent"` goes silent and must be re-keyed to
`"paigasus::core::agent"`. Span *names* are unaffected (§7.4). The migration
section must say this explicitly — a dashboard that quietly stops matching is
the worst version of this change.

It must state plainly that the old directives **stop matching** rather than
becoming redundant.

It must **not** name a version. The version is decided by release-plz after
merge (§6), so any number written at authoring time is a guess that will be
wrong. Point at the CHANGELOGs instead, where D4 lands the fact with the correct
version attached.

The paragraph explaining that `paigasus_helikon_core::agent` misses the
`workflow.rs` span is kept in substance and moved to the `core` row's context,
updated to `paigasus::core` — the trap still exists at the leaf tier, and it is
now avoided by using the form D1 already recommends.

The prose marking `paigasus::temporal` provisional, and the sentence deferring
this decision to a follow-up, are both deleted.

### 5.5 An honest note on `paigasus` vs `paigasus::`

After this ticket nothing under `crates/*/src` emits on a `paigasus_helikon_*`
target, so for Helikon's own events the bare `paigasus` prefix and `paigasus::`
select the same set.

The book must not present that as a guarantee, for two reasons. It holds only
because a lint says so (§4), not because the contract does, and a lint has an
escape hatch. And it says nothing about *other* crates: a consuming application
with a target beginning `paigasus` is caught by the bare prefix and not by
`paigasus::`. Keep recommending `paigasus::`; give both reasons in a sentence
each.

### 5.6 Recipes

`RUST_LOG='warn,paigasus_helikon_core=debug'` → `RUST_LOG='warn,paigasus::core=debug'`.
Add a runtime-group recipe using D3. The provider recipes are unaffected.

### 5.7 What does not change

No crate `README.md`. No crate's public Rust API, usage example, install story,
feature flag, or published status moves, so CLAUDE.md's README rule does not
fire. A conscious call, not a silent skip.

The facade `README.md` is `include_str!`'d into its rustdoc, making its Rust
fences doctests — another reason not to touch it without cause.

---

## 6. Release mechanics

All six affected crates are already published, so this is release-plz's
**pure-auto** path. **No `version` field, no `[workspace.dependencies]` pin and
no CHANGELOG is edited by hand in this PR.**

This is load-bearing, not merely tidy. CLAUDE.md documents that a manual
same-PR version bump defeats `dependencies_update`: the cascade that re-pins the
facade only runs when release-plz itself performs the sibling bump. The manual
bump is required only when a *stub ascends from `0.0.0`*, which applies to no
crate here.

**Expected outcome after merge**, under D4's non-breaking decision. Baselines
are as of `main@fece785f`; four of them moved after this branch was cut, when
release #215 landed, so read the "Now" column from `main` rather than trusting
these numbers if more releases have shipped since:

| Crate | Now (`main@fece785f`) | Expected |
|---|---|---|
| `paigasus-helikon-core` | `0.5.18` | `0.5.19` |
| `paigasus-helikon-runtime-tokio` | `0.1.22` | `0.1.23` |
| `paigasus-helikon-runtime-axum` | `0.2.4` | `0.2.5` |
| `paigasus-helikon-runtime-actix` | `0.2.4` | `0.2.5` |
| `paigasus-helikon-runtime-temporal` | `0.4.3` | `0.4.4` |
| `paigasus-helikon-runtime-agentcore` | `0.2.7` | `0.2.8` |

Patch bumps, because release-plz bumps an additive `feat` as **patch** on a 0.x
crate — the minor bump is reserved for breaking changes. All six stay
caret-compatible, so `dependencies_update` re-pins dependents without any
consumer facing an incompatible upgrade. The facade and other dependents take
their usual cascade patch bumps.

Had D4 gone the other way, the same table would read `0.6.0` / `0.2.0` /
`0.3.0` / `0.3.0` / `0.5.0` / `0.3.0`, every one of them caret-incompatible, and
the release PR would carry roughly twenty crates. **Check the release PR against
this table** — a much larger release than this is a signal that a breaking
marker reached `main` unintentionally, not that release-plz misbehaved.

`tests/workspace-lints` is `publish = false` with a `release = false` block, and
attracts no bump.

## 7. Verification

### 7.1 An executable test for the `EnvFilter` semantics

`tests/workspace-lints/tests/envfilter_semantics.rs`, in the **existing**
member. `EnvFilter` semantics are a workspace-wide property, not any one
crate's.

The location is pinned rather than left to the implementer because the
alternatives differ in release plumbing: a *new* sibling member would need its
own `Cargo.toml`, a `members` entry in the root `Cargo.toml`, and a
`[[package]] … release = false` block in `release-plz.toml`, or release-plz
would try to version it. Reusing the existing member needs none of that.

`tracing-subscriber` is not a Helikon runtime dependency and must not become
one — the book's "bring your own observability stack" stance depends on core
staying `tracing`-only. It is already pinned in the root
`[workspace.dependencies]` with the `env-filter` feature; §4.6 takes it as a
**dev-dependency** of `tests/workspace-lints`.

**Mechanism.** Build an `EnvFilter` from the directive, layer it under
`tracing_subscriber::registry()` together with a small capture layer that
records each event's target into a shared `Vec`, install it for the duration of
a closure with `tracing::subscriber::with_default`, emit one
`tracing::event!(target: "…", Level::DEBUG, "")` per probe target, and assert on
the captured set.

Constructing a `Metadata` by hand and calling `Filter::enabled` was considered
and rejected: `Metadata` needs a `Callsite`, and `Filter::enabled` needs a
`Context`, which needs a `Subscriber` — so the "direct" route ends up building
the same machinery with more ceremony and less fidelity to what actually
happens at runtime. `with_default` is also the house pattern; the openai and
litellm `translate/request.rs` test modules already use it.

**One `#[test]` per directive, each emitting through its own callsite.**
`tracing::subscriber::with_default` is thread-local while a `tracing` callsite's
interest cache is global, so reusing one `event!` site across four differently
configured subscribers is the classic interest-caching flake. Separate tests
with separate callsites sidestep it; do not loop directives inside one test.

| Directive | Enables | Excludes |
|---|---|---|
| `paigasus::core=debug` | `paigasus::core::agent`, `paigasus::core::workflow` | `paigasus::openai::chat`, `paigasus::runtime_axum::registry` |
| `paigasus::runtime=debug` | all five `runtime_*` subsystems | `paigasus::core::agent`, `paigasus::openai::chat` |
| `paigasus::=debug` | every `paigasus::` target | `hyper::client` |
| `paigasus=debug` | every `paigasus::` target **and** `paigasus_helikon_core::session` | `hyper::client` |

The last row is the one that matters most: it pins the raw-prefix behaviour that
D2, D3 and the whole book section depend on, and it is the fact SMA-557 could
only verify by hand.

### 7.1.1 The self-poisoning hazard — read before writing either test

`tracing_target_docs.rs` walks **both `crates/` and `tests/`**, and
`scan_targets` finds any `target:` followed by an adjacent string literal in
*real code*. A test file in `tests/workspace-lints` that writes a genuine
`tracing::event!(target: "paigasus::fake::x", …)` as a negative control
therefore injects a phantom component `fake` into that guard's source set and
reddens it — a failure in a *different* test file, blaming a component nobody
declared.

Two rules follow, and both are load-bearing:

1. **§7.1's probe targets must be passed as `const`s, not string literals.**
   `scan_targets` requires a *string literal* adjacent to the needle
   (`tests/workspace-lints/src/lib.rs:214-220`), so
   `const T_CORE_AGENT: &str = "paigasus::core::agent";` plus
   `tracing::event!(target: T_CORE_AGENT, …)` contributes nothing to the docs
   guard.

   Writing the probes as literals would *appear* to work — every `paigasus::`
   probe in §7.1's table uses a real documented component, and the two negative
   probes (`paigasus_helikon_core::session`, `hyper::client`) yield `None` from
   `component_of` (`src/lib.rs:238`). But it would make the probe file a
   **second, invisible source of truth**: `tracing_target_docs.rs` walks
   `tests/` as well as `crates/` (`:147`), so those components would enter
   `in_source` permanently. If every real `runtime_tokio` call site were later
   deleted, the docs guard would still demand the book row — inverting the
   "documented but not in source" arm the guard exists for.

   The `const` form removes the hazard in both directions and frees the test to
   probe any target it likes, including deliberately malformed ones.
2. **§4.5's malformed fixtures must be Rust string literals, never real
   invocations.** `mask_trivia` blanks string literals before `scan_targets`
   searches for the needle, so a fixture written as
   `r#"tracing::warn!(target: "paigasus::Core::agent", "m");"#` is invisible to
   the docs guard while remaining a perfectly good input to `scan_invocations`,
   which takes `&str`. This is the same technique
   that keeps SMA-543's and SMA-557's own test fixtures from poisoning each
   other; it is not a new trick, but it is not optional either.

### 7.2 The guard's own expectations

- `scan_invocations` reports no `TargetArg::Absent` and no
  `TargetArg::NonLiteral` for any file under `crates/*/src`.
- Every target literal matches the three-segment shape.
- The `// allow(tracing-target-coverage)` marker appears zero times under
  `crates/*/src`, counted with the **same** comment-aware bookkeeping the
  existing marker uses — not `src.contains(…)`, which would also count the
  marker text inside a string literal, the false positive
  `tests/workspace-lints/src/lib.rs:846-877` exists to prevent.

  `collect_allow_marker_lines` (`src/lib.rs:296`) is private and hardcodes
  `ALLOW_MARKER` (`:49`). Generalizing it to take the marker as a parameter
  touches the SMA-543 guard's code path, so that refactor is part of this
  ticket's work rather than an incidental tidy-up, and the syntax guard's own
  tests must stay green across it.

- **The predicate macros are in scope.** `enabled`, `event_enabled` and
  `span_enabled` are in `TRACING_MACROS` (`src/lib.rs:34-37`), so assertion 1
  will demand a `target:` on them too. None exists in the workspace today. The
  first one written must either carry a target or change the zero-marker
  expectation above — a deliberate choice, which is the point.

### 7.3 Existing gates that must stay green

- `tracing_target_docs.rs` — will redden until §5.3 lands. That is its job.
- `tracing_target_syntax.rs` — every new `target:` uses `:`, never `=`. The
  41 edits are exactly the population that guard was written for.
- `cargo test --workspace --all-features` — the full required gate, not
  per-crate. A per-crate run has previously missed a feature-unification defect.
- `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-features --all-targets -- -D warnings` before
  every push; the `pre-push` hook runs both.
- `mdbook build docs/book`.
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`.

### 7.4 Manual spot check — **optional**

Run an example that actually reads `RUST_LOG` with
`RUST_LOG='paigasus::core=debug'` and confirm the `agent.run` / `agent.turn` /
`gen_ai.chat` / `tool.execute` spans still appear; then re-run with the old
`paigasus_helikon_core=debug` and confirm they do **not**.

**Use `crates/paigasus-helikon-runtime-agentcore/examples/agent_http.rs`.** It is
the only example that satisfies *both* halves of the check: it wires
`EnvFilter::try_from_default_env()` (`:28-29`) **and** it drives a real
`LlmAgent` (`:36`), which is what raises the four spans. It needs a live
`ANTHROPIC_API_KEY` (`:33`) — unavoidable, since demonstrating the agent trace
tree means running an agent, which means running a model.

Two examples were named by earlier drafts and both were wrong. Neither is a
valid substitute:

- `crates/paigasus-helikon/examples/langfuse_tracing.rs` installs
  `tracing_subscriber::registry().with(tracing_opentelemetry::layer()…)` with no
  `EnvFilter` and no `RUST_LOG` handling at all (`:150-152`), and `main`
  hard-`?`s on `LANGFUSE_PUBLIC_KEY` / `LANGFUSE_SECRET_KEY` (`:133-134`), so
  without live Langfuse credentials it exits before emitting anything.
- `crates/paigasus-helikon-runtime-agentcore/examples/echo_http.rs` does wire
  `EnvFilter`, but its `EchoAgent` implements `Agent` directly and never enters
  `LlmAgent`'s loop — so it raises none of the four spans at any `RUST_LOG`
  setting. Substituting it fixed the first example's missing `EnvFilter` while
  silently breaking the half that mattered.

**This check is optional, because the claim it tests is already covered
mechanically, and more tightly.** §4.4's assertion 4 proves the four
`info_span!` sites in `core` carry `paigasus::core::agent` — it fails if any
does not — and §7.1's `two_segment_component_selects_only_that_component`
proves an `EnvFilter` built from `paigasus::core=debug` selects that exact
target. The composition of those two *is* the claim. Running the example adds
confidence that a real subscriber is wired as expected; it adds no evidence of
correctness that CI does not already carry, and it must never be treated as a
gate — it cannot run without a paid API key.

**What this does and does not de-risk.** Exported OTel span *names* are
unaffected — `tracing-opentelemetry` 0.33 derives the span name from the
`tracing` span's name, never from its target. But the target **is** exported as
an attribute, and `with_target` defaults to **`true`**
(`tracing-opentelemetry-0.33.0/src/layer.rs:667`, documented at `:880`), so
`KeyValue::new("target", …)` is attached to every span (`:1185`) and event
(`:1370-1376`). **Every exported span's `target` attribute changes value in this
ticket.** An earlier draft called the OTel side "unaffected"; that was wrong,
and §5.4 now carries the migration row it implies.

## 8. Non-goals

- **Adding subsystems, or renaming any provider component.** The five provider
  components and their 56 sites are untouched.
- **Adding `#[tracing::instrument]` anywhere.** There is none today, it would
  put targets on module paths again by default, and D7 now rejects it
  mechanically. Lifting that ban — by teaching the guard to require
  `target = "paigasus::…"` on the attribute, note `=` not `:` — is a separate
  decision.
- **New `tracing` call sites.** This ticket re-targets existing events; it adds
  no observability.
- **Changing span names, levels or fields.** Only the `target:` argument is
  added.

  Note this is not always a pure prepend: `tracing`'s macro arms require
  `target:` **before** `parent:`, and `agent.rs:548`, `:859` and `:919` all pass
  `parent:` as their first argument. The new argument goes ahead of it. This
  fails loudly at compile time rather than silently, but a mechanical
  "append an argument" edit will not build.
- **A CONTRIBUTING row for component renames.** SMA-557's D1(c) already routes
  a *future* component rename through the existing `BREAKING CHANGE:` mechanism,
  and that stays true — D4 concerns this ticket's own adoption, which renames no
  stable component. SMA-557 flagged promoting the rule into CONTRIBUTING as a
  reasonable follow-up rather than a prerequisite; that also stays true.
- **An ADR.** `docs/book/src/decisions/index.md` still says a formal ADR section
  is "the planned next step". This spec does not create one.

---

## 9. Commits and PR

Branch: `feature/sma-568-decide-whether-core-and-the-runtime-crates-should-adopt`.

All scopes below already exist in `.versionrc`'s `scopeRegex` and in
`pr-title.yml`'s `scopes:` list **on `main`** — no new scope is registered, so
the base-branch-reads-the-allowlist trap does not apply.

| Commit | Touches |
|---|---|
| `docs(spec): SMA-568 …` | this document |
| `docs(plan): SMA-568 …` | the implementation plan |
| `feat(core): SMA-568 …` | `crates/paigasus-helikon-core/src` — 12 sites |
| `feat(runtime): SMA-568 …` | the five runtime crates — 30 sites (29 newly targeted, plus the component rename on `activities.rs`) |
| `test(lints): SMA-568 …` | `scan_invocations`, the coverage/shape/D6/D7 guard, `envfilter_semantics.rs`, the `src/lib.rs:190-192` comment fix, and the regenerated `Cargo.lock` |
| `docs(docs): SMA-568 …` | the book page |

PR title: `feat(core): SMA-568 adopt paigasus:: tracing targets in core and the runtimes`.

**No `!`, and no `BREAKING CHANGE:` footer anywhere** — see D4. If that decision
is ever revisited, note that this repo's squash-merge is configured
`squash_merge_commit_title: PR_TITLE`, `squash_merge_commit_message:
COMMIT_MESSAGES`, so per-commit footers *do* reach release-plz and a `!` in the
title reaches it too. Both carriers work here; the decision not to use them is
about proportionality, not plumbing.

Lowercase subject after the `SMA-568 ` prefix, satisfying
`subjectPattern: ^([A-Z]{2,4}-\d+ )?[^A-Z].+$`, with a full Conventional
Commits `type(scope)!:` prefix. The PR body cites PR numbers, not other
`SMA-###` tokens, which trip CodeRabbit's Linked Issues check.
