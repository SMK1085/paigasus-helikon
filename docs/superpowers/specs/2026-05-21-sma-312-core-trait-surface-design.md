# SMA-312 — Core trait surface (Model, Tool, Agent, Session, Guardrail, Hook, Runner) — design

- **Linear:** [SMA-312](https://linear.app/smaschek/issue/SMA-312/define-core-trait-surface-model-tool-agent-session-guardrail-hook)
- **Branch:** `feature/sma-312-define-core-trait-surface-model-tool-agent-session-guardrail`
- **Status:** design (awaiting implementation plan)
- **Author:** Sven Maschek
- **Date:** 2026-05-21

## 1. Goal

Land the seven canonical object-safe traits of `paigasus-helikon-core` — `Model`, `Tool<Ctx>`, `Agent<Ctx>`, `Session`, `Guardrail<Ctx>`, `Hook<Ctx>`, `Runner<Ctx>` — together with the minimum set of named carrier types (events, errors, contexts, request/response shapes) that the trait signatures reference. The goal is *name-stable surface*: downstream tickets (provider crates, runtime crates, the agent loop, the macro crate) can begin work in parallel because the names they need to import already exist, even if their internal shapes will grow.

The Linear ticket's acceptance criteria are:

1. All seven traits compile and have rustdoc examples.
2. Trait objects can be constructed without GAT/AFIT issues.
3. No silent retries anywhere (per ADR-10).

This design meets all three.

This ticket explicitly does **not** ship the agent loop (`LoopState`), the permissions layer (`PermissionPolicy`/`PermissionMode`), the canonical wire-format `Item`, any default backend (e.g. `MemorySession`, `OpenAiModel`), or the `#[tool]` macro. Each is a separate SMA-* ticket. See §10.

## 2. Decisions and rationale

Seven decisions, all derived from the Notion ADRs and Architecture pages. Each line that says "per ADR-N" is a non-discretionary input from the Notion source-of-truth; the others are local choices made in this brainstorming pass.

| Decision | Choice | Rationale |
|---|---|---|
| Scope of carrier types | **Name-stable carriers with minimum-fields**: types referenced by trait signatures exist publicly, carry just enough field shape to be useful, and are `#[non_exhaustive]` where they're enums. | Lets follow-up tickets fill in field shapes without renaming. The two rejected alternatives — `()` type aliases (rustdoc examples become hollow, and every follow-up requires a workspace-wide rename) and verbatim Notion build-out (would pull in `LoopState`, permissions, full `Item`, etc., turning SMA-312 into multi-week scope) are both worse trade-offs. |
| Async dispatch | **`#[async_trait::async_trait]` on every object-safe trait** (per ADR-2). | Native AFIT is stable since 1.75 but not dyn-safe without experimental `dyn*`. Every trait in this ticket must be object-safe so users can hold `Vec<Arc<dyn Tool<Ctx>>>`, `Box<dyn Model>`, etc. The one-allocation-per-call cost is invisible against network round-trips. |
| `Send + Sync` bounds | **Every trait is `Send + Sync`; every `Ctx` type parameter is bound `Ctx: Send + Sync + 'static`.** | Required for crossing `.await` points in multi-threaded executors and for sharing `Arc<dyn Trait>` across tasks. `'static` matches the lifetime of `BoxStream<'static, ...>` returned by the streaming methods. |
| Streaming primitive | **`futures_core::stream::BoxStream<'static, T>`** for `Model::invoke` and `Agent::run`; `Runner::run_streamed` returns a `RunResultStreaming` wrapper carrying its own `BoxStream`. | `BoxStream` is the lowest-common-denominator boxed-stream type that doesn't drag in `tokio` at the core layer. Compatible with `tokio-stream`, `async-stream`, and any executor. |
| Cancellation | **`tokio_util::sync::CancellationToken`** passed by reference to `Model::invoke`. | Notion ADR-1 names this explicitly. `tokio-util` with `default-features = false, features = ["rt"]` is the smallest dependency footprint — no tokio runtime dependency at core. Users on non-tokio executors can still construct and observe `CancellationToken`s; only the helper combinators require a runtime. Re-exported as `paigasus_helikon_core::CancellationToken` for ergonomics. |
| Error model | **One `thiserror` enum per domain, all `#[non_exhaustive]`, all with an `Other(anyhow::Error)` escape hatch** (per ADR-10). | `thiserror` at the library boundary, `anyhow` as the escape hatch for arbitrary upstream failures. `#[non_exhaustive]` lets us add variants in a minor version. Six enums: `ModelError`, `ToolError`, `SessionError`, `GuardrailError`, `AgentError`, `RunError`. `ToolError::InvalidArgs { schema_errors }` is the single recoverable variant — the runner is permitted to feed schema errors back to the model once before surfacing `AgentError::InvalidStructuredOutput` (per ADR-10). No other recovery is allowed. |
| Module layout | **One module per domain** (`model`, `tool`, `session`, `guardrail`, `hook`, `agent`, `runner`, `context`); `lib.rs` re-exports the full surface flat. | Keeps each module small enough to hold in context. Users import `paigasus_helikon_core::Tool` not `paigasus_helikon_core::tool::Tool`. Mirrors the Notion sub-page structure 1:1 so cross-references stay obvious. |

### 2.1 Why this is a `feat(core)` and not a `chore(core)`

The bootstrap-must-be-`chore` rule in CLAUDE.md applied specifically to the SMA-307 release-plz bootstrap, where a `feat` commit touching every `Cargo.toml` would have caused release-plz to attribute a version bump to every crate in the workspace. SMA-312 lands net-new public API in a single crate (`paigasus-helikon-core`), which is precisely what `feat(core):` is for. release-plz will bump `paigasus-helikon-core` to a new pre-1.0 version on merge; that is the intended behavior post-bootstrap.

## 3. Files added / modified

### Added

| Path | Purpose |
|---|---|
| `crates/paigasus-helikon-core/src/lib.rs` | Replace stub with crate-level docs and flat public re-exports (`pub use model::*;` etc.). |
| `crates/paigasus-helikon-core/src/model.rs` | `Model` trait + `ModelRequest`, `ModelEvent`, `ModelCapabilities`, `FinishReason`, `ModelError`. |
| `crates/paigasus-helikon-core/src/tool.rs` | `Tool<Ctx>` trait + `ToolContext<Ctx>`, `ToolOutput`, `ToolError`. |
| `crates/paigasus-helikon-core/src/session.rs` | `Session` trait + `SessionEvent`, `SequenceId`, `ConversationSnapshot`, `SessionError`. |
| `crates/paigasus-helikon-core/src/guardrail.rs` | `Guardrail<Ctx>` trait + `GuardrailInput<'a>`, `GuardrailVerdict`, `GuardrailKind`, `GuardrailError`. |
| `crates/paigasus-helikon-core/src/hook.rs` | `Hook<Ctx>` trait + `HookEvent`, `HookDecision`. |
| `crates/paigasus-helikon-core/src/agent.rs` | `Agent<Ctx>` trait + `AgentInput`, `AgentEvent`, `AgentError`. |
| `crates/paigasus-helikon-core/src/runner.rs` | `Runner<Ctx>` trait + `RunConfig`, `RunResult`, `RunResultStreaming`, `RunError`. |
| `crates/paigasus-helikon-core/src/context.rs` | `RunContext<Ctx>` + the canonical re-export of `CancellationToken`. |
| `crates/paigasus-helikon-core/tests/object_safety.rs` | Integration test that constructs `Box<dyn Trait>` and `Vec<Arc<dyn Trait>>` for every object-safe trait against a trivial impl. Locks AC #2 in CI. |

### Modified

| Path | Change |
|---|---|
| `Cargo.toml` (workspace root) | Add `futures-core = "0.3"` and `tokio-util = { version = "0.7", default-features = false, features = ["rt"] }` to `[workspace.dependencies]`. See §8. |
| `crates/paigasus-helikon-core/Cargo.toml` | Add the seven runtime deps as `dep.workspace = true` (`async-trait`, `thiserror`, `anyhow`, `serde`, `serde_json`, `futures-core`, `tokio-util`). |
| `docs/book/src/concepts/core-primitives.md` | Replace the stub callout with a one-paragraph "the seven traits are defined in [`paigasus-helikon-core`](https://docs.rs/paigasus-helikon-core); rustdoc is the canonical reference until this page graduates" note. The stubs on the other concept pages (`tools.md`, `model-providers.md`, `sessions.md`, `permissions-guardrails-hooks.md`) stay as-is — they describe features that aren't fully landed yet. |

### Not modified

- **Facade crate `paigasus-helikon`** — already re-exports `paigasus-helikon-core` unconditionally per SMA-304. The new surface flows through automatically. No facade edit needed; verified by `cargo check -p paigasus-helikon` in §9.
- **`CLAUDE.md`** — no new non-obvious convention warrants a top-level callout. The "`async-trait` on object-safe traits" and "MSRV 1.75" rules already live there; this ticket exercises them but doesn't change them.
- **`.github/rulesets/main-protection-checks.json`** — no new CI gate, only the existing `fmt` / `clippy` / `test` / `docs` / `doc-coverage` jobs.
- **`deny.toml`** — no new license categories from the two new deps. `futures-core` is MIT/Apache-2.0; `tokio-util` is MIT.

## 4. Module layout

```
crates/paigasus-helikon-core/
├── Cargo.toml             # workspace inheritance + runtime deps
├── src/
│   ├── lib.rs             # crate docs + flat re-exports
│   ├── model.rs
│   ├── tool.rs
│   ├── session.rs
│   ├── guardrail.rs
│   ├── hook.rs
│   ├── agent.rs
│   ├── runner.rs
│   └── context.rs
└── tests/
    └── object_safety.rs
```

`lib.rs` flattens the namespace:

```rust
//! Trait surface and core types for the Paigasus Helikon AI SDK.
//!
//! See the [`paigasus-helikon` book] for conceptual documentation; this crate's
//! rustdoc is the canonical reference for the trait signatures and carrier types.
//!
//! [`paigasus-helikon` book]: https://smk1085.github.io/paigasus-helikon/

pub mod agent;
pub mod context;
pub mod guardrail;
pub mod hook;
pub mod model;
pub mod runner;
pub mod session;
pub mod tool;

pub use agent::*;
pub use context::*;
pub use guardrail::*;
pub use hook::*;
pub use model::*;
pub use runner::*;
pub use session::*;
pub use tool::*;
```

Each `pub use module::*;` is a one-liner with no doc comment of its own — the items inside the modules carry their own docs, and the workspace `missing_docs = "warn"` lint already enforces that. (Re-exports inherit visibility but not docs, so adding `///` here would only duplicate.)

## 5. Trait signatures

Verbatim from the Notion ADRs (ADR-1 for `Model`, ADR-2 for the dispatch choice, ADR-6 for `Runner`, ADR-11 for `Agent`, the Core Primitives page for the others). Every trait carries a rustdoc example that compiles under `cargo test --doc`.

### 5.1 `Model` (model.rs)

```rust
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use tokio_util::sync::CancellationToken;

/// An LLM provider. The single canonical async interface.
///
/// One trait covers Chat Completions, Responses, Anthropic Messages, Bedrock
/// Converse, and Gemini `FunctionDeclaration`. Capability differences are
/// surfaced via [`ModelCapabilities`], not split traits. See ADR-1.
#[async_trait]
pub trait Model: Send + Sync {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError>;

    fn capabilities(&self) -> ModelCapabilities;
}
```

### 5.2 `Tool<Ctx>` (tool.rs)

```rust
use async_trait::async_trait;

/// A tool an agent can call.
///
/// Object-safe by design — applications hold heterogeneous registries as
/// `Vec<Arc<dyn Tool<Ctx>>>`.
#[async_trait]
pub trait Tool<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> &serde_json::Value;
    fn output_schema(&self) -> Option<&serde_json::Value> { None }

    async fn invoke(
        &self,
        ctx: &ToolContext<Ctx>,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError>;
}
```

### 5.3 `Session` (session.rs)

```rust
use async_trait::async_trait;

/// Conversation persistence as an append-only event log.
///
/// `Session` is not a flat message list — the event log shape gives evals
/// (deterministic replay), durability (Temporal/Restate event sourcing), and
/// audit. See the *Sessions* concept page.
#[async_trait]
pub trait Session: Send + Sync {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError>;
    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError>;
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError>;
}
```

### 5.4 `Guardrail<Ctx>` (guardrail.rs)

```rust
use async_trait::async_trait;

/// Input/output safety check, run in parallel with the agent.
#[async_trait]
pub trait Guardrail<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    async fn check(
        &self,
        ctx: &RunContext<Ctx>,
        input: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError>;
}
```

### 5.5 `Hook<Ctx>` (hook.rs)

```rust
use async_trait::async_trait;

/// Lifecycle interceptor.
#[async_trait]
pub trait Hook<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    async fn on_event(
        &self,
        ctx: &RunContext<Ctx>,
        event: &HookEvent,
    ) -> HookDecision;
}
```

### 5.6 `Agent<Ctx>` (agent.rs)

```rust
use async_trait::async_trait;
use futures_core::stream::BoxStream;

/// One trait for both LLM-driven and workflow agents. See ADR-11.
#[async_trait]
pub trait Agent<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str;
    fn description(&self) -> &str;

    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError>;
}
```

`Agent::run` returns `BoxStream<'static, AgentEvent>` (events, not `Result<AgentEvent, _>`); fatal errors surface as the `AgentEvent::RunFailed { error }` variant once it lands. The outer `Result<_, AgentError>` covers failure to *start* the stream.

### 5.7 `Runner<Ctx>` (runner.rs)

```rust
use async_trait::async_trait;

/// The pluggable execution backend. The durability seam (tokio / Temporal /
/// AgentCore). See ADR-6.
#[async_trait]
pub trait Runner<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    async fn run<A>(
        &self,
        agent: &A,
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResult, RunError>
    where
        A: Agent<Ctx> + ?Sized;

    async fn run_streamed<A>(
        &self,
        agent: &A,
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResultStreaming, RunError>
    where
        A: Agent<Ctx> + ?Sized;
}
```

The `A: Agent<Ctx> + ?Sized` bound on the methods (rather than a `<A: Agent<Ctx>>` parameter on the trait) is what keeps `Runner<Ctx>` itself object-safe. The `?Sized` admits both `&LlmAgent<...>` and `&dyn Agent<Ctx>` at the call site. `async-trait` generates the per-method desugaring that accommodates the per-method generic.

## 6. Carrier-type shapes

Three tiers: **fully shaped** (defined now, no expected change), **named placeholder** (named publicly, fields land later), **stub enum** (one or two variants now, will grow).

### 6.1 Fully shaped

```rust
// model.rs

/// Provider capability flags. See ADR-1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelCapabilities {
    pub streaming: bool,
    pub tools: bool,
    pub parallel_tool_calls: bool,
    pub structured_output: bool,
    pub server_managed_state: bool,
    pub reasoning: bool,
    pub vision: bool,
    pub audio: bool,
}

/// Why a model stopped emitting tokens for a single response.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other(String),
}

/// The streaming union — token, reasoning, tool-call delta, finish.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ModelEvent {
    TokenDelta { text: String },
    ReasoningDelta { text: String },
    ToolCallDelta { call_id: String, name: Option<String>, args_delta: String },
    Finish { reason: FinishReason },
}

// guardrail.rs

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum GuardrailVerdict {
    Pass,
    Tripwire { kind: GuardrailKind, info: serde_json::Value },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuardrailKind {
    InputPolicy,
    OutputPolicy,
    Other(String),
}

// hook.rs

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum HookDecision {
    Allow,
    Deny { reason: String },
    ReplaceInput { value: serde_json::Value },
    ReplaceOutput { value: serde_json::Value },
    InjectSystemMessage { text: String },
}

// session.rs

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
    serde::Serialize, serde::Deserialize,
)]
pub struct SequenceId(pub u64);
```

### 6.2 Named placeholder (named publicly, field shape lands later)

Each is `#[non_exhaustive]` (or `#[non_exhaustive]` on a unit struct), constructable by users via the constructors documented on each, and has a `// SMA-3xx — field shape lands with <ticket>` doc note so future-me knows where to look:

```rust
// model.rs

/// The request envelope crossing the model boundary.
///
/// Field shape (messages, tools, tool_choice, response_format, temperature,
/// previous_response_id, ...) lands with the provider tickets that exercise
/// it. Today the type exists so trait signatures resolve and rustdoc
/// examples compile.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ModelRequest {}

impl ModelRequest {
    pub fn new() -> Self { Self::default() }
}

// tool.rs

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ToolOutput {
    pub content: serde_json::Value,
}

pub struct ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    _ctx: std::marker::PhantomData<fn() -> Ctx>,
}

impl<Ctx> ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct a bare `ToolContext`. Field shape lands with the agent-loop
    /// ticket.
    pub fn new() -> Self { Self { _ctx: std::marker::PhantomData } }
}

// context.rs

/// Carries user context, session handle, hook registry, tracer, cancellation
/// token. Field shape lands with the agent-loop ticket.
pub struct RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    _ctx: std::marker::PhantomData<fn() -> Ctx>,
}

impl<Ctx> RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    pub fn new() -> Self { Self { _ctx: std::marker::PhantomData } }
}

pub use tokio_util::sync::CancellationToken;

// agent.rs

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AgentInput {}

// runner.rs

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RunConfig {}

#[derive(Debug, Default)]
#[non_exhaustive]
pub struct RunResult {}

#[non_exhaustive]
pub struct RunResultStreaming {}

// session.rs

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ConversationSnapshot {}
```

The `PhantomData<fn() -> Ctx>` pattern keeps `RunContext<Ctx>` and `ToolContext<Ctx>` covariant in `Ctx` *without* requiring `Ctx: Send + Sync` to be transitive across the phantom — i.e. the context type itself is `Send + Sync` regardless of `Ctx`. (Which it has to be, because `Hook<Ctx>::on_event` and `Guardrail<Ctx>::check` take `&RunContext<Ctx>` and are themselves `Send + Sync`.)

### 6.3 Stub enums

```rust
// session.rs — variants from the Sessions concept page; ts/content shapes
// stay placeholder until the wire-format ticket.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionEvent {
    UserMessage { text: String },
    AssistantMessage { text: String, agent: String },
    ToolCalled { call_id: String, name: String, args: serde_json::Value },
    ToolReturned { call_id: String, output: serde_json::Value },
    HandoffOccurred { from: String, to: String },
    Compacted { summary: String, original_count: usize },
}

// agent.rs — variant set from the Agent Loop concept page, trimmed to what
// the trait surface needs. The full 14-variant ADT lands with the agent-loop
// ticket.

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AgentEvent {
    RunStarted { agent: String },
    TokenDelta { text: String },
    RunCompleted,
    RunFailed { error: String },
}

// hook.rs

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum HookEvent {
    OnRunStart,
    OnTurnStart { turn: u32 },
    PreToolUse { tool: String, args: serde_json::Value },
    PostToolUse { tool: String, output: serde_json::Value },
    OnHandoff { from: String, to: String },
    OnRunComplete,
}

// guardrail.rs

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum GuardrailInput<'a> {
    UserText(&'a str),
    ModelOutput(&'a str),
}
```

## 7. Error enums

Six `thiserror` enums, all `#[non_exhaustive]`, all with `Other(anyhow::Error)`. The shapes below are minimum-viable; variants will grow as concrete impls land.

```rust
// model.rs

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModelError {
    #[error("model provider unavailable")]
    Unavailable,

    #[error("rate limited (retry after {retry_after_ms:?} ms)")]
    RateLimited { retry_after_ms: Option<u64> },

    #[error("context length exceeded")]
    ContextLengthExceeded,

    #[error("model refused: {reason}")]
    Refused { reason: String },

    #[error("transport error: {0}")]
    Transport(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// tool.rs

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    /// Recoverable — the runner is permitted to feed `schema_errors` back to
    /// the model once before surfacing `AgentError::InvalidStructuredOutput`.
    /// See ADR-10.
    #[error("invalid tool arguments: {schema_errors:?}")]
    InvalidArgs { schema_errors: Vec<String> },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// session.rs

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    #[error("session backend unavailable")]
    Unavailable,

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// guardrail.rs

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GuardrailError {
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// agent.rs

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentError {
    #[error("model failed: {0}")]
    Model(#[from] ModelError),

    #[error("tool failed: {0}")]
    Tool(#[from] ToolError),

    #[error("session failed: {0}")]
    Session(#[from] SessionError),

    #[error("guardrail tripped: {kind:?}")]
    Guardrail { kind: GuardrailKind },

    #[error("invalid structured output after one repair attempt")]
    InvalidStructuredOutput,

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// runner.rs

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    #[error("agent failed: {0}")]
    Agent(#[from] AgentError),

    #[error("max iterations reached")]
    MaxIterations,

    #[error("cancelled")]
    Cancelled,

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

`AgentError::Guardrail` does not embed the full `GuardrailError` because a *tripwire is not a guardrail failure* — it's a guardrail success that the agent must halt for. `GuardrailError` is reserved for the guardrail itself crashing.

Per ADR-10, no auto-retry: `RateLimited { retry_after_ms }` is informational; the runner does not sleep-and-retry on it. Retries are configured at the application layer via `RunConfig::retry_policy` once that lands.

## 8. Workspace dependency adds

Two new pins in `[workspace.dependencies]` at the repo root, both MIT/Apache-2.0:

```toml
futures-core = "0.3"
tokio-util   = { version = "0.7", default-features = false, features = ["rt"] }
```

`futures-core` (not `futures`) is the minimal crate that exports `BoxStream` — `futures` itself would pull in executors, channels, and combinators we don't need at the core layer.

`tokio-util` with `default-features = false, features = ["rt"]` gates the dependency to just the `CancellationToken` family. This does **not** pull in a tokio runtime — it pulls in a few `Arc`/`AtomicBool` types that work on any executor. (The `rt` feature name in `tokio-util` enables runtime-aware combinators; the type itself is in `tokio-util::sync` and is always available. Pinning the feature explicitly is belt-and-suspenders.)

Both crates' MSRV is comfortably below 1.75. `cargo msrv` will confirm this on the per-PR `msrv.yml` job.

`paigasus-helikon-core/Cargo.toml` then references them as `dep.workspace = true`:

```toml
[dependencies]
async-trait  = { workspace = true }
thiserror    = { workspace = true }
anyhow       = { workspace = true }
serde        = { workspace = true }
serde_json   = { workspace = true }
futures-core = { workspace = true }
tokio-util   = { workspace = true }
```

## 9. Testing strategy

The crate has no behavior to test — only API shape. The verification commands below are the local proof of the AC; CI re-runs each on every PR via the existing `ci.yml` matrix.

| AC | Verification |
|---|---|
| **All seven traits compile** | `cargo build -p paigasus-helikon-core` exits 0. `cargo build -p paigasus-helikon` (facade) also exits 0, confirming the re-export still works without a facade edit. |
| **All seven traits have rustdoc examples** | `cargo test --doc -p paigasus-helikon-core` exits 0. Every `pub trait` has a `# Example` block constructing a trivial impl and exercising one method. |
| **Trait objects construct without GAT/AFIT issues** | `cargo test --test object_safety -p paigasus-helikon-core` exits 0. The integration test in `tests/object_safety.rs` (sketch in §9.1) constructs `Box<dyn Model>`, `Vec<Arc<dyn Tool<()>>>`, `Box<dyn Session>`, `Box<dyn Guardrail<()>>`, `Box<dyn Hook<()>>`, `Box<dyn Agent<()>>`, `Box<dyn Runner<()>>`. A compile failure here is a CI-blocking AC regression. |
| **No silent retries** | Code review check. No `.retry(...)` combinator, no loop that re-invokes a `Model`/`Tool`/`Runner` method on its own. `ToolError::InvalidArgs` has a docstring naming the one-shot recovery path; no other variant claims recoverability. Captured in §7. |
| **Workspace lints clean** | `cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings` exits 0. Workspace `missing_docs = "warn"` is honored by every public item. |
| **Rustdoc clean** | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p paigasus-helikon-core` exits 0. |
| **Doc coverage ≥ 80%** | `DOC_COVERAGE_THRESHOLD=80 bash scripts/check-doc-coverage.sh` exits 0. The crate is already opted in via the workspace's `[lints] workspace = true` block. |
| **MSRV holds** | `cargo msrv --path crates/paigasus-helikon-core verify` exits 0. New deps (`futures-core`, `tokio-util`) have MSRV well below 1.75. |

### 9.1 `tests/object_safety.rs` — sketch

```rust
//! Locks acceptance criterion #2: every object-safe trait can be held behind
//! `Box<dyn _>` and `Vec<Arc<dyn _>>`. A compile failure here is an AC
//! regression.

use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentError, AgentInput, CancellationToken, Guardrail,
    GuardrailError, GuardrailInput, GuardrailVerdict, Hook, HookDecision,
    HookEvent, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
    RunConfig, RunContext, RunError, RunResult, RunResultStreaming, Runner,
    SequenceId, Session, SessionError, SessionEvent, ConversationSnapshot,
    Tool, ToolContext, ToolError, ToolOutput,
};

