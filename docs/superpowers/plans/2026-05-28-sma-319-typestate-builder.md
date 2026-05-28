# SMA-319 — Typestate Builder for `LlmAgent` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the ergonomic typestate builder for `LlmAgent`, plus the structural change that the typed-output path (SMA-320) hangs off. After this plan, `LlmAgent::builder()` is the canonical construction path, the compiler statically refuses `.build()` without `.name(…)` and `.model(…)`, and `LlmAgent<Ctx, M>` becomes `LlmAgent<Ctx, M, T = String>` so `.output_type::<T>()` is a real typestate transition.

**Architecture:** Three commits on `feature/sma-319-typestate-builder-for-llmagent`, all scoped to `paigasus-helikon-core` (PR-squash collapses them into one `feat(core): SMA-319 …` minor bump). Phase A modifies `agent.rs` (drops the `M: Model + 'static` struct bound, adds the `T = String` generic + `_output` field + inherent `builder()` slot + the `T: Send + Sync + 'static` bound on the `Agent<Ctx>` impl), and updates the two struct-literal test touch sites in the same commit so the tree stays green between commits. Phase B creates `src/agent_builder.rs` with markers, the `LlmAgentBuilder` struct, all transitions, and inline unit tests; wires it through `lib.rs`. Phase C adds the `trybuild` dev-dep, the `trybuild_ui` harness, and seven UI fixtures.

**Tech Stack:** Rust 1.75 MSRV, `schemars` (existing — used by `OutputType::from_schema`), `serde` / `serde_json` (existing), `trybuild = 1` (existing in `[workspace.dependencies]` from SMA-315), `tokio` (dev-dep, existing).

**Design reference:** [`docs/superpowers/specs/2026-05-28-sma-319-typestate-builder-design.md`](../specs/2026-05-28-sma-319-typestate-builder-design.md).

**Branch:** `feature/sma-319-typestate-builder-for-llmagent` (already created and currently checked out).

---

## File structure

### Created
- `crates/paigasus-helikon-core/src/agent_builder.rs` — typestate markers (`NoName`, `HasName`, `NoModel`, `HasModel`), `LlmAgentBuilder<Ctx, M, T, N, Mo>`, all `impl` blocks (any-state setters, `.name`, `.model`/`.shared_model`, `.output_type<T>`, `.build`), inline `#[cfg(test)] mod tests`.
- `crates/paigasus-helikon-core/tests/trybuild_ui.rs` — trybuild harness for the UI fixtures.
- `crates/paigasus-helikon-core/tests/ui/builder_missing_name.rs` — compile-fail fixture.
- `crates/paigasus-helikon-core/tests/ui/builder_missing_model.rs` — compile-fail fixture.
- `crates/paigasus-helikon-core/tests/ui/builder_missing_both.rs` — compile-fail fixture.
- `crates/paigasus-helikon-core/tests/ui/builder_name_twice.rs` — compile-fail fixture.
- `crates/paigasus-helikon-core/tests/ui/builder_model_twice.rs` — compile-fail fixture.
- `crates/paigasus-helikon-core/tests/ui/builder_happy_path.rs` — pass fixture covering the full any-state surface.
- `crates/paigasus-helikon-core/tests/ui/builder_output_type_typed.rs` — pass fixture proving `T` flows to `LlmAgent<Ctx, M, T>`.

### Modified
- `crates/paigasus-helikon-core/src/agent.rs` — drop `M: Model + 'static` from struct `where`, add `T = String` generic + `_output: PhantomData<fn() -> T>` field, add `pub fn builder()` slot, add `T: Send + Sync + 'static` bound on the `Agent<Ctx>` impl head.
- `crates/paigasus-helikon-core/src/lib.rs` — `pub mod agent_builder;` + `pub use agent_builder::*;`.
- `crates/paigasus-helikon-core/Cargo.toml` — add `trybuild = { workspace = true }` to `[dev-dependencies]`.
- `crates/paigasus-helikon-core/tests/loop_happy_path.rs` — add `_output: std::marker::PhantomData,` to the `LlmAgent { … }` struct literal at line 21.
- `crates/paigasus-helikon-core/tests/loop_parallel_tools.rs` — same one-line addition to the `LlmAgent::<(), _> { … }` literal at line 50.

### Untouched (verified)
- `crates/paigasus-helikon-providers-openai`, `-anthropic` — they impl `Model`, never reference `LlmAgent` by type or construct it. Zero impact.
- `crates/paigasus-helikon-macros` — doesn't reference `LlmAgent`. Zero impact.
- `crates/paigasus-helikon-core/tests/common/mod.rs` — provides `MockModel`/`MockTool`/etc. for runtime tests; never constructs `LlmAgent`. Zero impact.
- `crates/paigasus-helikon/src/lib.rs` (facade) — already does `pub use paigasus_helikon_core::*;` unconditionally. New markers and builder become available automatically.
- `Cargo.toml` (workspace root) — `trybuild = "1"` already present in `[workspace.dependencies]` (added by SMA-315).

---

## Phase A — `agent.rs` generic widening + struct-literal touch-ups

Single commit at the end: `feat(core): SMA-319 widen LlmAgent generics for typestate builder`.

This phase only restructures the existing surface; it does not introduce the builder. After this phase the workspace builds and all existing tests pass — the struct-literal sites have to be updated in the same commit because dropping the `M: Model` bound is benign but adding the `_output` field is a soft-break for them.

### Task A.1: Drop the `M: Model + 'static` bound from the `LlmAgent` struct definition

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs:182-213`

- [ ] **Step 1: Open `agent.rs` and locate the `LlmAgent` struct**

The current struct (line 182):

```rust
pub struct LlmAgent<Ctx, M>
where
    Ctx: Send + Sync + 'static,
    M: crate::Model + 'static,
{
    /// Agent identifier (used in events and trace spans).
    pub name: String,
    // … 11 more fields …
    pub config: crate::RunConfig,
}
```

- [ ] **Step 2: Remove the `M: Model + 'static` bound from the struct's `where` clause**

Edit lines 182-186 so the struct definition becomes:

```rust
pub struct LlmAgent<Ctx, M, T = String>
where
    Ctx: Send + Sync + 'static,
{
```

Note three changes in one edit:
1. Add `, T = String` to the generics.
2. Remove the `M: crate::Model + 'static,` line entirely.
3. Leave the `Ctx: Send + Sync + 'static` line.

The original docstring above the struct ("Constructed via direct field assignment in SMA-314; the ergonomic typestate builder lands in SMA-319.") should be updated to reflect that SMA-319 has now landed — change "lands in SMA-319" to "lands via `LlmAgent::builder()`; struct-literal construction stays available as an escape hatch."

- [ ] **Step 3: Add the `_output: PhantomData<fn() -> T>` field**

After the last field (`pub config: crate::RunConfig,` at line 212), and before the closing `}` at line 213, add:

