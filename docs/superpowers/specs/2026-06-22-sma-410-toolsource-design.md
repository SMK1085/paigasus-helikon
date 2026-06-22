# SMA-410 — `ToolSource` trait + builder sugar for MCP tool discovery

- **Status:** Draft v2 — revised after adversarial spec challenge (pending GATE 1 approval)
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
- Builder sugar: `.tool_source(...)`, `.tool_sources(...)`, `.shared_tool_source(...)`, `.shared_tool_sources(...)`, `.mcp_servers(...)`.
- `McpServerHandle: ToolSource<Ctx>` in `paigasus-helikon-mcp`.

### Non-goals

- **Live re-discovery / hot reload** of an MCP server's tool list. Tools are resolved once, at construction.
- **Per-run dynamic sources.** Sources are fixed when the agent is built.
- **Changing behavior for agents that use no sources.** The existing sync `.build()` path is byte-for-byte unchanged.
- **Detecting duplicate names among purely static tools.** Two static `.tool(a).tool(a)` keep today's silent first-wins behavior (see D3). Only collisions *introduced by a resolved source* are rejected.

## 3. Ticket reconciliation (premise drift)

Two parts of the ticket's framing are stale against the current code; the design corrects them:

1. **The ticket says SMA-327 shipped `.tools(handle.tools::<Ctx>().await?)`** (async, fallible). The *actual* `McpServerHandle::tools::<Ctx>()` is **synchronous and infallible** — discovery already happened at `.connect()` time and is cached on the handle (`crates/paigasus-helikon-mcp/src/client/handle.rs:205`). So the `ToolSource` async/fallible signature is *more general* than what MCP needs today; the MCP impl is a thin, always-`Ok` wrapper.

2. **The ticket says "discovery resolves at run start (cache after first resolution)."** Run-start resolution would require storing sources + a memoized cache **on `LlmAgent`**, which is **not `#[non_exhaustive]`** (`crates/paigasus-helikon-core/src/agent.rs:228-231`, all `pub` fields, struct-literal construction is a documented escape hatch). Adding a field is therefore a **breaking** change → core `0.6.0` → and because every published sibling pins core via the workspace at `^0.5.x` (which does **not** match `0.6.0`), the *entire* core-dependent crate set would need a same-PR republish to avoid an unsatisfiable downstream `cargo` resolution. To avoid that, we resolve at an **async build finalizer** instead (Approach B below), which leaves `LlmAgent` untouched and keeps the change additive.

## 4. Decisions

Resolved with the maintainer during brainstorming:

| # | Decision | Choice |
|---|----------|--------|
| D1 | Error type for `ToolSource::tools()` | **Dedicated `ToolSourceError`** (not `ToolError`, whose `InvalidArgs`/`Denied`/`Other` variants are about *invoking* a tool, not *discovering* one). |
| D2 | Builder surface | **`.tool_source` + `.tool_sources` + `.mcp_servers` alias** (+ `shared_*` parity variants; see §5.3). |
| D3 | Name-collision policy | **Fail loudly — scoped to source-introduced collisions.** A resolved source's tool whose name already exists in the merged namespace (static tools or an earlier source) is rejected at `build_resolved()` (tools dispatch by name, so a silent duplicate is first-match-wins shadowing). Purely static-vs-static duplicates are **out of scope** and keep today's first-wins behavior. |
| D4 | Resolution model | **Approach B — additive async build finalizer.** Resolve sources in a new `.build_resolved().await?`; fold the resolved tools into the existing `tools` field; `LlmAgent` and `AgentError` stay untouched → additive → **patch** bumps, only the core+mcp+facade trio republishes. (Explicitly rejects the breaking "resolve at run start" Approach A.) |

**Consequence of D4 on D1:** because resolution and the duplicate-name check now happen in `build_resolved()` (construction time), there is **no run-time tool-source failure path**, so **`AgentError` is *not* extended** — the original "add `AgentError::ToolSource`" half of D1 is moot. `ToolSourceError` surfaces directly from `build_resolved()`. (A future ticket reintroducing run-start resolution is when `AgentError::ToolSource` would be added.)

