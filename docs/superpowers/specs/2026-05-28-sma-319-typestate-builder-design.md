# SMA-319 — Typestate builder for `LlmAgent`

**Linear:** [SMA-319](https://linear.app/smaschek/issue/SMA-319/typestate-builder-for-llmagent)
**Branch:** `feature/sma-319-typestate-builder-for-llmagent`
**References:**
- [Structured Output & Builder (Notion)](https://www.notion.so/355830e8fbaa818ab932d9c646657ced)
- ADR — *Typestate builder for agent construction*

## Goal

Ship the ergonomic typestate builder for `LlmAgent` plus the structural change that the typed-output path (SMA-320) hangs off. After this ticket:

- `LlmAgent::builder()` is the canonical construction path; struct-literal construction remains available as an escape hatch.
- The compiler statically refuses to `.build()` an agent without both a `.name(…)` and a `.model(…)`. `trybuild` compile-fail tests lock the error surface.
- `LlmAgent` carries a third generic parameter `T` (default `String`) representing the structured-output type. `.output_type::<T>()` is a typestate transition that swaps `T` on the resulting agent.
- The Model type `M` is **inferred** from the `.model(m)` call site — no turbofish anywhere in the call chain.

## Non-goals

- Wiring the `T` parameter through the runner / `RunResult` machinery — that's SMA-320's job. SMA-319 only shapes the storage and the builder API; runner behaviour for typed output (`response_format`, retry/repair, typed `RunResult`) stays unchanged. See the "Acceptance criteria mapping" section below for how SMA-319's AC #2 splits across the two tickets.
- A `.config(RunConfig)` setter. `.max_turns(u32)` covers everything `RunConfig` exposes today; SMA-321 will add `.config(…)` (or further sub-knob setters) when it adds the other `RunConfig` fields, without a breaking change.
- Builder-side validation beyond the typestate (e.g. "must have at least one tool if output_type is set"). The builder enforces structure, not policy.
- A `Default` impl for `LlmAgentBuilder` in its initial state. `LlmAgent::builder()` is the entry point.

## Acceptance criteria mapping

| AC (from Linear) | Coverage in SMA-319 | Evidence |
|---|---|---|
| `cargo build` fails when `.name` or `.model` is missing (verified via `trybuild` compile-fail tests). | **Full.** | `tests/ui/builder_missing_*.rs` fixtures (see Tests section). |
| `RunResult<T>` carries `final_output: T` after `.output_type::<T>()` was set. | **Partial.** `T` flows from `.output_type::<T>()` into `LlmAgent<Ctx, M, T>`. The runner-side wiring that converts that `T` into a real `RunResult<T>` lands in SMA-320 (`response_format` plumbing, deserialization, one-shot retry on validation failure). | `tests/ui/builder_output_type_typed.rs` proves `T` reaches the agent type. The full chain to `RunResult<T>` is owned by SMA-320. |

This split is the reason for the SMA-319 / SMA-320 boundary: SMA-319 freezes the public API surface (including the `T` parameter); SMA-320 fills in runtime behavior without further signature churn.

## Crate layout

| Crate | Change |
|---|---|
| `paigasus-helikon-core` | Modify `src/agent.rs`: add `T = String` generic to `LlmAgent`, add `_output: PhantomData<fn() -> T>` field, add `pub fn builder()` associated function, update the `Agent<Ctx>` impl to carry `T`. New file `src/agent_builder.rs` containing the typestate markers, the `LlmAgentBuilder` struct, and all impls. `src/lib.rs` adds `pub mod agent_builder; pub use agent_builder::*;`. New `tests/ui/` directory plus `tests/trybuild_ui.rs` harness. New `dev-dependencies` entry for `trybuild`. |
| `paigasus-helikon` (facade) | No change. The facade already re-exports `paigasus_helikon_core::*` unconditionally — the typestate markers and builder become available through it automatically. |
| `Cargo.toml` (workspace) | No change. `trybuild` is already in `[workspace.dependencies]` (pinned by SMA-315). |

`paigasus-helikon-core` is already at `0.1.0` — a single `feat(core): SMA-319 add typestate builder for LlmAgent` commit drives the normal release-plz minor bump. No 0.0.0 → 0.1.0 escape dance is needed here.

## `LlmAgent` generic change

```rust
// crates/paigasus-helikon-core/src/agent.rs

pub struct LlmAgent<Ctx, M, T = String>
where
    Ctx: Send + Sync + 'static,
{
    pub name: String,
    pub description: String,
    pub instructions: std::sync::Arc<dyn Instructions<Ctx>>,
    pub model: std::sync::Arc<M>,
    pub tools: Vec<std::sync::Arc<dyn crate::Tool<Ctx>>>,
    pub handoffs: Vec<std::sync::Arc<dyn crate::Agent<Ctx>>>,
    pub output_type: Option<OutputType>,
    pub input_guardrails: Vec<std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    pub output_guardrails: Vec<std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    pub hooks: Vec<std::sync::Arc<dyn crate::Hook<Ctx>>>,
    pub model_settings: crate::ModelSettings,
    pub config: crate::RunConfig,
    _output: std::marker::PhantomData<fn() -> T>,
}
```

**Why `T = String` as the default:** every existing reference to `LlmAgent<Ctx, M>` continues to compile and behaves identically. The runner currently produces `RunResult<String>` regardless of this marker; SMA-320 will plumb the marker through.

**Why `PhantomData<fn() -> T>`:** `T` is never owned by the struct (no field of type `T`), but we still need it in the type signature so the builder can carry it across `.output_type::<T>()` transitions. `fn() -> T` keeps `T` invariant — defensive (covariance/contravariance distinctions don't matter when `T` doesn't appear in a real field, but the `fn() -> _` form is the idiom and avoids accidental subtyping surprises if `T` ever does enter the field set).

**Why drop the `M: Model + 'static` bound from the struct:** the inherent-impl head `impl LlmAgent<(), (), String>` (used to attach the `builder` associated function) would otherwise require `(): Model`, which fails. The bound now lives only on the impls that actually call into `M` (the `Agent<Ctx>` impl and the inherent impl docking point). The struct field `model: Arc<M>` is well-formed for any sized `M`.

**Why on `LlmAgent` and not just on the builder:** AC #2 (see mapping above) calls for `T` to flow into `RunResult<T>`. Putting `T` only on the builder would discard it at `.build()` and force SMA-320 to re-add it, breaking signatures twice. Adding `T` here means SMA-320 only adds runtime behavior, not signatures.

The existing `pub fn builder()` slot becomes:

```rust
impl LlmAgent<(), (), String> {
    // Disambiguator: we need an inherent impl block to attach `builder`,
    // and the type parameters here don't matter because `builder` ignores
    // `Self` entirely. Pick a concrete head so the impl is unambiguous.
    pub fn builder<Ctx>() -> LlmAgentBuilder<Ctx, (), String, NoName, NoModel>
    where
        Ctx: Send + Sync + 'static,
    {
        LlmAgentBuilder {
            name: None,
            description: None,
            instructions: None,
            model: None,
            tools: Vec::new(),
            handoffs: Vec::new(),
            output_type: None,
            input_guardrails: Vec::new(),
            output_guardrails: Vec::new(),
            hooks: Vec::new(),
            model_settings: crate::ModelSettings::default(),
            config: crate::RunConfig::default(),
            _state: std::marker::PhantomData,
        }
    }
}
```

**Why a fixed `impl LlmAgent<(), (), String>` head:** Rust requires inherent-impl heads to be concrete. The associated function doesn't use `Self`, so the head is just a docking point — `()` for both `Ctx` and `M` is the obvious sentinel. Users call `LlmAgent::builder::<MyCtx>()` (or rely on `Ctx` inference from later setter calls).

## `agent_builder.rs` — typestate markers and builder

### Markers

```rust
// crates/paigasus-helikon-core/src/agent_builder.rs

/// Typestate marker: `.name(…)` has not been called yet.
pub struct NoName;
/// Typestate marker: `.name(…)` has been called.
pub struct HasName;
/// Typestate marker: `.model(…)` / `.shared_model(…)` has not been called yet.
pub struct NoModel;
/// Typestate marker: `.model(…)` / `.shared_model(…)` has been called.
pub struct HasModel;
```

These are zero-sized public types. They appear in user-facing error messages when `.build()` is called too early; the doc comments above are what shows up in IDE hover.

### Struct

```rust
pub struct LlmAgentBuilder<Ctx, M, T, N, Mo>
where
    Ctx: Send + Sync + 'static,
{
    name: Option<String>,
    description: Option<String>,
    instructions: Option<std::sync::Arc<dyn Instructions<Ctx>>>,
    model: Option<std::sync::Arc<M>>,
    tools: Vec<std::sync::Arc<dyn crate::Tool<Ctx>>>,
    handoffs: Vec<std::sync::Arc<dyn crate::Agent<Ctx>>>,
    output_type: Option<crate::OutputType>,
    input_guardrails: Vec<std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    output_guardrails: Vec<std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    hooks: Vec<std::sync::Arc<dyn crate::Hook<Ctx>>>,
    model_settings: crate::ModelSettings,
    config: crate::RunConfig,
    _state: std::marker::PhantomData<fn() -> (N, Mo, T)>,
}
```

The `M = ()` initial state means `model` is `Option<Arc<()>>`, always `None` until `.model(…)` transitions to `M2`. The transition consumes `self` and rebuilds the struct with `Some(Arc::new(m))` of type `Option<Arc<M2>>`.

### Methods callable in **any** state

Optional setters. Each takes `mut self`, mutates a field, returns `Self` unchanged in its typestate parameters.

```rust
impl<Ctx, M, T, N, Mo> LlmAgentBuilder<Ctx, M, T, N, Mo>
where
    Ctx: Send + Sync + 'static,
{
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }

    pub fn instructions(mut self, i: impl Instructions<Ctx> + 'static) -> Self {
        self.instructions = Some(std::sync::Arc::new(i));
        self
    }

    pub fn shared_instructions(mut self, i: std::sync::Arc<dyn Instructions<Ctx>>) -> Self {
        self.instructions = Some(i);
        self
    }

    // Singular adders: take an owned trait impl and wrap in Arc internally.
    pub fn tool(mut self, t: impl crate::Tool<Ctx> + 'static) -> Self {
        self.tools.push(std::sync::Arc::new(t) as std::sync::Arc<dyn crate::Tool<Ctx>>);
        self
    }

    pub fn shared_tool(mut self, t: std::sync::Arc<dyn crate::Tool<Ctx>>) -> Self {
        self.tools.push(t);
        self
    }

    // Plural setters: accept any iterable of pre-wrapped trait objects.
    pub fn tools<I>(mut self, t: I) -> Self
    where I: IntoIterator<Item = std::sync::Arc<dyn crate::Tool<Ctx>>>,
    {
        self.tools = t.into_iter().collect();
        self
    }

    pub fn handoff(mut self, h: impl crate::Agent<Ctx> + 'static) -> Self { … }
    pub fn shared_handoff(mut self, h: std::sync::Arc<dyn crate::Agent<Ctx>>) -> Self { … }
    pub fn handoffs<I>(mut self, h: I) -> Self
    where I: IntoIterator<Item = std::sync::Arc<dyn crate::Agent<Ctx>>> { … }

    pub fn hook(mut self, h: impl crate::Hook<Ctx> + 'static) -> Self { … }
    pub fn shared_hook(mut self, h: std::sync::Arc<dyn crate::Hook<Ctx>>) -> Self { … }
    pub fn hooks<I>(mut self, h: I) -> Self
    where I: IntoIterator<Item = std::sync::Arc<dyn crate::Hook<Ctx>>> { … }

    pub fn input_guardrail(mut self, g: impl crate::Guardrail<Ctx> + 'static) -> Self { … }
    pub fn shared_input_guardrail(mut self, g: std::sync::Arc<dyn crate::Guardrail<Ctx>>) -> Self { … }
    pub fn input_guardrails<I>(mut self, g: I) -> Self
    where I: IntoIterator<Item = std::sync::Arc<dyn crate::Guardrail<Ctx>>> { … }

    pub fn output_guardrail(mut self, g: impl crate::Guardrail<Ctx> + 'static) -> Self { … }
    pub fn shared_output_guardrail(mut self, g: std::sync::Arc<dyn crate::Guardrail<Ctx>>) -> Self { … }
    pub fn output_guardrails<I>(mut self, g: I) -> Self
    where I: IntoIterator<Item = std::sync::Arc<dyn crate::Guardrail<Ctx>>> { … }

    pub fn model_settings(mut self, s: crate::ModelSettings) -> Self {
        self.model_settings = s;
        self
    }

    pub fn max_turns(mut self, n: u32) -> Self {
        self.config.max_turns = n;
        self
    }
}
```

**Singular vs plural vs shared:** every collection field has three entry points, mirroring `.model` / `.shared_model`:

- `.tool(impl Tool<Ctx> + 'static)` — owned-value append, wraps in `Arc` internally. Ergonomic default.
- `.shared_tool(Arc<dyn Tool<Ctx>>)` — append a pre-wrapped trait object, for tools shared across multiple agents.
- `.tools(impl IntoIterator<Item = Arc<dyn Tool<Ctx>>>)` — replace the whole vec. Takes `IntoIterator` so both `vec![…]` and the SMA-315 `tools![…]` macro work.

Same triplet for `.handoff` / `.hook` / `.input_guardrail` / `.output_guardrail`. `.instructions` has the owned + shared pair only (single-valued, no plural).

The canonical call shapes are `.tool(a).tool(b)` (singular chain) or `.tools(tools![a, b])` (SMA-315 macro). The Notion page's `.tools([a, b])` example does not compile (heterogeneous unit types fail `[T; N]`'s homogeneity requirement); fix as a doc follow-up.

### Required transitions

```rust
// .name(…) — only callable when Name = NoName
impl<Ctx, M, T, Mo> LlmAgentBuilder<Ctx, M, T, NoName, Mo>
where
    Ctx: Send + Sync + 'static,
{
    pub fn name(self, n: impl Into<String>) -> LlmAgentBuilder<Ctx, M, T, HasName, Mo> {
        LlmAgentBuilder {
            name: Some(n.into()),
            description: self.description,
            instructions: self.instructions,
            model: self.model,
            tools: self.tools,
            handoffs: self.handoffs,
            output_type: self.output_type,
            input_guardrails: self.input_guardrails,
            output_guardrails: self.output_guardrails,
            hooks: self.hooks,
            model_settings: self.model_settings,
            config: self.config,
            _state: std::marker::PhantomData,
        }
    }
}

// .model(…) / .shared_model(…) — only callable when Model = NoModel
impl<Ctx, M0, T, N> LlmAgentBuilder<Ctx, M0, T, N, NoModel>
where
    Ctx: Send + Sync + 'static,
{
    pub fn model<M2>(self, m: M2) -> LlmAgentBuilder<Ctx, M2, T, N, HasModel>
    where
        M2: crate::Model + 'static,
    {
        self.shared_model(std::sync::Arc::new(m))
    }

    pub fn shared_model<M2>(self, m: std::sync::Arc<M2>) -> LlmAgentBuilder<Ctx, M2, T, N, HasModel>
    where
        M2: crate::Model + 'static,
    {
        LlmAgentBuilder {
            name: self.name,
            description: self.description,
            instructions: self.instructions,
            model: Some(m),
            tools: self.tools,
            handoffs: self.handoffs,
            output_type: self.output_type,
            input_guardrails: self.input_guardrails,
            output_guardrails: self.output_guardrails,
            hooks: self.hooks,
            model_settings: self.model_settings,
            config: self.config,
            _state: std::marker::PhantomData,
        }
    }
}
```

**Why `.model` delegates to `.shared_model`:** single source of truth for the transition. The `Arc::new(m)` wrap is the only difference.

**Why owned `M` is the default entry point:** the common case is one model per agent, constructed locally — wrapping it in `Arc` is friction. `.shared_model(Arc<M>)` is the explicit path when the model is already shared across multiple agents.

### Optional T transition

```rust
impl<Ctx, M, T0, N, Mo> LlmAgentBuilder<Ctx, M, T0, N, Mo>
where
    Ctx: Send + Sync + 'static,
{
    pub fn output_type<T2>(self) -> LlmAgentBuilder<Ctx, M, T2, N, Mo>
    where
        T2: Send + Sync + 'static + serde::de::DeserializeOwned + schemars::JsonSchema,
    {
        LlmAgentBuilder {
            name: self.name,
            description: self.description,
            instructions: self.instructions,
            model: self.model,
            tools: self.tools,
            handoffs: self.handoffs,
            output_type: Some(crate::OutputType::from_schema::<T2>()),
            input_guardrails: self.input_guardrails,
            output_guardrails: self.output_guardrails,
            hooks: self.hooks,
            model_settings: self.model_settings,
            config: self.config,
            _state: std::marker::PhantomData,
        }
    }
}
```

**Why not one-shot:** unlike `.name` / `.model`, `.output_type::<T>()` is callable on any state and any number of times (last call wins). Each call is its own typestate transition.

**Why `Send + Sync + 'static` here AND on `.build()`:** redundant for soundness but localizes the diagnostic. Without it, `.output_type::<Rc<u32>>()` compiles fine and the error fires later at `.build()`.

**`.output_type::<String>()` is degenerate but legal:** equivalent to never calling `.output_type` (modulo the `output_type` field being `Some(schema_for_string)`). Document in the method's rustdoc.

### `.build()` — only on the final state

```rust
impl<Ctx, M, T> LlmAgentBuilder<Ctx, M, T, HasName, HasModel>
where
    Ctx: Send + Sync + 'static,
    M: crate::Model + 'static,
    T: Send + Sync + 'static,
{
    pub fn build(self) -> LlmAgent<Ctx, M, T> {
        LlmAgent {
            name: self.name.expect("name is HasName"),
            description: self.description.unwrap_or_default(),
            instructions: self
                .instructions
                .unwrap_or_else(|| std::sync::Arc::new(String::new())),
            model: self.model.expect("model is HasModel"),
            tools: self.tools,
            handoffs: self.handoffs,
            output_type: self.output_type,
            input_guardrails: self.input_guardrails,
            output_guardrails: self.output_guardrails,
            hooks: self.hooks,
            model_settings: self.model_settings,
            config: self.config,
            _output: std::marker::PhantomData,
        }
    }
}
```

**Why `.expect(…)`:** the typestate guarantees `Some`, but Rust still requires an `unwrap`. Messages reference the typestate so a hypothetical panic is diagnostic.

**Why `T: Send + Sync + 'static`:** the `Agent<Ctx>` impl needs `T` sendable across the runner's await points. Matches what SMA-320 will need to flow `T` through `RunResult<T>`.

### Defaults & invariants

- `description`: defaults to `""` when `.description(…)` is not called. Setting matters in multi-agent setups — handoff targets render their description into the dispatching agent's prompt, so unset descriptions silently degrade routing quality.
- `instructions`: defaults to `Arc::new(String::new())`. An empty `String` renders as `""`, which `LlmAgent::run` already treats as "no system prompt" (it skips the `Item::System` push).
- All `Vec` fields default to empty.
- `model_settings`: `ModelSettings::default()`.
- `config.max_turns`: `16` (matches `RunConfig::default()`).
- `output_type`: `None`. `.output_type::<T>()` populates it via `OutputType::from_schema::<T>()` and transitions the `T` generic.
- One-shot vs repeatable:
  - `.name(…)`: one-shot (compile error if called twice).
  - `.model(…)` / `.shared_model(…)`: one-shot (compile error if called twice, regardless of which variant).
  - `.output_type::<T>()`: repeatable; last call wins.
  - `.description(…)` / `.instructions(…)` / `.shared_instructions(…)` / `.model_settings(…)` / `.max_turns(…)`: repeatable; last call wins.
  - Plural setters (`.tools`, `.handoffs`, …): replace.
  - Singular adders (`.tool`, `.handoff`, …) and shared-adders (`.shared_tool`, `.shared_handoff`, …): append.

### `Agent` impl update

```rust
#[async_trait::async_trait]
impl<Ctx, M, T> crate::Agent<Ctx> for LlmAgent<Ctx, M, T>
where
    Ctx: Send + Sync + 'static,
    M: crate::Model + 'static,
    T: Send + Sync + 'static,
{
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    async fn run(&self, ctx: crate::RunContext<Ctx>, input: AgentInput) -> Result<…> {
        // body unchanged — T does not appear
    }
}
```

The body is byte-identical to today's impl. Only the impl head gains `T` and the corresponding bound.

## Tests

### Unit tests in `agent_builder.rs`

Inline `#[cfg(test)] mod tests { … }` block. Each test runs on every CI matrix row (no trybuild dependency).

| Test | What it locks |
|---|---|
| `build_with_required_only` | Builder with only `.name`/`.model` produces an `LlmAgent` whose `description`, instructions render, `tools`, `output_type`, etc. are at default values. |
| `description_set_via_builder` | `.description("d")` produces `agent.description == "d"`; absent the call, `agent.description == ""`. |
| `singular_adders_append` | `.tool(a).tool(b)` → `tools.len() == 2` and order is preserved. Repeat for `.handoff`, `.hook`, `.input_guardrail`, `.output_guardrail`. |
| `shared_adders_append` | `.shared_tool(arc_a).shared_tool(arc_b)` → `tools.len() == 2`; uses `Arc::ptr_eq` to confirm the supplied Arcs are stored, not re-wrapped. |
| `plural_setters_replace` | `.tool(a).tools(vec![b])` → `tools == vec![b]`. Mirror across the other Vec fields. Both `Vec<…>` and `tools![…]`-style iterators accepted. |
| `max_turns_overrides_default` | `.max_turns(99)` → `config.max_turns == 99`; without it, `config.max_turns == 16`. |
| `output_type_populates_schema` | After `.output_type::<MyStruct>()`, `output_type.is_some()` and the schema's root matches `schemars::schema_for!(MyStruct)`. |
| `output_type_last_call_wins` | `.output_type::<A>().output_type::<B>()` → schema matches `B`. |
| `shared_model_avoids_double_arc` | `.shared_model(arc_clone)` does not wrap the Arc again — compared by `Arc::ptr_eq` against the input. |
| `shared_instructions_avoids_double_arc` | `.shared_instructions(arc_clone)` likewise stores the input Arc unchanged. |

These exercise the `Self`-returning path; the typestate-transition correctness is locked by trybuild.

### `trybuild` UI tests

New directory `crates/paigasus-helikon-core/tests/ui/`:

| File | Kind | What it locks |
|---|---|---|
| `builder_missing_name.rs` | compile-fail | `.model(m).build()` — `.build` not found on `LlmAgentBuilder<…, NoName, HasModel>`. |
| `builder_missing_model.rs` | compile-fail | `.name("x").build()` — `.build` not found on `LlmAgentBuilder<…, HasName, NoModel>`. |
| `builder_missing_both.rs` | compile-fail | `.build()` on the initial state — `.build` not found on `LlmAgentBuilder<…, NoName, NoModel>`. |
| `builder_name_twice.rs` | compile-fail | `.name("a").name("b")` — second `.name` not found on `<…, HasName, _>`. |
| `builder_model_twice.rs` | compile-fail | `.model(m1).model(m2)` — second `.model` not found on `<…, _, HasModel>`. |
| `builder_happy_path.rs` | pass | Full chain exercising every any-state setter at least once: `.name`, `.description`, `.instructions`, `.tool` (singular), `.tools` (plural via `vec![…]`), `.shared_tool`, `.handoff`, `.hook`, `.input_guardrail`, `.output_guardrail`, `.model_settings`, `.max_turns`, then `.model` and `.build`. Lock the full any-state surface in one fixture so future signature drift on any optional fails here. |
| `builder_output_type_typed.rs` | pass | `.output_type::<Answer>()` produces a `let _: LlmAgent<MyCtx, MockModel, Answer> = …` — binding to the explicit type proves `T` flows through. |

New harness `crates/paigasus-helikon-core/tests/trybuild_ui.rs`:

```rust
//! UI tests for the LlmAgent typestate builder. The workflow restricts
//! execution to the latest-stable CI matrix row (`.github/workflows/ci.yml`)
//! because trybuild `.stderr` snapshots pin rustc diagnostic text
//! byte-for-byte and that text drifts across rustc releases.

#[test]
fn trybuild_ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/builder_missing_*.rs");
    t.compile_fail("tests/ui/builder_*_twice.rs");
    t.pass("tests/ui/builder_happy_path.rs");
    t.pass("tests/ui/builder_output_type_typed.rs");
}
```

**CI gating:** the existing `--skip trybuild_ui` filter (substring match) catches both the macros harness and this new one. No workflow change needed.

**Mock model in fixtures:** each fixture needs a concrete `M` to pass to `.model(…)`. Inline a tiny `MockModel` per fixture (~10 lines: `impl Model` with a stub `invoke` returning `Err(ModelError::Unavailable)`). Self-contained — trybuild doesn't share state between files.

## Migration / blast radius

1. **`LlmAgent<Ctx, M>` → `LlmAgent<Ctx, M, T = String>`**: the default-generic parameter makes every existing *type* reference compile unchanged.
2. **New `_output: PhantomData<fn() -> T>` field**: soft-break for *struct-literal* construction of `LlmAgent`. Internal touch sites (`tests/loop_happy_path.rs`, `loop_parallel_tools.rs`) need one-line additions; the implementation plan enumerates them via `rg -n "LlmAgent\s*\{" crates/`. No downstream consumers exist yet (pre-1.0 workspace), so external blast radius is theoretical.
3. **Dropped `M: Model + 'static` bound from the struct definition**: bound moves to the `Agent<Ctx>` impl and the inherent impl that calls into `M::invoke`. No internal touch sites; deliberate narrowing.
4. **`Agent<Ctx>` impl head gains `T` and `T: Send + Sync + 'static`**: no call-site impact — trait surface unchanged.
5. **`paigasus-helikon-providers-openai` / `-anthropic` / `-macros`**: don't reference `LlmAgent`. Zero impact.
6. **Doc coverage**: ~18 new `pub` items (markers + builder struct + triplet methods). Each is trivially documentable; budget the chore in the plan to stay under the 80% gate.
7. **PR title**: `feat(core): SMA-319 add typestate builder for LlmAgent` (satisfies both `pr-title.yml` rules — lowercase verb after `SMA-319`, full `type(scope):` prefix).
8. **release-plz**: `paigasus-helikon-core` already at `0.1.0`. One `feat(core): SMA-319 …` commit drives the normal 0.1.0 → 0.2.0 minor bump.

## Open questions deferred to implementation

- **Inference of `Ctx`**: if the user calls only `.name(…).model(m).build()` with no Ctx-bearing values flowing through, the compiler can't infer `Ctx`. Document in the rustdoc that the first call to any Ctx-carrying setter (`.instructions`, `.tool`, etc.) pins `Ctx`; in their absence, the user must turbofish at `builder::<MyCtx>()` or annotate the `let` binding. This is an unavoidable property of the typestate design, not a defect.