```rust
    /// SMA-319: marker for the structured-output type. Doesn't appear
    /// in any field's value — only exists so the builder can flow
    /// `T` across `.output_type::<T>()` transitions.
    _output: std::marker::PhantomData<fn() -> T>,
```

- [ ] **Step 4: Verify the struct compiles (with expected breakage downstream)**

```
cargo check -p paigasus-helikon-core --lib
```

Expected: `cargo check` succeeds for `--lib` alone (no test crates touched yet). The `Agent<Ctx>` impl for `LlmAgent<Ctx, M>` still has `LlmAgent<Ctx, M>` not `LlmAgent<Ctx, M, T>` — Rust resolves the missing third parameter to its default (`String`). This will fail in the next task without an explicit fix, but compilation of the struct alone is fine here.

### Task A.2: Update the `Agent<Ctx>` impl head to carry `T`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs:424-432`

- [ ] **Step 1: Locate the existing impl head**

Current (line 424):

```rust
#[async_trait::async_trait]
impl<Ctx, M> crate::Agent<Ctx> for LlmAgent<Ctx, M>
where
    Ctx: Send + Sync + 'static,
    M: crate::Model + 'static,
{
```

- [ ] **Step 2: Add the `T` generic and its bound**

Rewrite the impl head to:

```rust
#[async_trait::async_trait]
impl<Ctx, M, T> crate::Agent<Ctx> for LlmAgent<Ctx, M, T>
where
    Ctx: Send + Sync + 'static,
    M: crate::Model + 'static,
    T: Send + Sync + 'static,
{
```

Note: `M: crate::Model + 'static` stays on this impl — it's where we actually call `model.invoke()`, so the bound belongs here.

The body of the `name`, `description`, and `run` methods does NOT change. `T` does not appear in any method body — it's purely a marker.

- [ ] **Step 3: Verify the lib still builds (tests will still be broken)**

```
cargo build -p paigasus-helikon-core --lib
```

Expected: succeeds.

### Task A.3: Add the inherent `LlmAgent::builder()` docking point

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (insert after the `LlmAgent` struct definition, before the `AgentEvent` enum)

- [ ] **Step 1: Insert the inherent impl block**

After the closing `}` of the `LlmAgent` struct (line 213 in the original; after the new `_output` field), insert:

```rust
impl LlmAgent<(), (), String> {
    /// Construct a new [`LlmAgentBuilder`] in its initial state.
    ///
    /// `Ctx` is the per-run context type carried by [`RunContext`] —
    /// pass it as a turbofish if no setter call pins it implicitly
    /// (e.g. `.instructions(|ctx: &RunContext<MyCtx>| …)`).
    ///
    /// # Example
    ///
    /// ```ignore
    /// use paigasus_helikon_core::{LlmAgent, Model};
    ///
    /// # fn make_model() -> impl Model + 'static { unimplemented!() }
    /// let agent = LlmAgent::builder::<()>()
    ///     .name("triage")
    ///     .model(make_model())
    ///     .build();
    /// ```
    pub fn builder<Ctx>() -> crate::LlmAgentBuilder<Ctx, (), String, crate::NoName, crate::NoModel>
    where
        Ctx: Send + Sync + 'static,
    {
        crate::LlmAgentBuilder::__new()
    }
}
```

Notes:
- The head `LlmAgent<(), (), String>` is a pure docking point — `()` for both `Ctx` and `M` works because we dropped the `M: Model` bound from the struct. The `String` for `T` is the default.
- `LlmAgentBuilder::__new()` is an associated function Phase B introduces with `pub` visibility plus `#[doc(hidden)]` — `pub` is required because the wildcard re-export `pub use agent_builder::*;` in `lib.rs` won't pick up `pub(crate)` items, and the docking point in `agent.rs` reaches it via the re-export path. The double-underscore name + `#[doc(hidden)]` signals "internal, do not call from outside the crate" at the API-surface level.
- The example uses ` ```no_run ``` ` so rustdoc compiles it (catching future API drift) without trying to execute `unimplemented!()`.

- [ ] **Step 2: Verify the lib still type-checks (the `LlmAgentBuilder` reference will fail until Phase B)**

```
cargo check -p paigasus-helikon-core --lib
```

Expected: fails with "cannot find type `LlmAgentBuilder` in module `crate`" or similar. **This is expected** — Phase B introduces the builder. Do not try to fix this here; commit happens at the end of the phase, after Phase B has landed and the symbol resolves.

(If executing phases as separate commits, defer this task — and Tasks A.4/A.5 — to land just before the Phase B commit so the tree never has a broken intermediate state. The simpler convention: treat all of Phase A + Phase B as one logical commit, and run `cargo check` only at the phase boundary.)

### Task A.4: Update `tests/loop_happy_path.rs` struct literal

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/loop_happy_path.rs:21-34`

- [ ] **Step 1: Locate the struct literal**

Current (line 21-34):

```rust
    LlmAgent {
        name: "test".into(),
        description: "test agent".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools: Vec::new(),
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
    }
```

- [ ] **Step 2: Add the `_output` field before the closing brace**

Insert one line after `config: RunConfig::default(),`:

```rust
        _output: std::marker::PhantomData,
```

The full literal becomes:

```rust
    LlmAgent {
        name: "test".into(),
        description: "test agent".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools: Vec::new(),
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    }
```

The surrounding function signature `fn build_agent<M>(model: Arc<M>) -> LlmAgent<(), M>` does not need to change — the third generic parameter defaults to `String`.

### Task A.5: Update `tests/loop_parallel_tools.rs` struct literal

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/loop_parallel_tools.rs:50-66`

- [ ] **Step 1: Locate the struct literal**

Current (line 50-66):

```rust
    let agent = LlmAgent::<(), _> {
        name: "test".into(),
        description: "parallel test".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools: vec![
            tool_a as Arc<dyn paigasus_helikon_core::Tool<()>>,
            tool_b as Arc<dyn paigasus_helikon_core::Tool<()>>,
        ],
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
    };
```

- [ ] **Step 2: Add the `_output` field before the closing brace**

Insert one line after `config: RunConfig::default(),`:

```rust
        _output: std::marker::PhantomData,
```

The `LlmAgent::<(), _>` annotation is fine; the third generic still defaults to `String`.

### Task A.6: (Deferred) Phase A commit

The commit for Phase A lands **after** Phase B completes — Phase A's Task A.3 leaves the lib in a broken intermediate state (references `crate::LlmAgentBuilder` which Phase B introduces). When following the strict TDD-per-task workflow, run `cargo build --workspace` only at the Phase B boundary. The Phase A commit subject lands at the end of Phase B as part of a combined `feat(core): SMA-319 …` commit.

---

## Phase B — `agent_builder.rs` module + wiring

Single commit at the end of this phase combines Phase A and B: `feat(core): SMA-319 add typestate builder for LlmAgent`. (Phase A's diff is the structural prep; Phase B's diff is the meat. They land together because Phase A is incomplete on its own.)

### Task B.1: Create the empty module file and wire it through `lib.rs`