## 5. Design

### 5.1 `ToolSource<Ctx>` trait (core)

`crates/paigasus-helikon-core/src/tool.rs` (next to `Tool`). This reuses the **exact, already-proven object-safe shape of `Tool`** (`tool.rs:64`: `#[async_trait] pub trait Tool<Ctx>: Send + Sync where Ctx: Send + Sync + 'static`, stored as `Arc<dyn Tool<Ctx>>`), so `Arc<dyn ToolSource<Ctx>>` is object-safe by the same precedent:

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
    /// with that [`ToolSourceError`]. Implementors that want labeled errors
    /// construct [`ToolSourceError::Resolution`] themselves (they know their
    /// own identity — see §5.2).
    async fn tools(&self) -> Result<Vec<std::sync::Arc<dyn Tool<Ctx>>>, ToolSourceError>;
}
```

Core does **not** re-export `async_trait`; downstream implementors add their own `async-trait` dep (the mcp crate already depends on it — `crates/paigasus-helikon-mcp/Cargo.toml:17`).

### 5.2 `ToolSourceError` (core)

```rust
/// Errors raised while resolving a [`ToolSource`] or merging resolved tools.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolSourceError {
    /// A source failed to produce its tools (e.g. a transport/discovery failure).
    ///
    /// Constructed by the failing [`ToolSource`] impl, which supplies its own
    /// `source` label; `build_resolved` propagates it unchanged.
    #[error("tool source {source:?} failed to resolve: {cause}")]
    Resolution {
        /// Caller-meaningful label for the failing source (supplied by the impl).
        source: String,
        /// Underlying cause.
        #[source]
        cause: anyhow::Error,
    },

    /// A resolved source introduced a tool whose name already exists in the
    /// merged namespace (static `.tools(...)` or an earlier source). Rejected
    /// at build time rather than silently shadowed, because tools dispatch by
    /// name (D3).
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

`#[non_exhaustive]` to match the other error enums (`ToolError`, `AgentError`). **`Resolution.source` is populated by the source, never synthesized by the finalizer** — `build_resolved` has only `Arc<dyn ToolSource>` trait objects and cannot know a source's identity, so it propagates whatever error a source returns. A source with no label simply returns `Other(anyhow)`. `DuplicateName` is the one variant the finalizer itself constructs (it owns the merge).

### 5.3 Builder changes (core)

`crates/paigasus-helikon-core/src/agent_builder.rs`.

**New field** on `LlmAgentBuilder`:

```rust
tool_sources: Vec<std::sync::Arc<dyn crate::ToolSource<Ctx>>>,
```

**New typestate dimension `So`** (`NoSources` / `HasSources`) gates the finalizers (§5.4). To keep the blast radius minimal, `So` is added **last with a default**:

```rust
pub struct LlmAgentBuilder<Ctx, M, T, N, Mo, So = NoSources>
where Ctx: Send + Sync + 'static { … , tool_sources: …, _state: PhantomData<fn() -> (N, Mo, So, T)> }
```

The default means **`LlmAgent::builder()`'s return type (`agent.rs:296`: `LlmAgentBuilder<Ctx, (), String, NoName, NoModel>`) and every external 5-arg mention stay valid unchanged** — verified no crate outside core even mentions the builder type. The required edits, enumerated against current line numbers:

