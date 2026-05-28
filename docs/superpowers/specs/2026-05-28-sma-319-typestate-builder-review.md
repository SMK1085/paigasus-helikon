# SMA-319 — Typestate builder for `LlmAgent` review

**Reviewer:** Claude (staff-engineering review)
**Reviewed:** [`2026-05-28-sma-319-typestate-builder-design.md`](./2026-05-28-sma-319-typestate-builder-design.md)
**Date:** 2026-05-28
**Sources cross-checked:** Linear SMA-319, Notion "Structured Output & Builder" + "Typestate builder for agent construction" (ADR), the current `paigasus-helikon-core::agent` module, the macros crate's existing trybuild harness (precedent for the new UI-test setup), `.github/workflows/ci.yml` (`--skip trybuild_ui` rule), workspace `Cargo.toml`.

The spec is well-shaped and the architectural decisions (consume `self` per transition, `PhantomData<fn() -> T>` for invariance, owned-`M` + `shared_model<Arc<M>>` split, marker types as zero-sized publics) are all the right calls. The issues below are mostly ergonomic gaps and one real "this may not compile" question the spec already flags.

## Critical issues

### 1. `impl LlmAgent<(), (), String>` is unlikely to compile

The spec's `builder()` docking point is an inherent impl on a concrete head:

```rust
impl LlmAgent<(), (), String> {
    pub fn builder<Ctx>() -> LlmAgentBuilder<Ctx, (), String, NoName, NoModel> { … }
}
```

…but the struct definition carries `where M: crate::Model + 'static`. `()` does not implement `Model`. In current stable Rust (1.75+), inherent-impl heads do enforce the type's `where` bounds — the impl block will fail with "the trait bound `(): Model` is not satisfied."

The spec acknowledges this as an open question and offers the fallback ("free function `pub fn llm_agent_builder<Ctx>()` re-exported from `agent_builder.rs`"). The risk is that this only gets discovered partway through implementation, by which point a lot of code references `LlmAgent::builder::<Ctx>()`.

**Fix**: validate up-front with a 10-line proof-of-concept before committing to the design. If `impl LlmAgent<(), (), String>` doesn't compile, restructure to the free-function entry point now and rename the call site to `paigasus_helikon::core::llm_agent_builder::<Ctx>()` (or similar). The user-visible call shape differs by ~one symbol; settling it before the implementation PR opens prevents rework.

A second viable shape: put `builder` on the builder type itself, not on `LlmAgent`:

```rust
impl<Ctx> LlmAgentBuilder<Ctx, (), String, NoName, NoModel>
where Ctx: Send + Sync + 'static,
{
    pub fn new() -> Self { … }
}
// User calls: LlmAgentBuilder::<MyCtx, _, _, _, _>::new() — ugly
// Or with default-Ctx: LlmAgentBuilder::<()>::new() — better, ergonomic in CtxIs() case
```

Less aesthetic than `LlmAgent::builder()` but compiles cleanly.

### 2. `.tool()` / `.handoff()` / `.hook()` ergonomics are inconsistent with `.model()`

The model setter accepts an owned value and wraps internally:

```rust
pub fn model<M2>(self, m: M2) -> LlmAgentBuilder<…> where M2: Model + 'static, { … self.shared_model(Arc::new(m)) }
pub fn shared_model<M2>(self, m: Arc<M2>) -> LlmAgentBuilder<…> { … }
```

Clean: `.model(my_model)` works for a freshly-constructed value; `.shared_model(arc)` works for a pre-wrapped one.

But `.tool` (and `.handoff`, `.hook`, `.input_guardrail`, `.output_guardrail`) take a pre-wrapped trait object:

```rust
pub fn tool(mut self, t: Arc<dyn Tool<Ctx>>) -> Self { … }
```

This forces every call site to write `.tool(Arc::new(my_tool) as Arc<dyn Tool<MyCtx>>)`. The Notion example shown in the design ("Structured Output & Builder") writes `.tools([fetch_flow_panel, fetch_karyotype])` — which is the Notion target user experience — and that doesn't work with the spec's current `.tools(Vec<Arc<dyn Tool<Ctx>>>)` signature.