struct NoopModel;
#[async_trait]
impl Model for NoopModel {
    async fn invoke(
        &self,
        _: ModelRequest,
        _: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        unimplemented!()
    }
    fn capabilities(&self) -> ModelCapabilities { ModelCapabilities::default() }
}

// ...analogous trivial impls for Tool, Session, Guardrail, Hook, Agent,
// Runner...

#[test]
fn trait_objects_construct() {
    let _: Box<dyn Model> = Box::new(NoopModel);
    let _: Vec<Arc<dyn Tool<()>>> = vec![/* trivial Tool impl */];
    let _: Box<dyn Session> = Box::new(/* trivial Session impl */);
    let _: Box<dyn Guardrail<()>> = Box::new(/* ... */);
    let _: Box<dyn Hook<()>> = Box::new(/* ... */);
    let _: Box<dyn Agent<()>> = Box::new(/* ... */);
    let _: Box<dyn Runner<()>> = Box::new(/* ... */);
}
```

The test body is mostly type ascriptions; the actual verification is that the file *compiles*. If any trait accidentally becomes non-object-safe (e.g. by acquiring an `async fn` without `async-trait`, or a generic method on the trait itself), the file fails to compile and CI fails.

## 10. Out of scope (deferred to follow-ups)

Each item below is named because the SMA-312 ticket text or the Notion ADRs reference it; we are deliberately not landing it here.

- **`LoopState` and the runner's implementation of the agent loop** — separate ticket. The seven traits plus a stub `RunResult` are enough surface for the runner crate to begin work in parallel.
- **`PermissionMode` / `PermissionPolicy` / `PermissionDecision`** — explicitly not in the SMA-312 trait list. They live under "Permissions" not the "seven core traits". Tracked separately.
- **Canonical `Item` wire format** — the message superset of OpenAI / Anthropic / Bedrock content types. Lands with provider tickets; `SessionEvent` and `ModelRequest` placeholder-shapes today use `String` and `serde_json::Value` where they will later use `Item`.
- **`Instructions<Ctx>`, the typestate builder, `LlmAgent<Ctx, M>`** — concrete agent. Separate ticket once the loop ticket lands.
- **Default backends** — `MemorySession`, `SqliteSession`, `OpenAiModel`, `AnthropicModel`, etc. Each lives in its own crate; SMA-312 only defines the traits they will implement.
- **The `#[tool]` macro** — `paigasus-helikon-macros` is a proc-macro crate from day one (per CLAUDE.md), but its first macro lands in a separate ticket.
- **`paigasus::schema::strict()` helper** — the per-provider JSON Schema rewriter for OpenAI strict mode / Bedrock quirks. Open question per the Notion *Open Questions & Caveats* page.
- **`RunConfig::retry_policy` shape** — the surface where application-level retry lives (per ADR-10). The field is named in this design but its sub-shape is deferred.