| Site | Current | Change |
|------|---------|--------|
| struct decl `:31` | `<Ctx,M,T,N,Mo>` | add `So = NoSources`; add `tool_sources` field; widen `_state` tuple to `(N,Mo,So,T)` |
| `__new` impl `:53` | `<Ctx,(),String,NoName,NoModel>` | init `tool_sources: Vec::new()` (state stays `NoSources` via default) |
| any-state setters `:85` | `impl<Ctx,M,T,N,Mo> …<…,N,Mo>` | add `So`: `impl<Ctx,M,T,N,Mo,So> …<…,N,Mo,So>` (return same `So`) |
| `.name` impl `:253` | `impl<Ctx,M,T,Mo> …<NoName,Mo>` | add `So`; preserve `So` across `NoName→HasName` |
| `.model`/`.shared_model` impl `:283` | `impl<Ctx,M0,T,N> …<N,NoModel>` | add `So`; preserve `So` across `NoModel→HasModel` |
| `.build` impl `:329` | `impl<Ctx,M,T> …<HasName,HasModel>` | **gate to `NoSources`**: `…<HasName,HasModel,NoSources>` |
| `.output_type` impl `:363` | `impl<Ctx,M,T0,N,Mo> …<N,Mo>` | add `So` (preserve across `T→T2`) |

The state-preserving setters' struct literals already write `_state: PhantomData`, so the literals themselves are unchanged — only the `impl`/return-type generic lists gain `So`. The `Send`/`Sync` rationale at `agent_builder.rs:47-48` (`fn() -> _`) is preserved by keeping `So` inside the `fn() -> (…)` marker.

**Registration methods** — a new impl block generic over the incoming `So`, transitioning to `HasSources`:

```rust
impl<Ctx, M, T, N, Mo, So> LlmAgentBuilder<Ctx, M, T, N, Mo, So> where Ctx: Send + Sync + 'static {
    /// Append a tool source, discovered at `build_resolved()`.
    pub fn tool_source(self, s: impl crate::ToolSource<Ctx> + 'static)
        -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources>;
    /// Append a pre-wrapped tool source.
    pub fn shared_tool_source(self, s: std::sync::Arc<dyn crate::ToolSource<Ctx>>)
        -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources>;
    /// Append several **homogeneous** tool sources (e.g. `[handle_a, handle_b]`).
    pub fn tool_sources<I, S>(self, sources: I) -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources>
    where I: IntoIterator<Item = S>, S: crate::ToolSource<Ctx> + 'static;
    /// Append several **heterogeneous / pre-wrapped** tool sources.
    pub fn shared_tool_sources<I>(self, sources: I) -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources>
    where I: IntoIterator<Item = std::sync::Arc<dyn crate::ToolSource<Ctx>>>;
    /// Ergonomic alias for [`Self::tool_sources`], matching the MCP mental model.
    /// Despite the name, accepts any `ToolSource` (core is MCP-agnostic).
    pub fn mcp_servers<I, S>(self, servers: I) -> LlmAgentBuilder<Ctx, M, T, N, Mo, HasSources>
    where I: IntoIterator<Item = S>, S: crate::ToolSource<Ctx> + 'static;
}
```

**Homogeneity note (from the challenge):** a Rust array literal `[a, b]` requires `a` and `b` to be the *same* type, so `.mcp_servers([fs_server, weather_server])` works because both are `McpServerHandle`. To mix source *types* (e.g. an `McpServerHandle` and a custom `ToolSource`), callers use `.shared_tool_sources([Arc::new(a) as Arc<dyn ToolSource<_>>, Arc::new(b)])` or chain individual `.tool_source(...)` calls. This mirrors the existing `tools()` setter (`agent_builder.rs:139`), which already takes `Arc<dyn Tool<Ctx>>` items for the same reason.

### 5.4 Finalizers (core)

- **`build()` — unchanged, gated to `So = NoSources`.** Sync, infallible, returns `LlmAgent<Ctx, M, T>`. A user who has registered a source cannot call `.build()` — it is a **compile error**, preventing the silent "sources dropped" footgun.