**Fix**: mirror the model pattern across all of these:

```rust
pub fn tool(mut self, t: impl Tool<Ctx> + 'static) -> Self {
    self.tools.push(Arc::new(t) as Arc<dyn Tool<Ctx>>);
    self
}
pub fn shared_tool(mut self, t: Arc<dyn Tool<Ctx>>) -> Self { … }

pub fn tools(mut self, t: impl IntoIterator<Item = Arc<dyn Tool<Ctx>>>) -> Self { … }
```

Same for `.handoff`/`.shared_handoff`, `.hook`/`.shared_hook`, `.input_guardrail`/`.shared_input_guardrail`, `.output_guardrail`/`.shared_output_guardrail`.

This is the single biggest ergonomic improvement available. Without it, the Notion design example is aspirational rather than achievable, and every realistic call site has Arc-noise.

(Note: even with this fix, `.tools([fetch_flow_panel, fetch_karyotype])` as a *heterogeneous* array literal won't compile because `[T; N]` requires homogeneous element types — see also #8 below. The `.tool().tool()` chain or `.tools(tools![…])` from SMA-315 are the realistic call shapes. Worth fixing the Notion example.)

### 3. AC #2 is not actually satisfied by SMA-319 alone

The Linear ticket's AC #2 reads:

> `RunResult<T>` carries `final_output: T` after `.output_type::<T>()` was set.

The spec satisfies this via the trybuild fixture `builder_output_type_typed.rs`, which binds `let _: LlmAgent<MyCtx, MockModel, Answer> = …`. That proves `T` flows to `LlmAgent`. It does **not** prove `T` flows to `RunResult<T>` — because the runner sees `&dyn Agent<Ctx>` (trait-erased; no `T`), and the existing `RunResult<T = String>` is set by `RunResultStreaming::collect`, not by the agent.

The non-goal section is honest about this: "Wiring the T parameter through the runner / RunResult machinery — that's SMA-320's job. SMA-319 only shapes the storage and the builder API." But the AC mapping is then misleading — the spec claims AC #2 is met when it's actually deferred.

**Fix**: rephrase the AC #2 entry in §"Acceptance criteria → evidence" (or wherever it sits — the spec's evidence-map table is implicit) as:

> AC #2 (partial): `T` flows from `.output_type::<T>()` to `LlmAgent<Ctx, M, T>` — verified by `builder_output_type_typed.rs`. The `RunResult<T>` wiring depends on the runner being generic over the agent's output type; lands with SMA-320.

This makes the SMA-319/SMA-320 split honest and prevents reviewers from rejecting the PR for an incomplete AC.

## Significant issues

### 4. `.description()` setter is missing

§"Non-goals" reads:

> A `.description(…)` builder method. Description remains a public field; callers who need to set it mutate the field post-build. (Discussed and consciously skipped during brainstorming.)

The rationale isn't given. Every other field has a builder method. Forcing users to write:

```rust
let mut agent = LlmAgent::builder().name("triage").model(m).build();
agent.description = "Routes incoming requests".into();
```

…is strictly worse than `.description("Routes incoming requests")` inline. The "post-build mutate" path is also awkward in real call sites that bind `agent` immutably (the common case).

**Recommendation**: include `.description(impl Into<String>) -> Self` as an any-state optional setter. One line in the impl block. Aligned with `.instructions`, `.tool`, etc. If the consciously-skipped reasoning was "description is rarely set in practice," that's not a reason to leave it inconsistent — it's a reason to add the setter and let users skip it.

This matters in multi-agent setups: handoff targets render their description into the prompt that the dispatching agent sees. Empty descriptions silently degrade routing quality.

### 5. `.output_type::<T>()` bounds are looser than `.build()` requires

The transition is defined as:

```rust
pub fn output_type<T2>(self) -> LlmAgentBuilder<…>
where T2: serde::de::DeserializeOwned + schemars::JsonSchema,
```

But `.build()` adds `T: Send + Sync + 'static`. So `.output_type::<Rc<u32>>()` compiles (Rc is `DeserializeOwned + JsonSchema`) and fails at `.build()` with a less-localized error.

**Fix**: add `Send + Sync + 'static` to `.output_type`'s bounds:

```rust
where T2: Send + Sync + 'static + serde::de::DeserializeOwned + schemars::JsonSchema,
```

Diagnostics now point at the `.output_type::<…>` call site, not at `.build()`. The bound is redundant from a soundness perspective (`.build` would catch it anyway) but better-localized errors are worth the duplication.

### 6. `.instructions` has no `.shared_instructions` counterpart

`.instructions(impl Instructions<Ctx> + 'static)` wraps in Arc internally. ✓ Good owned-value path.

But if a user already holds an `Arc<dyn Instructions<Ctx>>` (shared across multiple agents — a realistic case for dynamic instructions backed by a config service), there's no entry point. They'd need to construct a wrapper that impls `Instructions<Ctx>` and forwards.

**Fix**: add `.shared_instructions(Arc<dyn Instructions<Ctx>>) -> Self`. Mirrors the `.shared_model` pattern.

### 7. Marker type names risk future collision

§"Crate layout" says: `src/lib.rs` adds `pub mod agent_builder; pub use agent_builder::*;`. That exports `NoName`, `HasName`, `NoModel`, `HasModel` at the crate root of `paigasus-helikon-core`.

These names are generic — a future ticket adding builder-state typestates for a different builder (a hypothetical `RunConfigBuilder`, or `OpenAiModelBuilder` from SMA-316) will want `NoSomething` / `HasSomething` patterns and collide at the root.

**Fix**: namespace the markers. Either:

- Keep them inside the `agent_builder` module and re-export only `LlmAgentBuilder` at the root. Users who need to spell out marker types do `paigasus_helikon_core::agent_builder::{NoName, HasName, …}`. They rarely need to — the markers appear in error messages, not user code.
- Or prefix: `BuilderHasName`, `BuilderNoName`. Less idiomatic.

The first option is cheaper and standard practice (e.g. `reqwest::header::HeaderName` rather than `reqwest::HeaderName`).

### 8. Notion design example `.tools([t1, t2])` doesn't compile as written

The "Structured Output & Builder" Notion page — which the spec cites as the design target — shows:

```rust
.tools([fetch_flow_panel, fetch_karyotype])
```

Each `#[tool]`-macro-generated tool is a unit struct of a distinct type. `[T; N]` requires homogeneous element types. Even if `.tools` accepted `impl IntoIterator<Item = ToolValue>`, the array literal `[unit_a, unit_b]` won't type-check because `unit_a` and `unit_b` have different types.

Realistic call shapes:

```rust
.tool(fetch_flow_panel).tool(fetch_karyotype)         // singular chain
.tools(tools![fetch_flow_panel, fetch_karyotype])     // SMA-315 macro
```

**Fix**: update the Notion design example (out of scope for the spec itself, but worth flagging) so the user-facing reference matches what SMA-319 actually delivers. The spec should also call out which call shape it considers canonical, so SMA-320 / docs land consistently.

## Smaller items

- **Per-transition struct rebuild is 13 field moves.** A cleaner pattern is `LlmAgentBuilder<…> { inner: BuilderInner, _state: PhantomData<…> }` where transitions only rebuild the small marker carrier. Cosmetic; the spec's inlined form is explicit and obvious. Not load-bearing.
- **`MockModel` duplicated across UI fixtures.** Trybuild supports shared fixture modules via `#[path = "common/mock.rs"] mod common;`. The spec rejects this implicitly ("self-contained"); accepting it would centralize the 10-line stub. Marginal improvement.
- **Adding `_output: PhantomData<fn() -> T>` to `LlmAgent`** is a breaking change for struct-literal users. The spec enumerates touch sites (`loop_happy_path.rs`, `loop_parallel_tools.rs`). Per CLAUDE.md the workspace is pre-1.0 so this is acceptable, but the spec should note it as a soft-break for any downstream consumer (none yet) who used struct-literal construction.
- **Doc-coverage burden.** ~18 new `pub` items need `///` docs to keep the 80% threshold. The spec's "trivially documentable" claim is correct but worth the explicit chore-line in the implementation plan.
- **`builder_happy_path.rs` fixture details.** The spec says it covers "Full chain with both required + a sampling of optionals (instructions, tool, hook, max_turns)." Be concrete about which optionals — every one not exercised here is a place a typestate-irrelevant signature change can land undetected. Probably want: `.instructions`, `.tool` (singular), `.tools` (plural), `.handoff`, `.hook`, `.input_guardrail`, `.output_guardrail`, `.model_settings`, `.max_turns`, `.output_type::<T>()`. Ten optional calls — locks the full any-state surface in one fixture.
- **`.output_type::<String>()` is degenerate but legal.** No real bug; document in the rustdoc that selecting `T = String` is functionally equivalent to never calling `.output_type` (modulo the `output_type` field being `Some(schema_for_string)`).
- **CI `--skip trybuild_ui` substring match.** The macros crate's harness file is `tests/trybuild.rs` containing `#[test] fn trybuild_ui()`. The new core crate's harness is `tests/trybuild_ui.rs` containing `#[test] fn trybuild_ui()`. The cargo test names are both `trybuild_ui`; `--skip trybuild_ui` catches both. The spec's claim "The macros crate's harness is named `trybuild_ui`" is loose wording — the **function** is, the **file** is `trybuild.rs`. Minor. ✓ The CI gate works as described.
- **`feat(core): SMA-319 …` commit attribution.** The change adds a default generic parameter (`T = String`) and a `PhantomData` field. For struct-literal callers, this is a soft-break; for trait-object callers, it's transparent. release-plz will read it as a `feat` and propose a 0.1.0 → 0.2.0 minor bump. That's the right outcome.

## Verdict

This spec is in good shape but item #1 (the `impl LlmAgent<(), (), String>` viability question) is load-bearing: settle it before the implementation PR opens, with a 10-line proof-of-concept. The fallback to a free-function entry point is straightforward, but reshapes the user-visible call site, so the decision drives the rest of the PR.

Items #2 and #3 are the ergonomic and honesty fixes that meaningfully change the surface:

- **#2 (`.tool` accepts impl)** — unblocks the Notion design example, removes Arc-noise from realistic call sites. Real improvement.
- **#3 (AC #2 honesty)** — spec acknowledges in non-goals that runner wiring is SMA-320, but the AC evidence map then claims AC #2 is met. Tighten the wording so the ticket can close cleanly.

Items #4–#7 are pre-merge polish: `.description` setter (one line), `.output_type` bound tightening, `.shared_instructions`, marker namespacing. None are load-bearing.

The architectural choices — `PhantomData<fn() -> T>` for invariance, owned/shared model pair, consuming `self` per transition, `T: Send + Sync + 'static` at `.build()`, marker types as zero-sized publics with docstrings — are all the right calls and well-justified in the spec.

Once items 1–3 are settled the spec is ready for implementation.

## Sources

- [`docs/superpowers/specs/2026-05-28-sma-319-typestate-builder-design.md`](./2026-05-28-sma-319-typestate-builder-design.md)
- [Linear SMA-319](https://linear.app/smaschek/issue/SMA-319/typestate-builder-for-llmagent)
- [Notion — Structured Output & Builder](https://www.notion.so/355830e8fbaa818ab932d9c646657ced)
- [Notion — ADR Typestate builder for agent construction](https://www.notion.so/355830e8fbaa81b89cabc0c831162273)
- `crates/paigasus-helikon-core/src/agent.rs`
- `crates/paigasus-helikon-macros/tests/trybuild.rs` (precedent for the UI harness)
- `.github/workflows/ci.yml` (`--skip trybuild_ui` rule)