**Files:**
- Create: `crates/paigasus-helikon-core/src/agent_builder.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs`

- [ ] **Step 1: Create the empty module file**

Create `crates/paigasus-helikon-core/src/agent_builder.rs` with this initial content:

```rust
//! Typestate builder for [`crate::LlmAgent`]. See [SMA-319 design] for
//! the full rationale.
//!
//! [SMA-319 design]: https://github.com/SMK1085/paigasus-helikon/blob/main/docs/superpowers/specs/2026-05-28-sma-319-typestate-builder-design.md

// Phantom typestate markers and the builder land in subsequent steps.
```

- [ ] **Step 2: Wire the module through `lib.rs`**

Open `crates/paigasus-helikon-core/src/lib.rs`. Find the existing `pub mod` lines (after the crate-level docstring, around line 17). Add `agent_builder` alphabetically — between `agent` and `context`:

```rust
pub mod agent;
pub mod agent_builder;
pub mod context;
```

Then find the corresponding `pub use` block (around line 31) and add the wildcard re-export alphabetically — between `agent::*;` and `context::*;`:

```rust
pub use agent::*;
pub use agent_builder::*;
pub use context::*;
```

- [ ] **Step 3: Verify the empty module compiles**

```
cargo check -p paigasus-helikon-core --lib
```