- **`build_resolved()` — new, available for any `So` once `HasName + HasModel`** (`impl<Ctx,M,T,So> LlmAgentBuilder<Ctx,M,T,HasName,HasModel,So>`). Async, fallible:

  ```rust
  pub async fn build_resolved(self)
      -> Result<crate::LlmAgent<Ctx, M, T>, crate::ToolSourceError>;
  ```

  Algorithm:
  1. Resolve all sources **concurrently** (`futures_util::future::try_join_all`), which **preserves input (registration) order** in its `Ok(Vec<_>)`. The first source to return `Err` aborts with that `ToolSourceError`; the other in-flight resolutions are **cancel-dropped, not awaited** (moot for today's synchronous MCP impl; documented for future async sources).
  2. Build the merged tool list: static `self.tools` first, then resolved tools in source-registration order.
  3. **Duplicate-name check (deterministic, `O(n)`):** seed a `HashSet<&str>` with the static tool names (so static-vs-static is *not* flagged — D3), then scan the resolved tools in order; the **first resolved name already in the set** is returned as `ToolSourceError::DuplicateName { name }`. (Each newly-seen resolved name is inserted, so source-vs-earlier-source collisions are caught too.)
  4. Construct `LlmAgent` with `tools = merged` and every other field exactly as `build()` would.

  **Zero-source `build_resolved()`** resolves nothing, skips straight to step 4, and (because the dup check only scans *resolved* tools) **never newly errors** — so it is byte-equivalent to `build()` and is always a safe choice. This removes the asymmetry the challenge flagged: static-vs-static duplicates behave identically under `build()` and `build_resolved()`.

### 5.5 `LlmAgent` (core)

**Unchanged.** No new field, no `#[non_exhaustive]` change, `AgentError` untouched. Resolved tools live in the existing `pub tools` field.

**Sub-agents:** a child agent that needs MCP tools is resolved *before* attachment: `let child = LlmAgent::builder()…mcp_servers([…]).build_resolved().await?; parent_builder.handoff(child)`. `.handoff(...)` (`agent_builder.rs:148`) takes an already-built `impl Agent<Ctx>`, so the async resolution happens before the (sync) handoff — no change to handoff/`agent_as_tool` needed; documented so users don't try to call `.build_resolved()` inside `.handoff()`.

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
        Ok(<McpServerHandle>::tools::<Ctx>(self))
    }
}
```

- **Disambiguation (challenge Q1):** the inherent `tools<Ctx>(&self)` (`handle.rs:205`) and the trait `ToolSource::tools()` share a name. The impl calls the inherent one via the unambiguous **type-qualified path** `<McpServerHandle>::tools::<Ctx>(self)` (inherent methods take precedence in path syntax; the angle-bracketed form removes any doubt and guarantees no recursion). The test (§9) asserts the trait method returns a `Vec` of the **same concrete length** as the inherent method, proving no recursion / no divergence — not merely that it compiles.
- **Coherence (challenge Q2):** `impl<Ctx> ToolSource<Ctx> for McpServerHandle` is a local-trait-for-local-type impl with no overlap (no other `ToolSource` impl for the handle exists, and a downstream crate cannot add one — foreign trait, foreign type). `Ctx: Send + Sync + 'static` is the only bound needed. Sound.
- The inherent `McpServerHandle::tools::<Ctx>()` stays public and unchanged (the explicit/manual path).

## 6. Public API & facade reachability

| Symbol | Crate | Kind |
|--------|-------|------|
| `ToolSource<Ctx>` | core | new trait (async `tools()`) |
| `ToolSourceError` | core | new enum (`Resolution`, `DuplicateName`, `Other`) |
| `LlmAgentBuilder::{tool_source, shared_tool_source, tool_sources, shared_tool_sources, mcp_servers}` | core | new methods (flip `So → HasSources`) |
| `LlmAgentBuilder::build_resolved` | core | new async finalizer (`Result<_, ToolSourceError>`) |
| `NoSources` / `HasSources` | core | new typestate markers (documented like `NoName`/`HasName`) |
| `impl ToolSource<Ctx> for McpServerHandle` | mcp | new impl |

**Facade reachability (challenge MINOR 1 — corrected):** the facade re-exports core as a **module alias** (`crates/paigasus-helikon/src/lib.rs:4`: `pub use paigasus_helikon_core as core;`) and core globs its modules (`pub use tool::*` `:56`, `pub use agent_builder::*` `:44`). So all new core symbols are reachable as `paigasus_helikon::core::*` **automatically — there is no explicit facade re-export list to edit.** The facade's only obligation is the version bump (§7). The mcp impl reaches users via the existing `#[cfg(feature = "mcp")] pub use paigasus_helikon_mcp as mcp`.