## 11. Commit shape

Single PR on `feature/sma-312-define-core-trait-surface-model-tool-agent-session-guardrail`. Commit type for the implementation commit(s): `feat(core): SMA-312 …`. This is the workspace's first net-new `feat` post-bootstrap; release-plz will pick it up and bump `paigasus-helikon-core` to its next pre-1.0 version on merge.

The local commit-msg hook's scope allowlist already permits `core`. The PR title gate (`pr-title.yml`) will validate the squashed-merge title against the same allowlist.

This spec document lands on the same feature branch (not pre-merged to `main`) as `docs(spec): SMA-312 add design for core trait surface`.

## 12. Acceptance criteria (verification plan)

Before requesting review on the implementation PR, every command in §9 exits 0 locally, matching the CI matrix job-for-job:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
cargo msrv --path crates/paigasus-helikon-core verify
```

The `tests/object_safety.rs` integration test is the explicit lock for AC #2 ("trait objects can be constructed without GAT/AFIT issues"). The "no silent retries" AC #3 is enforced by code review against the rules in §7.

## 13. Risks and notes

- **Carrier placeholder churn.** The named-placeholder types (`ModelRequest`, `AgentInput`, `RunConfig`, etc.) will gain fields in subsequent tickets, each of which is technically a SemVer-breaking change pre-1.0 but is invisible to users who construct via `::default()` or `::new()`. The `#[non_exhaustive]` attribute prevents struct-literal construction outside the crate, so this churn cannot break downstream code unexpectedly.
- **`Runner<Ctx>` object-safety**. The per-method `A: Agent<Ctx> + ?Sized` bound is the only non-obvious shape in the trait set. If a future Rust release tightens object-safety rules around per-method generics with `async-trait`, the `tests/object_safety.rs` test catches it on the first CI run.
- **`tokio-util` dependency at core**. We re-export `CancellationToken` so users don't need to depend on `tokio-util` directly. If a non-tokio executor ever becomes the dominant target, swapping the canonical cancellation type is a major-version event for the core crate — but until that day, `tokio-util` is the path of least surprise.
- **No `dyn Agent<Ctx>` in `Runner::run`'s signature**. The bound is `A: Agent<Ctx> + ?Sized`, which admits `&dyn Agent<Ctx>` but doesn't *require* the indirection at every call site. This matches the Notion ADR-6 example (which uses a generic `A: Agent<Ctx>`) while keeping the `Runner` trait itself object-safe.
- **`dyn*` / native dyn-safe AFIT is a future reversal trigger** (ADR-2). When it stabilizes, we drop `async-trait` from the object-safe traits. Tracked on the *Open Questions & Caveats* Notion page; not in scope for this ticket.