Expected: still fails (Task A.3's reference to `crate::LlmAgentBuilder` is unresolved). That's fine — we'll get green at the end of Task B.6.

### Task B.2: Add the typestate markers and the `LlmAgentBuilder` struct

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent_builder.rs`

- [ ] **Step 1: Add the four marker types**

Append to `agent_builder.rs`:

```rust
/// Typestate marker: `.name(…)` has not been called yet.
#[derive(Debug)]
pub struct NoName;

/// Typestate marker: `.name(…)` has been called; `.build()` is now reachable
/// once `HasModel` is also satisfied.
#[derive(Debug)]
pub struct HasName;

/// Typestate marker: `.model(…)` / `.shared_model(…)` has not been called yet.
#[derive(Debug)]
pub struct NoModel;

/// Typestate marker: `.model(…)` / `.shared_model(…)` has been called; `.build()`
/// is now reachable once `HasName` is also satisfied.
#[derive(Debug)]
pub struct HasModel;
```

- [ ] **Step 2: Add the builder struct definition**

After the markers, add:

```rust
/// Typestate-driven builder for [`crate::LlmAgent`].
///
/// Constructed via [`crate::LlmAgent::builder()`]. `Ctx` is the per-run
/// context type; `M` is the concrete [`crate::Model`] implementation
/// (inferred from `.model(m)`); `T` is the structured-output type
/// (defaults to `String`; switched by `.output_type::<T>()`); `N` and
/// `Mo` are the typestate markers tracking which required setters have
/// been called.
///
/// `.build()` only exists once both `N = HasName` and `Mo = HasModel`.
/// Trying to `.build()` earlier is a compile error.
pub struct LlmAgentBuilder<Ctx, M, T, N, Mo>
where
    Ctx: Send + Sync + 'static,
{
    name: Option<String>,
    description: Option<String>,
    instructions: Option<std::sync::Arc<dyn crate::Instructions<Ctx>>>,
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

- [ ] **Step 3: Add the private `__new()` constructor**

This is what `LlmAgent::builder()` (Task A.3) calls. Append:

```rust
impl<Ctx> LlmAgentBuilder<Ctx, (), String, NoName, NoModel>
where
    Ctx: Send + Sync + 'static,
{
    /// Internal initial-state constructor. Called by
    /// [`crate::LlmAgent::builder()`]; not part of the public API
    /// (the double underscore is a "don't call from outside the
    /// crate" signal even though the method is `pub` for cross-module
    /// access).
    #[doc(hidden)]
    pub fn __new() -> Self {
        Self {
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

- [ ] **Step 4: Verify the lib compiles**

```
cargo check -p paigasus-helikon-core --lib
```

Expected: succeeds. The lib now exposes the marker types and the empty builder. The Phase A `LlmAgent::builder()` slot resolves correctly because `crate::LlmAgentBuilder` and `crate::NoName` / `crate::NoModel` exist via the wildcard re-export.

### Task B.3: Add any-state optional setters

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent_builder.rs`

- [ ] **Step 1: Write the failing test**

Append at the bottom of `agent_builder.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CancellationToken, Instructions, LlmAgent, Model, ModelCapabilities, ModelError,
        ModelEvent, ModelRequest, Tool, ToolContext, ToolError, ToolOutput,
    };
    use async_trait::async_trait;
    use futures_core::stream::BoxStream;
    use std::sync::Arc;

    // ── Tiny stubs that exist solely to compile against the typestate API.
    // The trybuild fixtures cover the *typestate* error surface; these unit
    // tests cover the *behavioral* surface (field plumbing, defaults).

    struct StubModel;
    #[async_trait]
    impl Model for StubModel {
        async fn invoke(
            &self,
            _r: ModelRequest,
            _c: CancellationToken,
        ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
            Err(ModelError::Unavailable)
        }
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities::default()
        }
    }

    struct StubTool;
    #[async_trait]
    impl<Ctx> Tool<Ctx> for StubTool
    where
        Ctx: Send + Sync + 'static,
    {
        fn name(&self) -> &str { "stub" }
        fn description(&self) -> &str { "stub tool" }
        fn schema(&self) -> &serde_json::Value {
            static S: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
            S.get_or_init(|| serde_json::json!({"type":"object"}))
        }
        async fn invoke(
            &self,
            _c: &ToolContext<Ctx>,
            _a: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput {
                content: serde_json::Value::String("ok".into()),
            })
        }
    }

    #[test]
    fn description_set_via_builder() {
        let agent = LlmAgent::builder::<()>()
            .description("triage agent")
            .name("triage")
            .model(StubModel)
            .build();
        assert_eq!(agent.description, "triage agent");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```
cargo test -p paigasus-helikon-core --lib agent_builder::tests::description_set_via_builder
```

Expected: FAIL — `.description` method does not exist on the builder.

- [ ] **Step 3: Implement the any-state setters**

Insert this `impl` block just before the `#[cfg(test)] mod tests` block:

```rust
// Any-state setters: callable in every typestate combination, return Self
// unchanged in (N, Mo, T) generics. Each takes `mut self`, mutates a field,
// returns Self.
impl<Ctx, M, T, N, Mo> LlmAgentBuilder<Ctx, M, T, N, Mo>
where
    Ctx: Send + Sync + 'static,
{
    /// Set the agent's human-readable description.
    ///
    /// Used by handoff targets when their parent agent's prompt is being
    /// rendered. Defaults to `""` if unset; setting it improves multi-agent
    /// routing quality.
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }

    /// Set the agent's system-prompt renderer.
    ///
    /// `Instructions` is implemented for `String`, `&'static str`, and any
    /// `Fn(&RunContext<Ctx>) -> String + Send + Sync`. The value is wrapped
    /// in an `Arc` internally — use [`Self::shared_instructions`] if you
    /// already hold an `Arc<dyn Instructions<Ctx>>`.
    pub fn instructions(mut self, i: impl crate::Instructions<Ctx> + 'static) -> Self {
        self.instructions = Some(std::sync::Arc::new(i));
        self
    }

    /// Set the agent's system-prompt renderer from a pre-wrapped trait object.
    ///
    /// Use this when the same `Instructions` impl is shared across multiple
    /// agents — avoids re-wrapping in another `Arc`.
    pub fn shared_instructions(
        mut self,
        i: std::sync::Arc<dyn crate::Instructions<Ctx>>,
    ) -> Self {
        self.instructions = Some(i);
        self
    }

    /// Append a tool to the agent's tool registry.
    ///
    /// Takes an owned value; wraps in `Arc` internally. Use
    /// [`Self::shared_tool`] for pre-wrapped trait objects.
    pub fn tool(mut self, t: impl crate::Tool<Ctx> + 'static) -> Self {
        self.tools
            .push(std::sync::Arc::new(t) as std::sync::Arc<dyn crate::Tool<Ctx>>);
        self
    }

    /// Append a pre-wrapped tool to the agent's tool registry.
    pub fn shared_tool(mut self, t: std::sync::Arc<dyn crate::Tool<Ctx>>) -> Self {
        self.tools.push(t);
        self
    }

    /// Replace the agent's tool registry with the supplied iterable.
    ///
    /// Accepts `Vec<Arc<dyn Tool<Ctx>>>`, the SMA-315 `tools![…]` macro
    /// output, or any other `IntoIterator`.
    pub fn tools<I>(mut self, t: I) -> Self
    where
        I: IntoIterator<Item = std::sync::Arc<dyn crate::Tool<Ctx>>>,
    {
        self.tools = t.into_iter().collect();
        self
    }

    /// Append a handoff candidate.
    pub fn handoff(mut self, h: impl crate::Agent<Ctx> + 'static) -> Self {
        self.handoffs
            .push(std::sync::Arc::new(h) as std::sync::Arc<dyn crate::Agent<Ctx>>);
        self
    }

    /// Append a pre-wrapped handoff candidate.
    pub fn shared_handoff(mut self, h: std::sync::Arc<dyn crate::Agent<Ctx>>) -> Self {
        self.handoffs.push(h);
        self
    }

    /// Replace the handoff candidate list.
    pub fn handoffs<I>(mut self, h: I) -> Self
    where
        I: IntoIterator<Item = std::sync::Arc<dyn crate::Agent<Ctx>>>,
    {
        self.handoffs = h.into_iter().collect();
        self
    }

    /// Append a lifecycle hook.
    pub fn hook(mut self, h: impl crate::Hook<Ctx> + 'static) -> Self {
        self.hooks
            .push(std::sync::Arc::new(h) as std::sync::Arc<dyn crate::Hook<Ctx>>);
        self
    }

    /// Append a pre-wrapped lifecycle hook.
    pub fn shared_hook(mut self, h: std::sync::Arc<dyn crate::Hook<Ctx>>) -> Self {
        self.hooks.push(h);
        self
    }

    /// Replace the hook list.
    pub fn hooks<I>(mut self, h: I) -> Self
    where
        I: IntoIterator<Item = std::sync::Arc<dyn crate::Hook<Ctx>>>,
    {
        self.hooks = h.into_iter().collect();
        self
    }

    /// Append an input guardrail.
    pub fn input_guardrail(mut self, g: impl crate::Guardrail<Ctx> + 'static) -> Self {
        self.input_guardrails
            .push(std::sync::Arc::new(g) as std::sync::Arc<dyn crate::Guardrail<Ctx>>);
        self
    }

    /// Append a pre-wrapped input guardrail.
    pub fn shared_input_guardrail(
        mut self,
        g: std::sync::Arc<dyn crate::Guardrail<Ctx>>,
    ) -> Self {
        self.input_guardrails.push(g);
        self
    }

    /// Replace the input-guardrail list.
    pub fn input_guardrails<I>(mut self, g: I) -> Self
    where
        I: IntoIterator<Item = std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    {
        self.input_guardrails = g.into_iter().collect();
        self
    }

    /// Append an output guardrail.
    pub fn output_guardrail(mut self, g: impl crate::Guardrail<Ctx> + 'static) -> Self {
        self.output_guardrails
            .push(std::sync::Arc::new(g) as std::sync::Arc<dyn crate::Guardrail<Ctx>>);
        self
    }

    /// Append a pre-wrapped output guardrail.
    pub fn shared_output_guardrail(
        mut self,
        g: std::sync::Arc<dyn crate::Guardrail<Ctx>>,
    ) -> Self {
        self.output_guardrails.push(g);
        self
    }

    /// Replace the output-guardrail list.
    pub fn output_guardrails<I>(mut self, g: I) -> Self
    where
        I: IntoIterator<Item = std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    {
        self.output_guardrails = g.into_iter().collect();
        self
    }

    /// Replace the [`crate::ModelSettings`] applied to every model call.
    pub fn model_settings(mut self, s: crate::ModelSettings) -> Self {
        self.model_settings = s;
        self
    }

    /// Set the per-run `max_turns` budget.
    ///
    /// Equivalent to constructing a [`crate::RunConfig`] with the specified
    /// `max_turns` and passing it via `.config(…)` (SMA-321 will add the
    /// full `.config` setter).
    pub fn max_turns(mut self, n: u32) -> Self {
        self.config.max_turns = n;
        self
    }
}
```

- [ ] **Step 4: Run the test to verify it passes (and the lib still builds)**

```
cargo test -p paigasus-helikon-core --lib agent_builder::tests::description_set_via_builder
```

Expected: PASS. Also run `cargo check -p paigasus-helikon-core --tests` to confirm the integration tests (already touched in Phase A) still compile.

### Task B.4: Add the `.name(…)` required transition

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent_builder.rs`

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn name_transitions_to_has_name() {
        // If this compiles, the transition typestate is correctly wired.
        // The downstream `.build()` requires HasName + HasModel, so we
        // chain `.model(…).build()` to prove the resulting builder is
        // in the right state.
        let agent = LlmAgent::builder::<()>()
            .name("triage")
            .model(StubModel)
            .build();
        assert_eq!(agent.name, "triage");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```
cargo test -p paigasus-helikon-core --lib agent_builder::tests::name_transitions_to_has_name
```

Expected: FAIL — `.name` method does not exist on the builder.

- [ ] **Step 3: Implement the `.name(…)` transition**

Append to `agent_builder.rs`, after the any-state setters block:

```rust
// .name(…) — only callable when the Name marker is NoName. Transitions
// to HasName, leaving every other generic parameter unchanged.
impl<Ctx, M, T, Mo> LlmAgentBuilder<Ctx, M, T, NoName, Mo>
where
    Ctx: Send + Sync + 'static,
{
    /// Set the agent's name and transition the typestate to `HasName`.
    ///
    /// Once called, `.name` is no longer in scope — calling it a second
    /// time is a compile error.
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
```

- [ ] **Step 4: Run the test to verify it passes**

```
cargo test -p paigasus-helikon-core --lib agent_builder::tests::name_transitions_to_has_name
```

Expected: still fails — `.model` and `.build` aren't implemented yet. Defer until the next task makes `.model` available. (The "FAIL" reason will mention `.model` not the test assertion — that's the expected progression.)

### Task B.5: Add the `.model(…)` / `.shared_model(…)` required transitions

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent_builder.rs`

- [ ] **Step 1: Implement the transitions**

Append to `agent_builder.rs`, after the `.name` impl:

```rust
// .model(…) / .shared_model(…) — only callable when the Model marker is
// NoModel. Transition consumes self and rebuilds with the new M2 generic
// inferred from the model argument.
impl<Ctx, M0, T, N> LlmAgentBuilder<Ctx, M0, T, N, NoModel>
where
    Ctx: Send + Sync + 'static,
{
    /// Set the agent's model from an owned value.
    ///
    /// `M2` is inferred from the argument type; the builder transitions
    /// to `LlmAgentBuilder<Ctx, M2, T, N, HasModel>`. Wraps the value in
    /// an `Arc` internally — use [`Self::shared_model`] if the model is
    /// already shared across multiple agents.
    pub fn model<M2>(self, m: M2) -> LlmAgentBuilder<Ctx, M2, T, N, HasModel>
    where
        M2: crate::Model + 'static,
    {
        self.shared_model(std::sync::Arc::new(m))
    }

    /// Set the agent's model from a pre-wrapped `Arc`.
    ///
    /// Stores the supplied `Arc` directly — no re-wrapping.
    pub fn shared_model<M2>(
        self,
        m: std::sync::Arc<M2>,
    ) -> LlmAgentBuilder<Ctx, M2, T, N, HasModel>
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

- [ ] **Step 2: Verify the lib still builds**

```
cargo check -p paigasus-helikon-core --lib
```

Expected: succeeds. Tests still fail because `.build()` doesn't exist yet.

### Task B.6: Add the `.build()` impl

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent_builder.rs`

- [ ] **Step 1: Implement `.build()`**

Append to `agent_builder.rs`, after the `.model` impl:

```rust
// .build() — only available on the fully-constructed state. The typestate
// guarantees `.name` and `.model` were both called, so the corresponding
// `Option`s are `Some`. We `.expect` with typestate-referencing messages
// for diagnostic clarity if the unreachable ever fires.
impl<Ctx, M, T> LlmAgentBuilder<Ctx, M, T, HasName, HasModel>
where
    Ctx: Send + Sync + 'static,
    M: crate::Model + 'static,
    T: Send + Sync + 'static,
{
    /// Finalize the builder into an [`crate::LlmAgent`].
    ///
    /// Only available when the builder has transitioned to both
    /// `HasName` and `HasModel`. Earlier states do not have a `.build`
    /// method in scope — `cargo build` fails with a clear error.
    pub fn build(self) -> crate::LlmAgent<Ctx, M, T> {
        crate::LlmAgent {
            name: self.name.expect("typestate HasName guarantees Some"),
            description: self.description.unwrap_or_default(),
            instructions: self
                .instructions
                .unwrap_or_else(|| std::sync::Arc::new(String::new())),
            model: self.model.expect("typestate HasModel guarantees Some"),
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

- [ ] **Step 2: Run both prior tests to verify they pass**

```
cargo test -p paigasus-helikon-core --lib agent_builder::tests
```

Expected: PASS for `description_set_via_builder` and `name_transitions_to_has_name`.

### Task B.7: Add the `.output_type::<T>()` transition

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent_builder.rs`

- [ ] **Step 1: Write the failing tests**

Add to the test module:

```rust
    #[derive(Debug, Default, PartialEq, serde::Deserialize, schemars::JsonSchema)]
    struct Answer { value: u32 }

    #[derive(Debug, Default, PartialEq, serde::Deserialize, schemars::JsonSchema)]
    struct Score { points: u32 }

    #[test]
    fn output_type_populates_schema() {
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .output_type::<Answer>()
            .build();
        let expected = serde_json::to_value(schemars::schema_for!(Answer)).unwrap();
        let actual = serde_json::to_value(&agent.output_type.unwrap().schema).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn output_type_last_call_wins() {
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .output_type::<Answer>()
            .output_type::<Score>()
            .build();
        let expected = serde_json::to_value(schemars::schema_for!(Score)).unwrap();
        let actual = serde_json::to_value(&agent.output_type.unwrap().schema).unwrap();
        assert_eq!(actual, expected);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```
cargo test -p paigasus-helikon-core --lib agent_builder::tests::output_type
```

Expected: FAIL — `.output_type` method does not exist.

- [ ] **Step 3: Implement the `.output_type::<T>()` transition**

Append to `agent_builder.rs`, after the `.build` impl:

```rust
// .output_type::<T>() — any-state, repeatable. Each call is a typestate
// transition that swaps the T generic and populates the OutputType schema.
impl<Ctx, M, T0, N, Mo> LlmAgentBuilder<Ctx, M, T0, N, Mo>
where
    Ctx: Send + Sync + 'static,
{
    /// Switch the structured-output type to `T2`.
    ///
    /// `T2 = String` (the default) is a no-op semantically (the
    /// `output_type` field becomes `Some(schema_for_string)`, which the
    /// runner treats the same as the default); pass any other `T2` to
    /// configure structured output. The runtime wiring lands in SMA-320.
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

- [ ] **Step 4: Run the tests to verify they pass**

```
cargo test -p paigasus-helikon-core --lib agent_builder::tests::output_type
```

Expected: PASS for both.

### Task B.8: Add the remaining behavioral unit tests

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent_builder.rs`

- [ ] **Step 1: Add the remaining tests**

Append to the test module:

```rust
    #[test]
    fn build_with_required_only_uses_defaults() {
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .build();
        assert_eq!(agent.description, "");
        assert!(agent.tools.is_empty());
        assert!(agent.handoffs.is_empty());
        assert!(agent.hooks.is_empty());
        assert!(agent.input_guardrails.is_empty());
        assert!(agent.output_guardrails.is_empty());
        assert!(agent.output_type.is_none());
        assert_eq!(agent.config.max_turns, 16);
    }

    #[test]
    fn singular_tool_adders_append() {
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .tool(StubTool)
            .tool(StubTool)
            .build();
        assert_eq!(agent.tools.len(), 2);
    }

    #[test]
    fn plural_tools_setter_replaces() {
        let pre: Vec<Arc<dyn Tool<()>>> = vec![Arc::new(StubTool)];
        let post: Vec<Arc<dyn Tool<()>>> = vec![Arc::new(StubTool), Arc::new(StubTool)];
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .tools(pre)
            .tools(post) // second call replaces
            .build();
        assert_eq!(agent.tools.len(), 2);
    }

    #[test]
    fn shared_tool_does_not_double_wrap() {
        let shared: Arc<dyn Tool<()>> = Arc::new(StubTool);
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .shared_tool(Arc::clone(&shared))
            .build();
        assert_eq!(agent.tools.len(), 1);
        assert!(Arc::ptr_eq(&agent.tools[0], &shared));
    }

    #[test]
    fn shared_model_does_not_double_wrap() {
        let shared: Arc<StubModel> = Arc::new(StubModel);
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .shared_model(Arc::clone(&shared))
            .build();
        assert!(Arc::ptr_eq(&agent.model, &shared));
    }

    #[test]
    fn shared_instructions_does_not_double_wrap() {
        let shared: Arc<dyn Instructions<()>> =
            Arc::new(String::from("you are helpful"));
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .shared_instructions(Arc::clone(&shared))
            .build();
        assert!(Arc::ptr_eq(&agent.instructions, &shared));
    }

    #[test]
    fn max_turns_overrides_default() {
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .max_turns(99)
            .build();
        assert_eq!(agent.config.max_turns, 99);
    }
```

- [ ] **Step 2: Run all unit tests**

```
cargo test -p paigasus-helikon-core --lib agent_builder
```

Expected: all PASS.

### Task B.9: Run the full workspace test suite, then commit Phase A + B together

- [ ] **Step 1: Run the full workspace test suite**

```
cargo test --workspace --all-features
```

Expected: all tests pass (including the previously-existing `loop_happy_path` / `loop_parallel_tools` integration tests, which were updated in Phase A).

- [ ] **Step 2: Run formatters and lints**

```
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: both succeed.

- [ ] **Step 3: Run rustdoc to verify all new `pub` items have doc comments**

```
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: succeeds. If any `missing_docs` warning fires, find the item and add a `///` comment.

- [ ] **Step 4: Stage the Phase A + B changes**

```
git add crates/paigasus-helikon-core/src/agent.rs \
        crates/paigasus-helikon-core/src/agent_builder.rs \
        crates/paigasus-helikon-core/src/lib.rs \
        crates/paigasus-helikon-core/tests/loop_happy_path.rs \
        crates/paigasus-helikon-core/tests/loop_parallel_tools.rs
```

- [ ] **Step 5: Commit Phase A + B**

```
git commit -m "$(cat <<'EOF'
feat(core): SMA-319 add typestate builder for LlmAgent

Add LlmAgentBuilder with phantom typestates (NoName/HasName,
NoModel/HasModel) that enforce required fields at compile time.
.build() only resolves when both .name and .model have been called;
trybuild compile-fail fixtures lock the error surface (added in
follow-up commit).

Widen LlmAgent<Ctx, M> to LlmAgent<Ctx, M, T = String> so
.output_type::<T>() is a real typestate transition that flows T
into the agent type. SMA-320 plumbs T through the runner /
RunResult chain; SMA-319 freezes the public surface.

Drop the M: Model + 'static bound from the LlmAgent struct
definition (kept on the Agent<Ctx> impl) so the inherent
docking point impl LlmAgent<(), (), String> compiles.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase C — trybuild dev-dep + UI fixtures

Single commit at the end: `test(core): SMA-319 add trybuild UI fixtures for typestate builder`. (Separate commit from Phase B because release-plz reads commit-prefix `test(…)` as no version bump — but the squashed PR commit keeps the `feat(core)` from Phase B as the canonical bump-trigger.)

### Task C.1: Add `trybuild` to `paigasus-helikon-core`'s dev-dependencies

**Files:**
- Modify: `crates/paigasus-helikon-core/Cargo.toml`

- [ ] **Step 1: Open `crates/paigasus-helikon-core/Cargo.toml`**

The current `[dev-dependencies]` block:

```toml
[dev-dependencies]
insta        = { workspace = true, features = ["yaml", "json"] }
schemars     = { workspace = true }
serde_json   = { workspace = true }
tokio        = { workspace = true, features = ["macros", "rt-multi-thread", "time", "sync"] }
```

- [ ] **Step 2: Add `trybuild` alphabetically (last)**

The result:

```toml
[dev-dependencies]
insta        = { workspace = true, features = ["yaml", "json"] }
schemars     = { workspace = true }
serde_json   = { workspace = true }
tokio        = { workspace = true, features = ["macros", "rt-multi-thread", "time", "sync"] }
trybuild     = { workspace = true }
```

- [ ] **Step 3: Verify it resolves**

```
cargo check -p paigasus-helikon-core --tests
```

Expected: succeeds. (`trybuild` adds no compile-time work to the test crate itself; the cost is per-fixture during `cargo test`.)

### Task C.2: Create the trybuild harness

**Files:**
- Create: `crates/paigasus-helikon-core/tests/trybuild_ui.rs`

- [ ] **Step 1: Write the harness**

Create `crates/paigasus-helikon-core/tests/trybuild_ui.rs`:

```rust
//! UI tests for the LlmAgent typestate builder. The workflow restricts
//! execution to the latest-stable CI matrix row (via the existing
//! `--skip trybuild_ui` filter in `.github/workflows/ci.yml`) because
//! trybuild `.stderr` snapshots pin rustc diagnostic text byte-for-byte
//! and that text drifts across rustc releases.

#[test]
fn trybuild_ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/builder_missing_*.rs");
    t.compile_fail("tests/ui/builder_*_twice.rs");
    t.pass("tests/ui/builder_happy_path.rs");
    t.pass("tests/ui/builder_output_type_typed.rs");
}
```

### Task C.3: Add the compile-fail fixtures

**Files:**
- Create: `crates/paigasus-helikon-core/tests/ui/builder_missing_name.rs`
- Create: `crates/paigasus-helikon-core/tests/ui/builder_missing_model.rs`
- Create: `crates/paigasus-helikon-core/tests/ui/builder_missing_both.rs`
- Create: `crates/paigasus-helikon-core/tests/ui/builder_name_twice.rs`
- Create: `crates/paigasus-helikon-core/tests/ui/builder_model_twice.rs`

Each fixture is self-contained (trybuild doesn't share state between files). Each declares a tiny `MockModel` struct in-file.

- [ ] **Step 1: Create `builder_missing_name.rs`**

```rust
//! `.model(m).build()` without `.name(…)` first — `.build` is not
//! reachable on `LlmAgentBuilder<…, NoName, HasModel>`.

use paigasus_helikon_core::{
    CancellationToken, LlmAgent, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

struct MockModel;

#[async_trait::async_trait]
impl Model for MockModel {
    async fn invoke(
        &self,
        _r: ModelRequest,
        _c: CancellationToken,
    ) -> Result<
        futures_core::stream::BoxStream<'static, Result<ModelEvent, ModelError>>,
        ModelError,
    > {
        Err(ModelError::Unavailable)
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

fn main() {
    let _ = LlmAgent::builder::<()>()
        .model(MockModel)
        .build();
}
```

- [ ] **Step 2: Create `builder_missing_model.rs`**

```rust
//! `.name("x").build()` without `.model(…)` first — `.build` is not
//! reachable on `LlmAgentBuilder<…, HasName, NoModel>`.

use paigasus_helikon_core::LlmAgent;

fn main() {
    let _ = LlmAgent::builder::<()>()
        .name("triage")
        .build();
}
```

- [ ] **Step 3: Create `builder_missing_both.rs`**

```rust
//! `.build()` on the initial state — `.build` is not reachable on
//! `LlmAgentBuilder<…, NoName, NoModel>`.

use paigasus_helikon_core::LlmAgent;

fn main() {
    let _ = LlmAgent::builder::<()>().build();
}
```

- [ ] **Step 4: Create `builder_name_twice.rs`**

```rust
//! `.name("a").name("b")` — the second `.name` is not in scope once
//! the typestate has transitioned to `HasName`.

use paigasus_helikon_core::LlmAgent;

fn main() {
    let _ = LlmAgent::builder::<()>()
        .name("first")
        .name("second");
}
```

- [ ] **Step 5: Create `builder_model_twice.rs`**

```rust
//! `.model(m1).model(m2)` — the second `.model` is not in scope once
//! the typestate has transitioned to `HasModel`.

use paigasus_helikon_core::{
    CancellationToken, LlmAgent, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

struct MockModel;

#[async_trait::async_trait]
impl Model for MockModel {
    async fn invoke(
        &self,
        _r: ModelRequest,
        _c: CancellationToken,
    ) -> Result<
        futures_core::stream::BoxStream<'static, Result<ModelEvent, ModelError>>,
        ModelError,
    > {
        Err(ModelError::Unavailable)
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

fn main() {
    let _ = LlmAgent::builder::<()>()
        .model(MockModel)
        .model(MockModel);
}
```

### Task C.4: Add the pass fixtures

**Files:**
- Create: `crates/paigasus-helikon-core/tests/ui/builder_happy_path.rs`
- Create: `crates/paigasus-helikon-core/tests/ui/builder_output_type_typed.rs`

- [ ] **Step 1: Create `builder_happy_path.rs`**

Exercise every any-state setter to lock the surface:

```rust
//! Full builder chain exercising every any-state setter at least
//! once, then `.model` and `.build`. Future signature drift on any
//! optional fails here.

use std::sync::Arc;

use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, CancellationToken, Guardrail, GuardrailError,
    GuardrailInput, GuardrailVerdict, Hook, HookDecision, HookEvent, LlmAgent, Model,
    ModelCapabilities, ModelError, ModelEvent, ModelRequest, ModelSettings, RunContext, Tool,
    ToolContext, ToolError, ToolOutput,
};

struct MockModel;

#[async_trait::async_trait]
impl Model for MockModel {
    async fn invoke(
        &self,
        _r: ModelRequest,
        _c: CancellationToken,
    ) -> Result<
        futures_core::stream::BoxStream<'static, Result<ModelEvent, ModelError>>,
        ModelError,
    > {
        Err(ModelError::Unavailable)
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

struct MockTool;

#[async_trait::async_trait]
impl<Ctx> Tool<Ctx> for MockTool
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str { "mock" }
    fn description(&self) -> &str { "mock tool" }
    fn schema(&self) -> &serde_json::Value {
        static S: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
        S.get_or_init(|| serde_json::json!({"type":"object"}))
    }
    async fn invoke(
        &self,
        _c: &ToolContext<Ctx>,
        _a: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            content: serde_json::Value::String("ok".into()),
        })
    }
}

struct MockHandoff;

#[async_trait::async_trait]
impl Agent<()> for MockHandoff {
    fn name(&self) -> &str { "handoff-target" }
    fn description(&self) -> &str { "handoff target" }
    async fn run(
        &self,
        _ctx: RunContext<()>,
        _input: AgentInput,
    ) -> Result<futures_core::stream::BoxStream<'static, AgentEvent>, AgentError> {
        unimplemented!()
    }
}

struct MockGuardrail;

#[async_trait::async_trait]
impl<Ctx> Guardrail<Ctx> for MockGuardrail
where
    Ctx: Send + Sync + 'static,
{
    async fn check(
        &self,
        _ctx: &RunContext<Ctx>,
        _input: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError> {
        Ok(GuardrailVerdict::Pass)
    }
}

struct MockHook;

#[async_trait::async_trait]
impl<Ctx> Hook<Ctx> for MockHook
where
    Ctx: Send + Sync + 'static,
{
    async fn on_event(&self, _ctx: &RunContext<Ctx>, _event: &HookEvent) -> HookDecision {
        HookDecision::Allow
    }
}

fn main() {
    let shared_tool: Arc<dyn Tool<()>> = Arc::new(MockTool);
    let _ = LlmAgent::builder::<()>()
        .description("comprehensive coverage")
        .instructions("you are helpful")
        .tool(MockTool)
        .tools(vec![Arc::new(MockTool) as Arc<dyn Tool<()>>])
        .shared_tool(shared_tool)
        .handoff(MockHandoff)
        .hook(MockHook)
        .input_guardrail(MockGuardrail)
        .output_guardrail(MockGuardrail)
        .model_settings(ModelSettings::default())
        .max_turns(8)
        .name("triage")
        .model(MockModel)
        .build();
}
```

The trait signatures above were cross-checked against `crates/paigasus-helikon-core/src/{guardrail,hook}.rs`:
- `Guardrail::check(&self, ctx, input: GuardrailInput<'_>) -> Result<GuardrailVerdict, GuardrailError>` (no `kind()` method on the trait — `GuardrailKind` is a separate enum used elsewhere).
- `Hook::on_event(&self, ctx, event: &HookEvent) -> HookDecision` (takes `&HookEvent` by reference, returns a `HookDecision` not unit).

If either signature has drifted by the time you run this, `cargo check -p paigasus-helikon-core --tests` will pinpoint the mismatch — adjust the impl, the rest of the chain doesn't change.

- [ ] **Step 2: Create `builder_output_type_typed.rs`**

```rust
//! `.output_type::<Answer>()` produces an `LlmAgent<MyCtx, MockModel, Answer>`.
//! Binding to the explicit type proves T flows through the builder to the
//! agent.

use paigasus_helikon_core::{
    CancellationToken, LlmAgent, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

struct MockModel;

#[async_trait::async_trait]
impl Model for MockModel {
    async fn invoke(
        &self,
        _r: ModelRequest,
        _c: CancellationToken,
    ) -> Result<
        futures_core::stream::BoxStream<'static, Result<ModelEvent, ModelError>>,
        ModelError,
    > {
        Err(ModelError::Unavailable)
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct Answer {
    #[allow(dead_code)]
    value: u32,
}

fn main() {
    let _: LlmAgent<(), MockModel, Answer> = LlmAgent::builder::<()>()
        .name("triage")
        .model(MockModel)
        .output_type::<Answer>()
        .build();
}
```

### Task C.5: Run the trybuild suite and accept the generated `.stderr` files

- [ ] **Step 1: Run the trybuild harness**

```
cargo test -p paigasus-helikon-core --test trybuild_ui
```

Expected on first run: the compile-fail fixtures produce a `wip/*.stderr` file under `target/tests/trybuild/paigasus-helikon-core/wip/`. trybuild prints a diff showing the expected (absent) vs actual stderr. The test FAILS with "stderr file does not exist — `cp wip/X.stderr tests/ui/X.stderr`".

- [ ] **Step 2: Promote the `wip/*.stderr` files**

For each compile-fail fixture, copy the generated stderr from `target/tests/trybuild/paigasus-helikon-core/wip/` into `crates/paigasus-helikon-core/tests/ui/`:

```
cp target/tests/trybuild/paigasus-helikon-core/wip/builder_missing_name.stderr \
   crates/paigasus-helikon-core/tests/ui/builder_missing_name.stderr
cp target/tests/trybuild/paigasus-helikon-core/wip/builder_missing_model.stderr \
   crates/paigasus-helikon-core/tests/ui/builder_missing_model.stderr
cp target/tests/trybuild/paigasus-helikon-core/wip/builder_missing_both.stderr \
   crates/paigasus-helikon-core/tests/ui/builder_missing_both.stderr
cp target/tests/trybuild/paigasus-helikon-core/wip/builder_name_twice.stderr \
   crates/paigasus-helikon-core/tests/ui/builder_name_twice.stderr
cp target/tests/trybuild/paigasus-helikon-core/wip/builder_model_twice.stderr \
   crates/paigasus-helikon-core/tests/ui/builder_model_twice.stderr
```

- [ ] **Step 3: Review the captured stderrs**

For each `.stderr`, open and confirm the diagnostic mentions the typestate marker types (e.g. "method `build` not found in `LlmAgentBuilder<…, NoName, …>`"). If a diagnostic is unclear or surfaces an unrelated error, the fixture or builder signatures need adjustment. The captured stderrs are then byte-exactly snapshotted against future rustc runs.

- [ ] **Step 4: Re-run the harness — should pass**

```
cargo test -p paigasus-helikon-core --test trybuild_ui
```

Expected: PASS.

### Task C.6: Verify full CI gates locally and commit Phase C

- [ ] **Step 1: Reproduce every CI gate locally**

```
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: all four succeed.

- [ ] **Step 2: Run doc coverage**

If `rustup toolchain list` shows `nightly-2026-05-01`:

```
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
```

Expected: passes (≥80%). If not present, install with `rustup toolchain install nightly-2026-05-01`. If coverage drops, find the under-documented item in `agent_builder.rs` and add a doc comment.

- [ ] **Step 3: Stage Phase C changes**

```
git add crates/paigasus-helikon-core/Cargo.toml \
        crates/paigasus-helikon-core/tests/trybuild_ui.rs \
        crates/paigasus-helikon-core/tests/ui/
```

- [ ] **Step 4: Commit Phase C**

```
git commit -m "$(cat <<'EOF'
test(core): SMA-319 add trybuild UI fixtures for typestate builder

Five compile-fail fixtures (missing name, missing model, missing
both, name twice, model twice) plus two pass fixtures (happy path
exercising every any-state setter, output_type<T> producing an
LlmAgent<Ctx, M, T> via explicit type annotation).

Add trybuild = { workspace = true } to core's dev-dependencies.
The existing --skip trybuild_ui filter in the CI matrix gates the
new harness to the latest-stable row (rustc stderr drifts across
releases; SMA-349 handles the gating mechanism).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase D — Open PR

### Task D.1: Push branch and open PR

- [ ] **Step 1: Push the branch**

```
git push -u origin feature/sma-319-typestate-builder-for-llmagent
```

- [ ] **Step 2: Open the PR**

```
gh pr create --title "feat(core): SMA-319 add typestate builder for LlmAgent" --body "$(cat <<'EOF'
## Summary
- LlmAgentBuilder with `NoName/HasName` + `NoModel/HasModel` typestate markers; `.build()` only resolves once both have been set.
- LlmAgent widened to `LlmAgent<Ctx, M, T = String>` so `.output_type::<T>()` is a real typestate transition flowing T to the agent type. Runner-side wiring lands in SMA-320.
- Five trybuild compile-fail fixtures lock the error surface; two pass fixtures lock the happy path and typed-output path.

Design: `docs/superpowers/specs/2026-05-28-sma-319-typestate-builder-design.md`

## Test plan
- [ ] CI green on `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`.
- [ ] `trybuild_ui` test row succeeds (latest-stable only, per SMA-349 gating).
- [ ] Local `cargo test --workspace --all-features` passes.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 3: Report the PR URL**

`gh pr create` prints the URL. Capture and report it.

---

## Self-review checklist

Run mentally against the spec before declaring complete:

1. **Acceptance criteria mapping** (spec §"Acceptance criteria mapping"):
   - AC #1 (compile-fail on missing `.name`/`.model`): covered by tasks C.3 + C.5 (builder_missing_*.rs fixtures).
   - AC #2 (T flows after `.output_type::<T>()`): covered by C.4 step 2 (builder_output_type_typed.rs explicit type binding) and B.7 step 1 (unit tests).
2. **Spec coverage**:
   - Typestate markers (spec §"Markers"): Task B.2.
   - Builder struct (spec §"Struct"): Task B.2.
   - Any-state setters incl. `.description` / `.shared_instructions` / singular+shared+plural for tool/handoff/hook/input_guardrail/output_guardrail (spec §"Methods callable in any state"): Task B.3.
   - `.name` transition (spec §"Required transitions"): Task B.4.
   - `.model` / `.shared_model` transitions (spec §"Required transitions"): Task B.5.
   - `.output_type<T>` transition (spec §"Optional T transition"): Task B.7.
   - `.build()` impl (spec §".build() — only on the final state"): Task B.6.
   - Defaults & invariants (spec §"Defaults & invariants"): unit tests in B.8 lock each.
   - `Agent` impl update (spec §"Agent impl update"): Task A.2.
   - Migration / blast radius (spec §"Migration / blast radius"): touch sites enumerated in Tasks A.4, A.5.
3. **Type consistency**: `.builder::<Ctx>()` returns `LlmAgentBuilder<Ctx, (), String, NoName, NoModel>` everywhere it's referenced. `.model<M2>(m)` consistently returns `<Ctx, M2, T, N, HasModel>`. `.output_type<T2>()` returns `<Ctx, M, T2, N, Mo>`. The struct field order in `.build()`, `__new()`, `.name()`, `.shared_model()`, and `.output_type()` is consistent.
4. **No placeholders**: every code block is complete and executable; no "TODO", "TBD", or "similar to above" handwaving.