## 7. Versioning & release mechanics

The change is **additive** (no breaking surface — adding methods, a defaulted type param, new public types; no removed/changed signatures; no struct-field changes to `LlmAgent`), so:

| Crate | Bump | Why |
|-------|------|-----|
| `paigasus-helikon-core` | `0.5.10` → **`0.5.11`** (patch) | New `ToolSource`/`ToolSourceError` + builder methods. Additive. |
| `paigasus-helikon-mcp` | `0.1.10` → **`0.1.11`** (patch) | New `ToolSource` impl. Uses the **same-PR** core API → triggers the documented same-PR core-bump caveat (mcp's `cargo publish --verify` builds against the *registry* core, which must already carry `ToolSource`). |
| `paigasus-helikon` (facade) | `0.4.6` → **`0.4.7`** (patch) | Per the "same-PR manual bump defeats `dependencies_update`" caveat — bump so the facade republishes tracking the current core/mcp reqs. |

For each: bump the crate `Cargo.toml` `version`, the matching `[workspace.dependencies]` pin in the root `Cargo.toml`, and the crate `CHANGELOG.md`. release-plz then publishes in dependency order (core → mcp/facade), so mcp verifies against the fresh core.

**No other crate is touched** — confirmed every sibling pins core at `^0.5.x` (root `Cargo.toml:73-78`), which `0.5.11` satisfies, and no sibling references the builder type or `impl`s `ToolSource`, so the eight-crate republish a breaking `0.6.0` would have forced is avoided.

## 8. Documentation impact

Per the repo's "keep the book and READMEs current in the same PR" rules:

- **mdBook**: the MCP / tools page(s) under `docs/book/src/` — add the `.mcp_servers([...]).build_resolved().await?` ergonomic alongside the existing explicit `.tools(handle.tools())` form. (`mdbook build docs/book` must stay clean — linkcheck is `warning-policy = "error"`.)
- **`crates/paigasus-helikon-mcp/README.md`**: show the new `ToolSource` ergonomic.
- **`crates/paigasus-helikon-core/README.md`**: mention `ToolSource` in the tools/builder section.
- **`crates/paigasus-helikon/README.md`** (facade): update its MCP usage example if present. Crate roster / feature→module map is unchanged, so the root `README.md` needs no edit (confirm during impl).
- **Doc gate (`-D warnings`, `missing_docs` workspace-wide, 80% coverage):** every new `pub` item needs `///` — the trait, its method, `ToolSourceError` + all variants + fields, `NoSources`/`HasSources` (match the `NoName`/`HasName` doc style at `agent_builder.rs:6-18`), all five registration methods, and `build_resolved`. A single undocumented `pub` item fails the `docs` job, not just the coverage aggregator.

## 9. Testing strategy

**core** (with a mock `ToolSource` test double incl. a call-counter, and a failing mock):
- `build_resolved()` with zero sources == `build()` (same tools; no error even with duplicate *static* names).
- Single source: resolved tools appended after static tools, in order.
- Multiple sources: registration order preserved under concurrent resolution.
- Source-introduced duplicate → `ToolSourceError::DuplicateName` with the **expected `name`** (cases: source-vs-static, source-vs-earlier-source); assert the exact name to lock the deterministic scan (§5.4 step 3).
- Static-vs-static duplicate with a source present is **not** flagged (only source-introduced collisions are) — guards the scoping in D3.
- A failing source → its `ToolSourceError` propagated; build aborts.
- Each source's `tools()` is invoked **exactly once** (call-counter).
- Typestate: `.tool_source(...)` then `.build()` is a **compile error** — a `trybuild` compile-fail case (the repo already has the trybuild toolchain gate from SMA-349). The fixture must fail to compile *cleanly* (expected stderr captured).

**mcp**:
- `McpServerHandle` implements `ToolSource<Ctx>` and the trait `tools()` returns a `Vec` of the **same concrete length** as the inherent `.tools::<Ctx>()` (against the existing in-crate test server fixture) — proves disambiguation, no recursion.

**facade**:
- The new symbols are reachable through `paigasus_helikon::core::*`; a compile-level smoke test of `.mcp_servers([...]).build_resolved()` behind the `mcp` feature.

## 10. Open sub-decision for GATE 1

**Typestate guard vs. wrapper type.** §5.3/§5.4 use a 6th typestate parameter `So` (defaulted) to make `.build()` a compile error once a source is registered. The challenge proposed a wrapper type (`SourcedLlmAgentBuilder`) returned by the registration methods, exposing only `build_resolved()` + source setters.

**Recommendation: keep the `So` typestate parameter.** Rationale:
- The wrapper would have to **either** duplicate the `NoName/NoModel` gating machinery (so `build_resolved` can still require name+model) **or** force sources to be registered *last* (after `.name()/.model()` and any `.handoff()/.instructions()/.tool()`), breaking the builder's current **order-independence** — every existing setter is an "any-state" method callable in any order.
- With the **defaulted** `So`, the blast radius is bounded and mechanical (the §5.3 table): `builder()`'s signature and all external references are unchanged; only in-crate `impl` headers thread `So`. No struct literals change.
- It is consistent with the builder's existing typestate-safety philosophy (`NoName`/`HasName`, `NoModel`/`HasModel`) and with the "fail loudly" choice (D3).

The lighter fallback remains **doc-only** (ungated `.build()` that silently drops sources) if the maintainer prefers minimal code over the compile-time guard. **Also bikeshed: the finalizer name `build_resolved`** (alternatives: `build_async`, `try_build`, `connect`).

## Appendix — Adversarial challenge changelog (v1 → v2)

The `spec-challenger` (Opus) returned **APPROVE-WITH-CHANGES**. Folded in:

- **[BLOCKER] Typestate generics were internally inconsistent / under-specified.** v2 §5.3 pins one canonical order with a **defaulted `So`**, shows the struct decl + `_state` marker, and enumerates every impl/return-type edit site by line; confirmed `builder()` and all external refs stay valid (no sibling references the builder type).
- **[BLOCKER] `mcp_servers([a, b])` only works for homogeneous source types.** v2 §5.3 documents the homogeneity constraint and adds `.shared_tool_sources(IntoIterator<Item = Arc<dyn ToolSource>>)` for the heterogeneous case.
- **[MAJOR] `So` over-engineering vs. wrapper.** Kept `So` with explicit justification (order-independence; bounded blast radius via default); recorded the wrapper as considered-and-rejected and surfaced as the GATE-1 sub-decision (§10).
- **[MAJOR] Duplicate-name asymmetry.** v2 D3/§5.4 scope the check to **source-introduced** collisions only; static-vs-static stays first-wins, so zero-source `build_resolved()` ≡ `build()`.
- **[MAJOR] Unspecified de-dup order.** v2 §5.4 step 3 specifies a deterministic insertion-order `HashSet<&str>` scan, `O(n)`, first source-introduced name reported.
- **[MAJOR] `Resolution.source` unpopulatable.** v2 §5.2 clarifies the source constructs `Resolution` itself; the finalizer only ever propagates source errors and constructs `DuplicateName`.
- **[MINOR] Facade re-export.** v2 §6 corrected to the module-alias auto-flow; no facade source edit.
- **[MINOR] Doc-gate surface, async-trait precedent/dep, sub-agent ordering.** Folded into §5.1/§5.5/§8.
- **[QUESTIONS] inherent-vs-trait disambiguation, coherence, sibling arity, zero-source dup, concurrent cancellation.** Resolved in §5.4/§5.6/§7 (verified no sibling references the builder; concurrent cancel-drop documented).

Rejected: none — all findings were justified and folded in (the only deferral is the §10 wrapper alternative, surfaced for GATE 1 rather than silently dropped).
