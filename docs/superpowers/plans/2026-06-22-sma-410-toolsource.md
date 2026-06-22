# SMA-410 `ToolSource` trait + builder sugar — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a core-side `trait ToolSource<Ctx>` and builder sugar (`.tool_source` / `.tool_sources` / `.mcp_servers` + an async `.build_resolved()` finalizer) so an `LlmAgent` can auto-discover an MCP server's tools, with `McpServerHandle` implementing `ToolSource`.

**Architecture:** Additive (Approach B). `LlmAgent` and `AgentError` are untouched; sources are resolved at an async build finalizer that folds discovered tools into the existing `tools` field. A 6th, defaulted typestate parameter `So` (`NoSources`/`HasSources`) makes `.build()` a compile error once a source is registered, forcing `.build_resolved().await?`.

**Tech Stack:** Rust (edition 2024, MSRV 1.85), `async-trait`, `thiserror`, `anyhow`, `futures-util` (`try_join_all`), `tokio` (tests), `trybuild` (compile-fail tests). Workspace crates: `paigasus-helikon-core`, `paigasus-helikon-mcp`, `paigasus-helikon` (facade).

**Spec:** `docs/superpowers/specs/2026-06-22-sma-410-toolsource-design.md` (v2, approved).

## Global Constraints

- **Worktree-absolute paths.** All work happens in the worktree at `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/feature+sma-410-toolsource-trait/`. File paths below are relative to that root. Never write to the main checkout path.
- **MSRV 1.85**, edition 2024; workspace inheritance is mandatory (don't hardcode per-crate metadata).
- **`missing_docs` is workspace-wide `warn` + the `docs` CI job runs `-D warnings`.** Every new `pub` item needs a `///` doc comment, or CI fails.
- **Commits are signed via a 1Password SSH key.** If a commit fails with `1Password: failed to fill whole buffer`, the vault is locked — stop and ask the user to unlock; do not bypass signing.
- **Never `git add -A`** (`.env`/`.claude` are not gitignored). Stage explicit paths only; verify with `git show --stat`.
- **Conventional Commits**: `<type>(<scope>): SMA-410 <lowercase subject>`. The local `commit-msg` hook runs `convco check`. Use scopes `core`, `mcp`, `facade`, `docs`, `release` (all in the allowlist). The PR title (later) must start lowercase after `SMA-410 `.
- **Run `cargo fmt --all` before every commit** (the pre-commit hook is a no-op; pre-push checks fmt+clippy).
- **No breaking changes.** Do not add a field to `LlmAgent`, do not change any existing signature, do not make `LlmAgent` `#[non_exhaustive]`.
- **Bumps (Task 7 only):** core `0.5.10`→`0.5.11`, mcp `0.1.10`→`0.1.11`, facade `0.4.6`→`0.4.7`, plus the matching `[workspace.dependencies]` pins and each `CHANGELOG.md`.

---

## File Structure

| File | Responsibility | Task |
|------|----------------|------|
| `crates/paigasus-helikon-core/src/tool.rs` | add `ToolSource<Ctx>` trait + `ToolSourceError` enum | 1 |
| `crates/paigasus-helikon-core/src/agent_builder.rs` | add `NoSources`/`HasSources`, `So` param, `tool_sources` field, 5 registration methods, gate `build()`, add `build_resolved()`; behavioral tests | 2, 3 |
| `crates/paigasus-helikon-core/tests/ui/build_with_source_requires_build_resolved.rs` (+`.stderr`) | trybuild compile-fail: `.tool_source(..).build()` rejected | 4 |
| `crates/paigasus-helikon-mcp/src/client/handle.rs` | `impl ToolSource<Ctx> for McpServerHandle` | 5 |
| `crates/paigasus-helikon-mcp/tests/client_tools.rs` | test the impl returns same tools as inherent `tools()` | 5 |
| `crates/paigasus-helikon-mcp/src/lib.rs`, `docs/book/src/*`, `crates/*/README.md` | docs | 6 |
| `crates/*/Cargo.toml`, root `Cargo.toml`, `crates/*/CHANGELOG.md` | version bumps | 7 |

---

## Task 1: `ToolSource` trait + `ToolSourceError` (core)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/tool.rs` (append after the `Tool` trait, before/after `ToolError` ~line 320)
- Test: same file, new `#[cfg(test)] mod tool_source_tests`

**Interfaces:**
- Produces:
  - `pub trait ToolSource<Ctx>: Send + Sync where Ctx: Send + Sync + 'static` with `async fn tools(&self) -> Result<Vec<Arc<dyn Tool<Ctx>>>, ToolSourceError>`
  - `pub enum ToolSourceError { Resolution { source: String, cause: anyhow::Error }, DuplicateName { name: String }, Other(anyhow::Error) }` (`#[non_exhaustive]`, `thiserror::Error`)

- [ ] **Step 1: Write the failing test.** Append to `crates/paigasus-helikon-core/src/tool.rs`:

```rust
#[cfg(test)]
mod tool_source_tests {
    use super::*;
    use std::sync::Arc;

    struct OneTool;
    #[async_trait]
    impl<Ctx> Tool<Ctx> for OneTool
    where
        Ctx: Send + Sync + 'static,
    {
        fn name(&self) -> &str { "one" }
        fn description(&self) -> &str { "one tool" }
        fn schema(&self) -> &serde_json::Value {
            static S: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
            S.get_or_init(|| serde_json::json!({"type":"object"}))
        }
        async fn invoke(
            &self,
            _c: &ToolContext<Ctx>,
            _a: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::new(serde_json::Value::Null))
        }
    }

    struct OkSource;
    #[async_trait]
    impl<Ctx> ToolSource<Ctx> for OkSource
    where
        Ctx: Send + Sync + 'static,
    {
        async fn tools(&self) -> Result<Vec<Arc<dyn Tool<Ctx>>>, ToolSourceError> {
            Ok(vec![Arc::new(OneTool) as Arc<dyn Tool<Ctx>>])
        }
    }

    #[tokio::test]
    async fn tool_source_yields_tools() {
        let src = OkSource;
        let tools = ToolSource::<()>::tools(&src).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "one");
    }

    #[test]
    fn duplicate_name_error_renders() {
        let e = ToolSourceError::DuplicateName { name: "echo".into() };
        assert!(e.to_string().contains("echo"));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails to compile** (`ToolSource`/`ToolSourceError` don't exist yet).

Run: `cargo test -p paigasus-helikon-core tool_source_tests 2>&1 | head -20`
Expected: FAIL — `cannot find trait ToolSource` / `cannot find type ToolSourceError`.

- [ ] **Step 3: Add `ToolSourceError`.** In `crates/paigasus-helikon-core/src/tool.rs`, after the `ToolError` enum (after ~line 320):

```rust
/// Errors raised while resolving a [`ToolSource`] or merging resolved tools.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolSourceError {
    /// A source failed to produce its tools (e.g. a transport/discovery failure).
    ///
    /// Constructed by the failing [`ToolSource`] implementation, which supplies
    /// its own `source` label; the agent builder propagates it unchanged.
    #[error("tool source {source:?} failed to resolve: {cause}")]
    Resolution {
        /// Caller-meaningful label for the failing source (supplied by the impl).
        source: String,
        /// Underlying cause.
        #[source]
        cause: anyhow::Error,
    },

    /// A resolved source introduced a tool whose name already exists in the
    /// merged namespace (static tools or an earlier source). Rejected at build
    /// time rather than silently shadowed, because tools dispatch by name.
    #[error("duplicate tool name {name:?} introduced by a tool source")]
    DuplicateName {
        /// The conflicting tool name.
        name: String,
    },

    /// Escape hatch for arbitrary source failures.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

- [ ] **Step 4: Add the `ToolSource` trait.** In the same file, after `ToolSourceError` (keep `use async_trait::async_trait;` already imported at top, line 8):

```rust
/// An asynchronous provider of [`Tool`]s, resolved when the agent is built.
///
/// Implemented by anything that can produce tools through (potentially) async
/// work — for example an MCP server handle that discovered its tools over a
/// transport. Register sources on the builder via
/// [`crate::LlmAgentBuilder::tool_source`], [`crate::LlmAgentBuilder::tool_sources`],
/// or [`crate::LlmAgentBuilder::mcp_servers`]; they are resolved exactly once by
/// [`crate::LlmAgentBuilder::build_resolved`].
///
/// Object-safe by the same construction as [`Tool`] — held as
/// `Arc<dyn ToolSource<Ctx>>`.
#[async_trait]
pub trait ToolSource<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Resolve the tools this source provides.
    ///
    /// Called once, at `build_resolved()`. Returning `Err` aborts the build
    /// with that [`ToolSourceError`]. Implementations that want labeled errors
    /// construct [`ToolSourceError::Resolution`] themselves.
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool<Ctx>>>, ToolSourceError>;
}
```

- [ ] **Step 5: Run the test to verify it passes.**

Run: `cargo test -p paigasus-helikon-core tool_source_tests 2>&1 | tail -15`
Expected: PASS (2 tests).

- [ ] **Step 6: fmt + clippy + commit.**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings 2>&1 | tail -5
git add crates/paigasus-helikon-core/src/tool.rs
git commit -m "feat(core): SMA-410 add ToolSource trait and ToolSourceError"
```
Expected: clippy clean; commit succeeds (signed).

---

## Task 2: Builder typestate `So` + `tool_sources` field + registration methods (core)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent_builder.rs` (struct `:31`, `__new` `:63`, any-state setters `:85`, `.name` `:253`, `.model`/`.shared_model` `:283`, `.build` `:329`, `.output_type` `:363`; add markers + registration impl block)
- Test: the existing `#[cfg(test)] mod tests` in the same file

**Interfaces:**
- Consumes: `ToolSource<Ctx>` (Task 1).
- Produces:
  - `pub struct NoSources;` / `pub struct HasSources;`
  - `LlmAgentBuilder<Ctx, M, T, N, Mo, So = NoSources>` with private field `tool_sources: Vec<Arc<dyn ToolSource<Ctx>>>`
  - Registration methods (all flip to `HasSources`): `tool_source(impl ToolSource<Ctx> + 'static)`, `shared_tool_source(Arc<dyn ToolSource<Ctx>>)`, `tool_sources<I, S>(I)`, `shared_tool_sources<I>(I)`, `mcp_servers<I, S>(I)`
  - `build()` gated to `So = NoSources` (unchanged behavior otherwise)

> **Typestate threading rule:** any `impl` block that should apply in *all* source-states must name `So` generically (`impl<…, So> LlmAgentBuilder<…, So>`); blocks specific to one state name it concretely (`__new` and `build()` → `NoSources`). Add `tool_sources: self.tool_sources` to every struct-literal that rebuilds the builder (`name`, `shared_model`, `output_type`). The `build()` body is unchanged (it constructs `LlmAgent`, which has no `tool_sources`; the empty `Vec` is dropped).

- [ ] **Step 1: Add the typestate markers.** In `crates/paigasus-helikon-core/src/agent_builder.rs`, after `HasModel` (~line 18):

```rust
/// Typestate marker: no [`crate::ToolSource`] has been registered. `.build()`
/// (sync) is available; this is the default.
pub struct NoSources;

/// Typestate marker: at least one [`crate::ToolSource`] has been registered.
/// `.build()` is a compile error — finalize with
/// [`LlmAgentBuilder::build_resolved`] instead.
pub struct HasSources;
```

- [ ] **Step 2: Add the `So` parameter + field to the struct.** Change the declaration at `:31` and add the field + widen `_state`:

```rust
pub struct LlmAgentBuilder<Ctx, M, T, N, Mo, So = NoSources>
where
    Ctx: Send + Sync + 'static,
{
    name: Option<String>,
    description: Option<String>,
    instructions: Option<std::sync::Arc<dyn crate::Instructions<Ctx>>>,
    model: Option<std::sync::Arc<M>>,
    tools: Vec<std::sync::Arc<dyn crate::Tool<Ctx>>>,
    tool_sources: Vec<std::sync::Arc<dyn crate::ToolSource<Ctx>>>,
    handoffs: Vec<crate::Handoff<Ctx>>,
    output_type: Option<crate::OutputType>,
    input_guardrails: Vec<std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    output_guardrails: Vec<std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    hooks: Vec<std::sync::Arc<dyn crate::Hook<Ctx>>>,
    model_settings: crate::ModelSettings,
    config: crate::RunConfig,
    #[allow(clippy::type_complexity)]
    _state: std::marker::PhantomData<fn() -> (N, Mo, So, T)>,
}
```

- [ ] **Step 3: Update `__new`.** Its impl header gains an explicit `NoSources`, and the literal initializes `tool_sources`:

```rust
impl<Ctx> LlmAgentBuilder<Ctx, (), String, NoName, NoModel, NoSources>
where
    Ctx: Send + Sync + 'static,
{
    #[doc(hidden)]
    pub fn __new() -> Self {
        Self {
            name: None,
            description: None,
            instructions: None,
            model: None,
            tools: Vec::new(),
            tool_sources: Vec::new(),
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

- [ ] **Step 4: Thread `So` through the any-state setters, `.name`, `.model`, `.output_type`.** Edit only the `impl` headers and rebuild-literals:
  - `:85` any-state setters: `impl<Ctx, M, T, N, Mo, So> LlmAgentBuilder<Ctx, M, T, N, Mo, So>` (bodies unchanged — they `mut self … ; self`).
  - `:253` `.name`: `impl<Ctx, M, T, Mo, So> LlmAgentBuilder<Ctx, M, T, NoName, Mo, So>`; return type `LlmAgentBuilder<Ctx, M, T, HasName, Mo, So>`; add `tool_sources: self.tool_sources,` to the rebuild literal.
  - `:283` `.model`/`.shared_model`: `impl<Ctx, M0, T, N, So> LlmAgentBuilder<Ctx, M0, T, N, NoModel, So>`; both return `LlmAgentBuilder<Ctx, M2, T, N, HasModel, So>`; add `tool_sources: self.tool_sources,` to the `shared_model` literal.
  - `:363` `.output_type`: `impl<Ctx, M, T0, N, Mo, So> LlmAgentBuilder<Ctx, M, T0, N, Mo, So>`; return `LlmAgentBuilder<Ctx, M, T2, N, Mo, So>`; add `tool_sources: self.tool_sources,` to the rebuild literal.

- [ ] **Step 5: Gate `build()` to `NoSources`.** Change only the `:329` impl header:

```rust
impl<Ctx, M, T> LlmAgentBuilder<Ctx, M, T, HasName, HasModel, NoSources>
```
(body unchanged.)

- [ ] **Step 6: Add the registration-methods impl block.** Place after the `build()` impl (after `:359`):

```rust
// Source registration — any state in. Each call transitions So -> HasSources,
// which removes the sync `.build()` from scope (use `.build_resolved()`).
impl<Ctx, M, T, N, Mo, So> LlmAgentBuilder<Ctx, M, T, N, Mo, So>
where
    Ctx: Send + Sync + 'static,
{
    /// Register a tool source whose tools are discovered at
    /// [`Self::build_resolved`]. Removes the sync `.build()` from scope.
    pub fn tool_source(
        mut self,
        s: impl crate::ToolSource<Ctx> + 'static,
    ) -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources> {
        self.tool_sources
            .push(std::sync::Arc::new(s) as std::sync::Arc<dyn crate::ToolSource<Ctx>>);
        self.into_has_sources()
    }

    /// Register a pre-wrapped tool source.
    pub fn shared_tool_source(
        mut self,
        s: std::sync::Arc<dyn crate::ToolSource<Ctx>>,
    ) -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources> {
        self.tool_sources.push(s);
        self.into_has_sources()
    }

    /// Register several **homogeneous** tool sources, e.g. `[handle_a, handle_b]`.
    /// To mix source *types*, use [`Self::shared_tool_sources`].
    pub fn tool_sources<I, S>(
        mut self,
        sources: I,
    ) -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources>
    where
        I: IntoIterator<Item = S>,
        S: crate::ToolSource<Ctx> + 'static,
    {
        for s in sources {
            self.tool_sources
                .push(std::sync::Arc::new(s) as std::sync::Arc<dyn crate::ToolSource<Ctx>>);
        }
        self.into_has_sources()
    }

    /// Register several **heterogeneous / pre-wrapped** tool sources.
    pub fn shared_tool_sources<I>(
        mut self,
        sources: I,
    ) -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources>
    where
        I: IntoIterator<Item = std::sync::Arc<dyn crate::ToolSource<Ctx>>>,
    {
        self.tool_sources.extend(sources);
        self.into_has_sources()
    }

    /// Ergonomic alias for [`Self::tool_sources`], matching the MCP mental
    /// model. Despite the name, accepts any [`crate::ToolSource`] (core is
    /// MCP-agnostic).
    pub fn mcp_servers<I, S>(
        self,
        servers: I,
    ) -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources>
    where
        I: IntoIterator<Item = S>,
        S: crate::ToolSource<Ctx> + 'static,
    {
        self.tool_sources(servers)
    }

    /// Private: rebuild the builder in the `HasSources` typestate.
    fn into_has_sources(self) -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources> {
        LlmAgentBuilder {
            name: self.name,
            description: self.description,
            instructions: self.instructions,
            model: self.model,
            tools: self.tools,
            tool_sources: self.tool_sources,
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

- [ ] **Step 7: Add a regression + registration smoke test** to the existing `mod tests`:

```rust
    #[test]
    fn build_unaffected_with_no_sources() {
        let agent = LlmAgent::builder::<()>()
            .name("t")
            .model(StubModel)
            .tool(StubTool)
            .build();
        assert_eq!(agent.tools.len(), 1);
    }

    #[test]
    fn registration_methods_typecheck() {
        // Registering a source must compile and yield a HasSources builder
        // (which no longer has `.build()`); we just construct and drop it.
        let _b = LlmAgent::builder::<()>()
            .name("t")
            .model(StubModel)
            .tool_source(StubTool);
        let _b2 = LlmAgent::builder::<()>()
            .name("t")
            .model(StubModel)
            .mcp_servers([StubTool, StubTool]);
    }
```

> Note: `StubTool` here doubles as a `ToolSource` only after Task 3 adds nothing for it — instead, define a local `StubSource` in this test module mirroring `OkSource` from Task 1 (a struct whose `ToolSource::tools` returns `vec![]`). Add it near `StubTool`:

```rust
    struct StubSource;
    #[async_trait]
    impl<Ctx> crate::ToolSource<Ctx> for StubSource
    where
        Ctx: Send + Sync + 'static,
    {
        async fn tools(
            &self,
        ) -> Result<Vec<Arc<dyn Tool<Ctx>>>, crate::ToolSourceError> {
            Ok(vec![])
        }
    }
```
…and use `.tool_source(StubSource)` / `.mcp_servers([StubSource, StubSource])` in the smoke test.

- [ ] **Step 8: Compile & run.**

Run: `cargo test -p paigasus-helikon-core --lib agent_builder 2>&1 | tail -20`
Expected: PASS (existing builder tests + the 2 new ones). If a setter impl fails to compile because `So` resolved to its default, add the explicit `So` generic to that header per the threading rule.

- [ ] **Step 9: fmt + clippy + commit.**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings 2>&1 | tail -5
git add crates/paigasus-helikon-core/src/agent_builder.rs
git commit -m "feat(core): SMA-410 add tool-source typestate and builder registration methods"
```

---

## Task 3: `build_resolved()` finalizer + behavior tests (core)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent_builder.rs` (new finalizer impl block; behavior tests in `mod tests`)

**Interfaces:**
- Consumes: the `HasName`/`HasModel`/`So` typestate + `tool_sources` field (Task 2); `ToolSourceError` (Task 1).
- Produces: `pub async fn build_resolved(self) -> Result<LlmAgent<Ctx, M, T>, ToolSourceError>` available for any `So` once `HasName + HasModel`.

- [ ] **Step 1: Write the failing behavior tests.** Add to `mod tests`. First add a configurable mock source with a call-counter and a name list, plus a failing source:

```rust
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct NamedTool(&'static str);
    #[async_trait]
    impl<Ctx> Tool<Ctx> for NamedTool
    where
        Ctx: Send + Sync + 'static,
    {
        fn name(&self) -> &str { self.0 }
        fn description(&self) -> &str { "named" }
        fn schema(&self) -> &serde_json::Value {
            static S: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
            S.get_or_init(|| serde_json::json!({"type":"object"}))
        }
        async fn invoke(
            &self,
            _c: &ToolContext<Ctx>,
            _a: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput { content: serde_json::Value::Null })
        }
    }

    struct CountingSource {
        names: Vec<&'static str>,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl<Ctx> crate::ToolSource<Ctx> for CountingSource
    where
        Ctx: Send + Sync + 'static,
    {
        async fn tools(
            &self,
        ) -> Result<Vec<Arc<dyn Tool<Ctx>>>, crate::ToolSourceError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self
                .names
                .iter()
                .map(|n| Arc::new(NamedTool(n)) as Arc<dyn Tool<Ctx>>)
                .collect())
        }
    }

    struct FailingSource;
    #[async_trait]
    impl<Ctx> crate::ToolSource<Ctx> for FailingSource
    where
        Ctx: Send + Sync + 'static,
    {
        async fn tools(
            &self,
        ) -> Result<Vec<Arc<dyn Tool<Ctx>>>, crate::ToolSourceError> {
            Err(crate::ToolSourceError::Resolution {
                source: "failing".into(),
                cause: anyhow::anyhow!("boom"),
            })
        }
    }

    #[tokio::test]
    async fn build_resolved_zero_sources_equals_build() {
        let agent = LlmAgent::builder::<()>()
            .name("t")
            .model(StubModel)
            .tool(NamedTool("a"))
            .build_resolved()
            .await
            .unwrap();
        assert_eq!(agent.tools.len(), 1);
        assert_eq!(agent.tools[0].name(), "a");
    }

    #[tokio::test]
    async fn build_resolved_appends_sources_in_order() {
        let calls = Arc::new(AtomicUsize::new(0));
        let agent = LlmAgent::builder::<()>()
            .name("t")
            .model(StubModel)
            .tool(NamedTool("static"))
            .tool_source(CountingSource { names: vec!["s1a", "s1b"], calls: calls.clone() })
            .tool_source(CountingSource { names: vec!["s2"], calls: calls.clone() })
            .build_resolved()
            .await
            .unwrap();
        let names: Vec<&str> = agent.tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["static", "s1a", "s1b", "s2"]);
        assert_eq!(calls.load(Ordering::SeqCst), 2); // each source resolved once
    }

    #[tokio::test]
    async fn build_resolved_rejects_source_vs_static_duplicate() {
        let calls = Arc::new(AtomicUsize::new(0));
        let err = LlmAgent::builder::<()>()
            .name("t")
            .model(StubModel)
            .tool(NamedTool("dup"))
            .tool_source(CountingSource { names: vec!["dup"], calls })
            .build_resolved()
            .await
            .unwrap_err();
        assert!(matches!(err, crate::ToolSourceError::DuplicateName { name } if name == "dup"));
    }

    #[tokio::test]
    async fn build_resolved_rejects_source_vs_source_duplicate() {
        let calls = Arc::new(AtomicUsize::new(0));
        let err = LlmAgent::builder::<()>()
            .name("t")
            .model(StubModel)
            .tool_source(CountingSource { names: vec!["x"], calls: calls.clone() })
            .tool_source(CountingSource { names: vec!["x"], calls })
            .build_resolved()
            .await
            .unwrap_err();
        assert!(matches!(err, crate::ToolSourceError::DuplicateName { name } if name == "x"));
    }

    #[tokio::test]
    async fn build_resolved_allows_static_vs_static_duplicate() {
        // Static duplicates keep today's first-wins behavior even via build_resolved,
        // as long as no SOURCE introduces the collision.
        let calls = Arc::new(AtomicUsize::new(0));
        let agent = LlmAgent::builder::<()>()
            .name("t")
            .model(StubModel)
            .tool(NamedTool("same"))
            .tool(NamedTool("same"))
            .tool_source(CountingSource { names: vec!["fresh"], calls })
            .build_resolved()
            .await
            .unwrap();
        let names: Vec<&str> = agent.tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["same", "same", "fresh"]);
    }

    #[tokio::test]
    async fn build_resolved_propagates_source_failure() {
        let err = LlmAgent::builder::<()>()
            .name("t")
            .model(StubModel)
            .tool_source(FailingSource)
            .build_resolved()
            .await
            .unwrap_err();
        assert!(matches!(err, crate::ToolSourceError::Resolution { source, .. } if source == "failing"));
    }
```

- [ ] **Step 2: Run to verify failure** (no `build_resolved` yet).

Run: `cargo test -p paigasus-helikon-core --lib build_resolved 2>&1 | head -20`
Expected: FAIL — `no method named build_resolved`.

- [ ] **Step 3: Implement `build_resolved`.** Add a new impl block (place after the registration block):

```rust
// .build_resolved() — async finalizer available for any So once Name+Model are
// set. Resolves all sources concurrently, merges (static first, then sources in
// registration order), rejects source-introduced name collisions, and builds.
impl<Ctx, M, T, So> LlmAgentBuilder<Ctx, M, T, HasName, HasModel, So>
where
    Ctx: Send + Sync + 'static,
    M: crate::Model + 'static,
    T: Send + Sync + 'static,
{
    /// Resolve all registered [`crate::ToolSource`]s and finalize into an
    /// [`crate::LlmAgent`].
    ///
    /// Sources resolve concurrently (registration order preserved in the
    /// merged tool list, after the static `.tool(...)` tools). A source that
    /// introduces a tool name already present in the merged namespace fails
    /// with [`crate::ToolSourceError::DuplicateName`]; a source whose
    /// `tools()` errors aborts the build with that error (remaining in-flight
    /// resolutions are dropped). With no sources this is equivalent to
    /// [`Self::build`].
    pub async fn build_resolved(
        self,
    ) -> Result<crate::LlmAgent<Ctx, M, T>, crate::ToolSourceError> {
        // Resolve concurrently; try_join_all preserves input order.
        let resolved: Vec<Vec<std::sync::Arc<dyn crate::Tool<Ctx>>>> =
            futures_util::future::try_join_all(self.tool_sources.iter().map(|s| s.tools()))
                .await?;

        // Merge: static tools first, then resolved tools in registration order.
        // Seed the seen-set with static names (owned, so we can then move
        // `self.tools` into `merged` without a borrow conflict). Only a name a
        // SOURCE introduces is rejected — static-vs-static is left to first-wins.
        let mut seen: std::collections::HashSet<String> =
            self.tools.iter().map(|t| t.name().to_owned()).collect();
        let mut merged = self.tools;
        for per_source in resolved {
            for tool in per_source {
                if !seen.insert(tool.name().to_owned()) {
                    return Err(crate::ToolSourceError::DuplicateName {
                        name: tool.name().to_owned(),
                    });
                }
                merged.push(tool);
            }
        }

        Ok(crate::LlmAgent {
            name: self.name.expect("typestate HasName guarantees Some"),
            description: self.description.unwrap_or_default(),
            instructions: self
                .instructions
                .unwrap_or_else(|| std::sync::Arc::new(String::new())),
            model: self.model.expect("typestate HasModel guarantees Some"),
            tools: merged,
            handoffs: self.handoffs,
            output_type: self.output_type,
            input_guardrails: self.input_guardrails,
            output_guardrails: self.output_guardrails,
            hooks: self.hooks,
            model_settings: self.model_settings,
            config: self.config,
            _output: std::marker::PhantomData,
        })
    }
}
```

- [ ] **Step 4: Run the behavior tests.**

Run: `cargo test -p paigasus-helikon-core --lib build_resolved 2>&1 | tail -20`
Expected: PASS (6 tests).

- [ ] **Step 5: fmt + clippy + commit.**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings 2>&1 | tail -5
git add crates/paigasus-helikon-core/src/agent_builder.rs
git commit -m "feat(core): SMA-410 add build_resolved finalizer resolving tool sources"
```

---

## Task 4: trybuild compile-fail — `.build()` rejected after a source (core)

**Files:**
- Create: `crates/paigasus-helikon-core/tests/ui/build_with_source_requires_build_resolved.rs`
- Create: `crates/paigasus-helikon-core/tests/ui/build_with_source_requires_build_resolved.stderr`
- Reference: `crates/paigasus-helikon-core/tests/trybuild_ui.rs` (the harness — confirm it globs `tests/ui/*.rs`)

**Interfaces:** Consumes the `HasSources` gating (Task 2).

- [ ] **Step 1: Inspect the trybuild harness** to match its convention.

Run: `sed -n '1,40p' crates/paigasus-helikon-core/tests/trybuild_ui.rs`
Expected: a `trybuild::TestCases` that adds `tests/ui/*.rs` (compile_fail). Note whether each fixture is added explicitly or by glob — if explicit, add the new file to the list.

- [ ] **Step 2: Write the compile-fail fixture.** Create `crates/paigasus-helikon-core/tests/ui/build_with_source_requires_build_resolved.rs`:

```rust
//! `.build()` must be a compile error once a `ToolSource` is registered —
//! the user must call `.build_resolved().await` instead.
use paigasus_helikon_core::{LlmAgent, Tool, ToolContext, ToolError, ToolOutput, ToolSource};
use async_trait::async_trait;
use std::sync::Arc;

struct M;
#[async_trait]
impl paigasus_helikon_core::Model for M {
    async fn invoke(
        &self,
        _r: paigasus_helikon_core::ModelRequest,
        _c: paigasus_helikon_core::CancellationToken,
    ) -> Result<
        futures_core::stream::BoxStream<
            'static,
            Result<paigasus_helikon_core::ModelEvent, paigasus_helikon_core::ModelError>,
        >,
        paigasus_helikon_core::ModelError,
    > {
        Err(paigasus_helikon_core::ModelError::Unavailable)
    }
    fn capabilities(&self) -> paigasus_helikon_core::ModelCapabilities {
        paigasus_helikon_core::ModelCapabilities::default()
    }
}

struct S;
#[async_trait]
impl<Ctx: Send + Sync + 'static> ToolSource<Ctx> for S {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool<Ctx>>>, paigasus_helikon_core::ToolSourceError> {
        Ok(vec![])
    }
}

fn main() {
    let _agent = LlmAgent::builder::<()>()
        .name("x")
        .model(M)
        .tool_source(S)
        .build(); //~ ERROR no method named `build`
}
```

- [ ] **Step 3: Generate the expected stderr.** Run with `TRYBUILD=overwrite` to capture, then inspect:

Run: `TRYBUILD=overwrite cargo test -p paigasus-helikon-core --test trybuild_ui 2>&1 | tail -20`
Then: `cat crates/paigasus-helikon-core/tests/ui/build_with_source_requires_build_resolved.stderr`
Expected: a `.stderr` mentioning `no method named \`build\` found` for the `HasSources` builder. Sanity-check the message references `build_resolved` or the `HasSources`/typestate so the fixture is pinned to the intended cause (not an unrelated error).

- [ ] **Step 4: Re-run without overwrite to confirm it passes.**

Run: `cargo test -p paigasus-helikon-core --test trybuild_ui 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 5: fmt + commit.**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/tests/ui/build_with_source_requires_build_resolved.rs \
        crates/paigasus-helikon-core/tests/ui/build_with_source_requires_build_resolved.stderr
# only if the harness lists fixtures explicitly:
# git add crates/paigasus-helikon-core/tests/trybuild_ui.rs
git commit -m "test(core): SMA-410 compile-fail guard for build() after tool_source"
```

---

## Task 5: `McpServerHandle: ToolSource<Ctx>` (mcp)

**Files:**
- Modify: `crates/paigasus-helikon-mcp/src/client/handle.rs` (add the impl; import `ToolSource`/`ToolSourceError`)
- Test: `crates/paigasus-helikon-mcp/tests/client_tools.rs` (uses `mod support; support::connect_fixture(...)`)

**Interfaces:**
- Consumes: `ToolSource<Ctx>` (Task 1), the inherent `McpServerHandle::tools::<Ctx>()` (`handle.rs:205`).
- Produces: `impl<Ctx: Send + Sync + 'static> ToolSource<Ctx> for McpServerHandle`.

- [ ] **Step 1: Write the failing test** in `crates/paigasus-helikon-mcp/tests/client_tools.rs` (append):

```rust
#[tokio::test]
async fn tool_source_impl_matches_inherent_tools() {
    use paigasus_helikon_core::ToolSource;
    let handle = support::connect_fixture(McpConnectOptions::new()).await;
    let inherent = handle.tools::<()>();
    let via_trait = ToolSource::<()>::tools(&handle).await.expect("resolve");
    assert_eq!(via_trait.len(), inherent.len());
    assert_eq!(via_trait.len(), 4); // fixture exposes 4 tools
}
```

- [ ] **Step 2: Run to verify failure** (trait not implemented).

Run: `cargo test -p paigasus-helikon-mcp --test client_tools tool_source_impl 2>&1 | head -20`
Expected: FAIL — `ToolSource` not implemented for `McpServerHandle`.

- [ ] **Step 3: Add the impl.** In `crates/paigasus-helikon-mcp/src/client/handle.rs`, extend the import at line 5 and add the impl after the inherent `tools` method (after ~line 220):

```rust
// line 5 — extend:
use paigasus_helikon_core::{Tool, ToolSource, ToolSourceError};
```

```rust
#[async_trait::async_trait]
impl<Ctx> ToolSource<Ctx> for McpServerHandle
where
    Ctx: Send + Sync + 'static,
{
    /// Resolve this server's tools. Discovery already happened at `connect()`,
    /// so the inherent [`McpServerHandle::tools`] adapter is cheap and
    /// infallible — this wrapper always returns `Ok`.
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool<Ctx>>>, ToolSourceError> {
        Ok(<McpServerHandle>::tools::<Ctx>(self))
    }
}
```

> `async_trait` is already a dependency (`crates/paigasus-helikon-mcp/Cargo.toml:17`). The `<McpServerHandle>::tools::<Ctx>(self)` path calls the **inherent** method unambiguously (no recursion into the trait method).

- [ ] **Step 4: Run the test.**

Run: `cargo test -p paigasus-helikon-mcp --test client_tools tool_source_impl 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 5: fmt + clippy + commit.**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-mcp --all-targets -- -D warnings 2>&1 | tail -5
git add crates/paigasus-helikon-mcp/src/client/handle.rs crates/paigasus-helikon-mcp/tests/client_tools.rs
git commit -m "feat(mcp): SMA-410 implement ToolSource for McpServerHandle"
```

---

## Task 6: Documentation (mdBook + READMEs)

**Files:**
- Modify: `crates/paigasus-helikon-mcp/src/lib.rs` (module-doc example — show the `.mcp_servers(...).build_resolved()` ergonomic)
- Modify: `crates/paigasus-helikon-mcp/README.md`, `crates/paigasus-helikon-core/README.md`, `crates/paigasus-helikon/README.md`
- Modify: the MCP/tools page under `docs/book/src/` (locate it first)

- [ ] **Step 1: Locate the book's MCP/tools page.**

Run: `ls docs/book/src && grep -rln "mcp\|McpServerHandle\|tools(" docs/book/src | head`
Expected: identify the page(s) that document MCP wiring (e.g. `docs/book/src/mcp.md` or a tools page).

- [ ] **Step 2: Add the ergonomic to the mcp crate module doc.** In `crates/paigasus-helikon-mcp/src/lib.rs`, after the existing `# Example: filesystem tools into an agent` block, add a second snippet (keep `no_run`), showing:

```rust
//! # Example: auto-discovery via the builder
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use paigasus_helikon_core::LlmAgent;
//! use paigasus_helikon_mcp::McpServerHandle;
//! # struct M;
//! # // ... a Model in real code
//! let fs = McpServerHandle::stdio(tokio::process::Command::new("npx"), |cmd| {
//!     cmd.args(["-y", "@modelcontextprotocol/server-filesystem", "/data"]);
//! })
//! .connect()
//! .await?;
//!
//! // let agent = LlmAgent::builder::<()>()
//! //     .name("assistant")
//! //     .model(model)
//! //     .mcp_servers([fs])
//! //     .build_resolved()
//! //     .await?;
//! # let _ = fs;
//! # Ok(())
//! # }
//! ```
```
Keep the original `tools()` example too (the explicit path remains valid). Adjust the `Discovery happens once at connect; tools() is synchronous.` line to add: "or register the handle with `.mcp_servers([...])` and call `.build_resolved()`."

- [ ] **Step 3: Update the three READMEs.** Add a short "auto-discovery" usage line/snippet to `crates/paigasus-helikon-mcp/README.md`; mention `ToolSource` in the tools/builder section of `crates/paigasus-helikon-core/README.md`; if `crates/paigasus-helikon/README.md` shows an MCP example, mirror the `.mcp_servers(...).build_resolved()` form there. Use drift-free `cargo add` (no hardcoded versions). The crate roster / feature→module map is unchanged, so the root `README.md` needs no edit.

- [ ] **Step 4: Build the book and the docs to verify no warnings.**

Run: `mdbook build docs/book 2>&1 | tail -15`
Expected: clean (linkcheck `warning-policy = "error"`).
Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-mcp -p paigasus-helikon-core --no-deps 2>&1 | tail -15`
Expected: clean (the new `no_run` doc example compiles; all new `pub` items are documented).

- [ ] **Step 5: commit.**

```bash
git add crates/paigasus-helikon-mcp/src/lib.rs crates/paigasus-helikon-mcp/README.md \
        crates/paigasus-helikon-core/README.md crates/paigasus-helikon/README.md docs/book/src
git commit -m "docs(mcp): SMA-410 document ToolSource auto-discovery ergonomic"
```

---

## Task 7: Version bumps + CHANGELOGs (release mechanics)

**Files:**
- Modify: `crates/paigasus-helikon-core/Cargo.toml`, `crates/paigasus-helikon-mcp/Cargo.toml`, `crates/paigasus-helikon/Cargo.toml` (`version`)
- Modify: root `Cargo.toml` (`[workspace.dependencies]` pins for the three crates)
- Modify: `crates/paigasus-helikon-core/CHANGELOG.md`, `crates/paigasus-helikon-mcp/CHANGELOG.md`, `crates/paigasus-helikon/CHANGELOG.md`

> **Why all three in one PR (from the spec §7):** mcp uses the same-PR `ToolSource` API, so `cargo publish --verify` builds mcp against the *registry* core — core must publish first carrying `ToolSource`. The manual same-PR core bump defeats release-plz's `dependencies_update` cascade, so the facade must be bumped explicitly too. No other crate is touched (`^0.5.x` still satisfies `0.5.11`).

- [ ] **Step 1: Confirm current versions.**

Run: `grep -nE '^version' crates/paigasus-helikon-core/Cargo.toml crates/paigasus-helikon-mcp/Cargo.toml crates/paigasus-helikon/Cargo.toml; grep -nE 'paigasus-helikon(-core|-mcp)? *=' Cargo.toml`
Expected: core `0.5.10`, mcp `0.1.10`, facade `0.4.6`; pins match.

- [ ] **Step 2: Bump the three crate versions** — core `0.5.11`, mcp `0.1.11`, facade `0.4.7` (each crate's `Cargo.toml` `version =`).

- [ ] **Step 3: Bump the `[workspace.dependencies]` pins** in root `Cargo.toml` to match: `paigasus-helikon-core … version = "0.5.11"`, `paigasus-helikon-mcp … version = "0.1.11"`, `paigasus-helikon … version = "0.4.7"`.

- [ ] **Step 4: Add a CHANGELOG entry to each.** Top of each `## [Unreleased]`-style section (match the existing file format), e.g. core: `- Add \`ToolSource\` trait and \`ToolSourceError\`; \`LlmAgentBuilder\` gains \`tool_source\`/\`tool_sources\`/\`shared_tool_source(s)\`/\`mcp_servers\` and an async \`build_resolved\` finalizer (SMA-410).`; mcp: `- Implement \`ToolSource\` for \`McpServerHandle\` (SMA-410).`; facade: `- Re-export the new core tool-source API; track core 0.5.11 / mcp 0.1.11 (SMA-410).`

- [ ] **Step 5: Verify the workspace still builds with the new pins.**

Run: `cargo build --workspace --all-features 2>&1 | tail -10`
Expected: success.

- [ ] **Step 6: commit.**

```bash
git add crates/paigasus-helikon-core/Cargo.toml crates/paigasus-helikon-mcp/Cargo.toml \
        crates/paigasus-helikon/Cargo.toml Cargo.toml \
        crates/paigasus-helikon-core/CHANGELOG.md crates/paigasus-helikon-mcp/CHANGELOG.md \
        crates/paigasus-helikon/CHANGELOG.md
git commit -m "chore(release): SMA-410 bump core 0.5.11, mcp 0.1.11, facade 0.4.7"
```

---

## Final verification (before opening the PR — Stage 5)

Run the full local CI gate (matches `.github/workflows/ci.yml`):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
mdbook build docs/book
```
All must be clean. (`Cargo.lock` is committed — include it if `cargo build` changed it.)

---

## Self-Review

**Spec coverage:**
- §5.1 `ToolSource` trait → Task 1 ✓
- §5.2 `ToolSourceError` → Task 1 ✓
- §5.3 builder field + `So` typestate + 5 registration methods + `mcp_servers` homogeneity / `shared_tool_sources` → Task 2 ✓
- §5.4 `build()` gating + `build_resolved` (concurrent resolve, static-first merge, deterministic `O(n)` source-scoped dedup, zero-source≡build, cancel-drop on first error) → Tasks 2+3 ✓
- §5.5 `LlmAgent` unchanged; sub-agent ordering → enforced by "no breaking change" constraint; sub-agent note is doc-only (Task 6 optional mention) ✓
- §5.6 `McpServerHandle: ToolSource` + disambiguation + same-length test → Task 5 ✓
- §6 facade reachability (no source edit, only bump) → Task 7 ✓
- §7 versioning (core/mcp/facade, pins, CHANGELOGs) → Task 7 ✓
- §8 docs (mdBook + 3 READMEs + doc-gate) → Task 6 ✓
- §9 tests (all core cases incl. typestate compile-fail via trybuild; mcp same-length) → Tasks 1–5 ✓
- §10 typestate guard (`So`) → Task 2 ✓ (approved as-is)

**Placeholder scan:** none — every code step has complete code. (Task 6 README edits are prose-guided because exact README contents must be read first; the snippets to add are specified.)

**Type consistency:** `ToolSource::tools` signature, `ToolSourceError` variants (`Resolution{source,cause}`, `DuplicateName{name}`, `Other`), `build_resolved` return type, and the `So`/`HasSources` markers are used identically across Tasks 1–5.
