# SMA-410 — `ToolSource` trait + builder sugar for MCP tool discovery

- **Status:** Draft (pending GATE 1 approval)
- **Date:** 2026-06-22
- **Ticket:** [SMA-410](https://linear.app/smaschek/issue/SMA-410)
- **Related:** SMA-327 (the `paigasus-helikon-mcp` rmcp wrapper)
- **Crates touched:** `paigasus-helikon-core`, `paigasus-helikon-mcp`, `paigasus-helikon` (facade)

## 1. Problem

SMA-327 shipped MCP support but forced verbose, explicit tool wiring:

```rust
let handle = McpServerHandle::connect(transport, opts).await?;
let agent = LlmAgent::builder()
    .name("assistant")
    .model(model)
    .tools(handle.tools::<Ctx>())   // user must thread discovery through by hand
    .build();
```

The original Notion design wanted the ergonomic:

```rust
let agent = LlmAgent::builder()
    .name("assistant")
    .model(model)
    .mcp_servers([fs_server, weather_server])   // auto-discovery
    .build_resolved().await?;
```

SMA-327 couldn't deliver this because **`paigasus-helikon-core` cannot depend on `paigasus-helikon-mcp`** (the dependency runs the other way), so the builder had no way to name an MCP server.

## 2. Goal

Restore the ergonomic via a **core-side abstraction** that the mcp crate implements:

- A `trait ToolSource<Ctx>` in `paigasus-helikon-core`.
- Builder sugar: `.tool_source(...)`, `.tool_sources(...)`, `.mcp_servers(...)`.
- `McpServerHandle: ToolSource<Ctx>` in `paigasus-helikon-mcp`.

### Non-goals

- **Live re-discovery / hot reload** of an MCP server's tool list. Tools are resolved once, at construction.
- **Per-run dynamic sources.** Sources are fixed when the agent is built.
- **Changing behavior for agents that use no sources.** The existing sync `.build()` path is byte-for-byte unchanged.

## 3. Ticket reconciliation (premise drift)

Two parts of the ticket's framing are stale against the current code; the design corrects them:

1. **The ticket says SMA-327 shipped `.tools(handle.tools::<Ctx>().await?)`** (async, fallible). The *actual* `McpServerHandle::tools::<Ctx>()` is **synchronous and infallible** — discovery already happened at `.connect()` time and is cached on the handle (`crates/paigasus-helikon-mcp/src/client/handle.rs:205`). So the `ToolSource` async/fallible signature is *more general* than what MCP needs today; the MCP impl is a thin wrapper.

2. **The ticket says "discovery resolves at run start (cache after first resolution)."** Run-start resolution would require storing sources + a memoized cache **on `LlmAgent`**, which is **not `#[non_exhaustive]`** (`crates/paigasus-helikon-core/src/agent.rs:228-231`, all `pub` fields, struct-literal construction is a documented escape hatch). Adding a field is therefore a **breaking** change → core `0.6.0` → and because every published sibling pins core via the workspace at `^0.5.x` (which does **not** match `0.6.0`), the *entire* core-dependent crate set would need a same-PR republish to avoid an unsatisfiable downstream `cargo` resolution. To avoid that, we resolve at an **async build finalizer** instead (Approach B below), which leaves `LlmAgent` untouched and keeps the change additive.

## 4. Decisions

Resolved with the maintainer during brainstorming:

| # | Decision | Choice |
|---|----------|--------|
| D1 | Error type for `ToolSource::tools()` | **Dedicated `ToolSourceError`** (not `ToolError`, whose `InvalidArgs`/`Denied`/`Other` variants are about *invoking* a tool, not *discovering* one). |
| D2 | Builder surface | **`.tool_source(one)` + `.tool_sources(many)` + `.mcp_servers(many)` alias** (full surface; satisfies the ticket's literal API and reads naturally for the common MCP case). |
| D3 | Name-collision policy | **Fail loudly.** A duplicate tool name across static tools + resolved sources is rejected at resolution time (tools dispatch by name, so a silent duplicate is first-match-wins shadowing). |
| D4 | Resolution model | **Approach B — additive async build finalizer.** Resolve sources in a new `.build_resolved().await?`; fold the resolved tools into the existing `tools` field; `LlmAgent` struct unchanged. Core change is additive → **patch** bump, only the core+mcp+facade trio republishes. |

**Consequence of D4 on D1:** because resolution and the duplicate-name check now happen in `build_resolved()` (construction time), there is **no run-time tool-source failure path**, so **`AgentError` is *not* extended** — the original "add `AgentError::ToolSource`" half of D1 is moot. `ToolSourceError` surfaces directly from `build_resolved()`. (If a future ticket reintroduces run-start resolution, that's when `AgentError::ToolSource` would be added.)

## 5. Design

### 5.1 `ToolSource<Ctx>` trait (core)

`crates/paigasus-helikon-core/src/tool.rs` (next to `Tool`):

```rust
/// An asynchronous provider of [`Tool`]s, resolved when the agent is built.
///
/// Implemented by anything that can produce tools through (potentially)
/// async work — e.g. an MCP server handle that discovered its tools over a
/// transport. Registered on the builder via [`LlmAgentBuilder::tool_source`],
/// [`tool_sources`], or [`mcp_servers`], and resolved exactly once by
/// [`LlmAgentBuilder::build_resolved`].
#[async_trait::async_trait]
pub trait ToolSource<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Resolve the tools this source provides.
    ///
    /// Called once, at `build_resolved()`. Returning `Err` aborts the build
    /// with that [`ToolSourceError`].
    async fn tools(&self) -> Result<Vec<std::sync::Arc<dyn Tool<Ctx>>>, ToolSourceError>;
}
```

Object-safe (via `async_trait`), so it can be stored as `Arc<dyn ToolSource<Ctx>>`.

### 5.2 `ToolSourceError` (core)

```rust
/// Errors raised while resolving a [`ToolSource`] or merging resolved tools.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolSourceError {
    /// A source failed to produce its tools (e.g. a transport/discovery failure).
    #[error("tool source {source:?} failed to resolve: {cause}")]
    Resolution {
        /// Caller-meaningful label for the failing source.
        source: String,
        /// Underlying cause.
        #[source]
        cause: anyhow::Error,
    },

    /// Two tools in the merged namespace (static `.tools(...)` + all resolved
    /// sources) share a name. Rejected at build time rather than silently
    /// shadowed, because tools dispatch by name (D3).
    #[error("duplicate tool name {name:?} across tools and tool sources")]
    DuplicateName {
        /// The conflicting tool name.
        name: String,
    },

    /// Escape hatch for arbitrary source failures.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

`#[non_exhaustive]` to match the other error enums in the crate (`ToolError`, `AgentError`).

### 5.3 Builder changes (core)

`crates/paigasus-helikon-core/src/agent_builder.rs`.

**New field** on `LlmAgentBuilder`:

```rust
tool_sources: Vec<std::sync::Arc<dyn crate::ToolSource<Ctx>>>,
```

**New typestate dimension `So`** (`NoSources` / `HasSources`) so the finalizers are correctly gated (D4 footgun guard — see 5.4). The builder's `_state` marker widens from `PhantomData<fn() -> (N, Mo, T)>` to `PhantomData<fn() -> (N, Mo, So, T)>`; the default is `NoSources`.

**Registration methods** (move the builder `NoSources → HasSources`):

```rust
/// Append a tool source whose tools are discovered at `build_resolved()`.
pub fn tool_source(self, s: impl crate::ToolSource<Ctx> + 'static)
    -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources>;

/// Append a pre-wrapped tool source.
pub fn shared_tool_source(self, s: std::sync::Arc<dyn crate::ToolSource<Ctx>>)
    -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources>;

/// Append several tool sources.
pub fn tool_sources<I, S>(self, sources: I)
    -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources>
where I: IntoIterator<Item = S>, S: crate::ToolSource<Ctx> + 'static;

/// Ergonomic alias for [`Self::tool_sources`], matching the MCP mental model.
/// Despite the name, accepts any `ToolSource` (core is MCP-agnostic).
pub fn mcp_servers<I, S>(self, servers: I)
    -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources>
where I: IntoIterator<Item = S>, S: crate::ToolSource<Ctx> + 'static;
```

(`shared_tool_source` is included for parity with the existing `shared_tool` / `shared_instructions` / `shared_handoff` convention.)

### 5.4 Finalizers (core)

- **`build()` — unchanged, gated to `So = NoSources`.** Sync, infallible, returns `LlmAgent<Ctx, M, T>`. A user who has registered a source cannot call `.build()` — it is a **compile error**, preventing the silent "sources dropped" footgun.

- **`build_resolved()` — new, available for any `So` once `HasName + HasModel`.** Async, fallible:

  ```rust
  pub async fn build_resolved(self)
      -> Result<crate::LlmAgent<Ctx, M, T>, crate::ToolSourceError>;
  ```

  Algorithm:
  1. Resolve all sources **concurrently** (`futures_util::future::try_join_all`); the first `Err` aborts with that `ToolSourceError`.
  2. Build the merged tool list: static `self.tools` first, then resolved tools in source-registration order.
  3. **Duplicate-name check** across the whole merged list → `ToolSourceError::DuplicateName` on the first clash (D3).
  4. Construct `LlmAgent` with `tools = merged` and every other field exactly as `build()` would.

  With zero sources, `build_resolved()` is just an async, infallible-in-practice wrapper over `build()` (so `.build_resolved()` is always a safe choice). The duplicate-name check runs only on this path, so **existing no-source agents are unaffected** (today two static tools with the same name silently first-win; that behavior is preserved for `build()`).

### 5.5 `LlmAgent` (core)

**Unchanged.** No new field, no `#[non_exhaustive]` change, `AgentError` untouched. Resolved tools live in the existing `pub tools` field.

### 5.6 `McpServerHandle: ToolSource<Ctx>` (mcp)

`crates/paigasus-helikon-mcp/src/client/handle.rs`:

```rust
#[async_trait::async_trait]
impl<Ctx> ToolSource<Ctx> for McpServerHandle
where
    Ctx: Send + Sync + 'static,
{
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool<Ctx>>>, ToolSourceError> {
        // Discovery already happened at connect(); the inherent `tools()` is a
        // cheap, infallible adapter, so this never errors today.
        Ok(McpServerHandle::tools::<Ctx>(self))
    }
}
```

**Sharp edge:** the inherent method and the trait method are both named `tools`. The impl body must call the **inherent** one via the disambiguating path `McpServerHandle::tools::<Ctx>(self)` (inherent methods take precedence in path syntax); a bare `self.tools()` would resolve to the inherent method too, but the explicit path makes intent unmistakable and avoids any accidental recursion. To be verified at implementation time with a focused test.

The existing inherent `McpServerHandle::tools::<Ctx>()` stays public and unchanged (it remains the explicit/manual path).

## 6. Public API summary

| Symbol | Crate | Kind | Notes |
|--------|-------|------|-------|
| `ToolSource<Ctx>` | core | new trait | async `tools()` |
| `ToolSourceError` | core | new enum | `Resolution`, `DuplicateName`, `Other` |
| `LlmAgentBuilder::tool_source` / `shared_tool_source` / `tool_sources` / `mcp_servers` | core | new methods | flip `So → HasSources` |
| `LlmAgentBuilder::build_resolved` | core | new async finalizer | returns `Result<_, ToolSourceError>` |
| `NoSources` / `HasSources` | core | new typestate markers | exported alongside `NoName`/`HasName` |
| `impl ToolSource<Ctx> for McpServerHandle` | mcp | new impl | wraps inherent `tools()` |

All re-exported through the facade: core symbols via `pub use paigasus_helikon_core` (the facade `pub use`s core types — confirm `ToolSource`/`ToolSourceError`/`NoSources`/`HasSources` are added to any explicit re-export list with `///` docs so the `-D warnings` docs gate stays green); the mcp impl via the existing `#[cfg(feature = "mcp")] pub use paigasus_helikon_mcp as mcp`.

## 7. Versioning & release mechanics

The change is **additive** (no breaking surface), so:

| Crate | Bump | Why |
|-------|------|-----|
| `paigasus-helikon-core` | `0.5.10` → **`0.5.11`** (patch) | New `ToolSource`/`ToolSourceError` + builder methods. Additive. |
| `paigasus-helikon-mcp` | `0.1.10` → **`0.1.11`** (patch) | New `ToolSource` impl. Uses the **same-PR** core API → triggers the documented same-PR core-bump caveat (mcp's `cargo publish --verify` builds against the *registry* core, which must already carry `ToolSource`). |
| `paigasus-helikon` (facade) | `0.4.6` → **`0.4.7`** (patch) | Per the "same-PR manual bump defeats `dependencies_update`" caveat — bump the facade so it republishes tracking the current core/mcp reqs. |

For each: bump the crate `Cargo.toml` `version`, the matching `[workspace.dependencies]` pin in the root `Cargo.toml`, and the crate `CHANGELOG.md`. release-plz then publishes in dependency order (core → mcp/facade), so mcp verifies against the fresh core.

**No other crate is touched** — the additive core bump keeps `^0.5.10` satisfied for every other sibling, so the eight-crate republish that a breaking `0.6.0` would have forced is avoided.

## 8. Documentation impact

Per the repo's "keep the book and READMEs current in the same PR" rules:

- **mdBook**: the MCP / tools page(s) under `docs/book/src/` — add the `.mcp_servers([...]).build_resolved().await?` ergonomic alongside the existing explicit `.tools(handle.tools())` form. (`mdbook build docs/book` must stay clean.)
- **`crates/paigasus-helikon-mcp/README.md`**: show the new `ToolSource` ergonomic.
- **`crates/paigasus-helikon-core/README.md`**: mention `ToolSource` in the tools/builder section.
- **`crates/paigasus-helikon/README.md`** (facade): if its usage example shows MCP wiring, update it. The crate roster / feature→module map is unchanged, so the root `README.md` likely needs no edit (confirm).

## 9. Testing strategy

**core** (with a mock `ToolSource` test double, incl. a call-counter):
- `build_resolved()` with zero sources == `build()` (same tools).
- Single source: resolved tools appended after static tools, in order.
- Multiple sources: registration order preserved; concurrent resolution.
- Duplicate name → `ToolSourceError::DuplicateName` (cases: static-vs-source, source-vs-source).
- A failing source → `ToolSourceError` propagated; build aborts.
- Each source's `tools()` is invoked **exactly once**.
- Typestate: `.tool_source(...)` then `.build()` is a **compile-fail** (trybuild or a documented `compile_fail` doctest).

**mcp**:
- `McpServerHandle` implements `ToolSource<Ctx>` and returns the same tools as the inherent `.tools::<Ctx>()` (against the existing in-crate test server fixture).
- Disambiguation: trait `tools()` returns the inherent result (no recursion).

**facade**:
- The new symbols are reachable through the facade; a compile-level smoke test of `.mcp_servers([...]).build_resolved()` behind the `mcp` feature.

## 10. Risks / open sub-decision

- **Sub-decision (for GATE 1):** the `So` typestate guard (5.4) adds a typestate dimension that threads through every builder impl block — mechanical but wide. The lighter alternative is **doc-only** (keep `.build()` ungated, document that it drops sources). Recommendation: **keep the typestate guard** — it's consistent with the existing `NoName`/`NoModel` safety philosophy and the "fail loudly" choice (D3), turning a silent footgun into a compile error. Flagged so it can be revisited.
- **Finalizer name** `build_resolved` is a bikeshed; alternatives considered: `build_async`, `try_build`, `connect`. `build_resolved` chosen for "build after resolving sources." Open to change in review.
- **Concurrent resolution** assumes sources are independent; correct for MCP handles (independent connections).
