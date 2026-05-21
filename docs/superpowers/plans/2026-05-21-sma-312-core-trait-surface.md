# SMA-312 — Core trait surface implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the seven canonical object-safe traits (`Model`, `Tool<Ctx>`, `Agent<Ctx>`, `Session`, `Guardrail<Ctx>`, `Hook<Ctx>`, `Runner<Ctx>`) of `paigasus-helikon-core`, with the minimum named carrier types and `thiserror` error enums referenced by the trait signatures.

**Architecture:** Public API surface only — no behavior. Each domain lives in its own module (`model.rs`, `tool.rs`, …) and is re-exported flat from `lib.rs`. Every object-safe trait uses `#[async_trait::async_trait]` (per ADR-2). Streaming uses `futures_core::stream::BoxStream<'static, _>`; cancellation uses `tokio_util::sync::CancellationToken`. Errors are six `thiserror` enums, all `#[non_exhaustive]`, all with an `Other(anyhow::Error)` escape hatch (per ADR-10).

**Tech Stack:** Rust 1.75 (MSRV), `async-trait` 0.1, `futures-core` 0.3, `tokio-util` 0.7 (default-features off, `rt` feature), `thiserror` 2, `anyhow` 1, `serde` 1, `serde_json` 1.

**Spec:** [`docs/superpowers/specs/2026-05-21-sma-312-core-trait-surface-design.md`](../specs/2026-05-21-sma-312-core-trait-surface-design.md).

**Branch:** `feature/sma-312-define-core-trait-surface-model-tool-agent-session-guardrail` (already created and checked out — the design commit lives here).

---

## Pre-flight

Verify you are on the feature branch and starting from the design commit:

```bash
git status
# Expected: On branch feature/sma-312-define-core-trait-surface-model-tool-agent-session-guardrail
#           nothing to commit, working tree clean

git log --oneline -1
# Expected: e451f21 docs(spec): SMA-312 add design for core trait surface
```

The `paigasus-helikon-core` crate currently exists as a stub from SMA-304. Its `src/lib.rs` is a two-line module docstring and its `Cargo.toml` has no runtime dependencies. Implementation work replaces both.

After each task's commit, run the matching CI gate locally (`cargo fmt --all -- --check`, `cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings`, `cargo test --workspace --all-features`, `cargo doc --workspace --all-features --no-deps` with `RUSTDOCFLAGS="-D warnings"`). The full gate runs again in Task 12.

---

## Task 1: Add new workspace dependencies (`futures-core`, `tokio-util`)

**Files:**
- Modify: `Cargo.toml` (workspace root, `[workspace.dependencies]`)
- Modify: `crates/paigasus-helikon-core/Cargo.toml`

- [ ] **Step 1: Add the two new pins to the workspace `[workspace.dependencies]` block**

Edit `Cargo.toml` at the repo root. Add these two lines to the third-party section of `[workspace.dependencies]`, immediately after the existing `async-trait = "0.1"` line (keeping the section alphabetically grouped is not enforced by the file today — the current order is "use-frequency-ish"; append at the end of the third-party block is fine):

```toml
futures-core = "0.3"
tokio-util   = { version = "0.7", default-features = false, features = ["rt"] }
```

The full third-party block after this edit looks like:

```toml
[workspace.dependencies]
serde         = { version = "1", features = ["derive"] }
serde_json    = "1"
schemars      = "1"
tokio         = { version = "1", features = ["full"] }
tracing       = "0.1"
opentelemetry = "0.27"
rmcp          = "0.16"
thiserror     = "2"
anyhow        = "1"
async-trait   = "0.1"
futures-core  = "0.3"
tokio-util    = { version = "0.7", default-features = false, features = ["rt"] }
```

- [ ] **Step 2: Add the seven runtime deps to `paigasus-helikon-core/Cargo.toml`**

Replace `crates/paigasus-helikon-core/Cargo.toml` so it inherits everything from workspace and declares the seven runtime deps:

```toml
[package]
name        = "paigasus-helikon-core"
description = "Trait surface and concrete types for the Paigasus Helikon AI SDK."
version                = "0.0.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true

[dependencies]
async-trait  = { workspace = true }
thiserror    = { workspace = true }
anyhow       = { workspace = true }
serde        = { workspace = true }
serde_json   = { workspace = true }
futures-core = { workspace = true }
tokio-util   = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 3: Verify workspace resolves and the empty crate still compiles**

```bash
cargo fmt --all -- --check
cargo build -p paigasus-helikon-core
```

Expected: `cargo fmt` exits 0. `cargo build` exits 0 (the existing stub `lib.rs` doesn't yet reference the new deps; it just compiles to an empty rlib).

- [ ] **Step 4: Verify MSRV holds with the new deps**

```bash
cargo msrv --path crates/paigasus-helikon-core verify
```

Expected: exits 0 (`futures-core 0.3` and `tokio-util 0.7` are MSRV-compatible with 1.75; if this fails, do **not** downgrade — bump `rust-version` in the root `Cargo.toml`'s `[workspace.package]` to what cargo demands, per CLAUDE.md).

If you don't have `cargo-msrv` installed locally, skip this step — the `msrv.yml` CI job re-verifies it on PR.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/paigasus-helikon-core/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(core): SMA-312 add futures-core and tokio-util workspace deps

futures-core (for BoxStream) and tokio-util (rt-only, for
CancellationToken) are the two new pins needed by the upcoming
paigasus-helikon-core trait surface. Wire them through the crate's
[dependencies] alongside the existing async-trait, thiserror, anyhow,
serde, and serde_json.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Crate skeleton (`lib.rs` + `context.rs`)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/lib.rs`
- Create: `crates/paigasus-helikon-core/src/context.rs`

`context.rs` lands first because every other module's trait method takes `&RunContext<Ctx>` or returns/uses types that touch it. `lib.rs` is rewritten now with the full module list but each `pub mod foo;` declaration is added incrementally as the module file appears — adding all eight at once would cause `cannot find module` errors. We start with the two we need.

- [ ] **Step 1: Replace `crates/paigasus-helikon-core/src/lib.rs`**

```rust
//! Trait surface and core types for the Paigasus Helikon AI SDK.
//!
//! This crate is the dependency root of the workspace; the facade crate
//! [`paigasus-helikon`] re-exports its surface unconditionally.
//!
//! The seven object-safe traits ([`Model`], [`Tool`], [`Session`],
//! [`Guardrail`], [`Hook`], [`Agent`], [`Runner`]) and their carrier
//! types form the contract every other Paigasus Helikon crate depends on.
//!
//! See the [project documentation site] for conceptual material; this
//! crate's rustdoc is the canonical reference for the trait signatures and
//! carrier types.
//!
//! [`paigasus-helikon`]: https://docs.rs/paigasus-helikon
//! [project documentation site]: https://smk1085.github.io/paigasus-helikon/

pub mod context;

pub use context::*;
```

- [ ] **Step 2: Create `crates/paigasus-helikon-core/src/context.rs`**

```rust
//! Run-scoped context types.
//!
//! [`RunContext`] carries user data, session handle, hook registry, tracer,
//! and cancellation token across the agent loop. The field shape lands with
//! the agent-loop ticket — today the type exists so the trait signatures
//! that reference it resolve.

use std::marker::PhantomData;

/// Carries user context, session handle, hook registry, tracer, and
/// cancellation token across one run of the agent loop.
///
/// Field shape lands with the agent-loop ticket.
///
/// # Example
///
/// ```
/// use paigasus_helikon_core::RunContext;
///
/// let _ctx: RunContext<()> = RunContext::new();
/// ```
pub struct RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    _ctx: PhantomData<fn() -> Ctx>,
}

impl<Ctx> RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct a bare [`RunContext`].
    ///
    /// The constructor signature will grow alongside the type's fields in
    /// the agent-loop ticket. Code that needs an empty context today should
    /// use this method rather than relying on struct-literal syntax (the
    /// type is `#[non_exhaustive]`-equivalent because all fields are
    /// private).
    pub fn new() -> Self {
        Self { _ctx: PhantomData }
    }
}

impl<Ctx> Default for RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Re-export of [`tokio_util::sync::CancellationToken`] so downstream
/// crates need not depend on `tokio-util` directly.
pub use tokio_util::sync::CancellationToken;
```

- [ ] **Step 3: Verify build, doc-tests, and clippy**

```bash
cargo fmt --all
cargo build -p paigasus-helikon-core
cargo test --doc -p paigasus-helikon-core
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p paigasus-helikon-core
```

Expected: every command exits 0. The doc-test in `RunContext::new` compiles and runs.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/
git commit -m "$(cat <<'EOF'
feat(core): SMA-312 add RunContext and CancellationToken re-export

Lays down the lib.rs crate-level docs and the first module (context),
which every later trait references. RunContext today is a phantom-typed
placeholder; its fields land with the agent-loop ticket.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: `Model` trait + supporting types + `ModelError`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/lib.rs` (add `pub mod model;` + `pub use model::*;`)
- Create: `crates/paigasus-helikon-core/src/model.rs`

- [ ] **Step 1: Add the module to `lib.rs`**

Edit `crates/paigasus-helikon-core/src/lib.rs`. Add `pub mod model;` and `pub use model::*;` so the file becomes:

```rust
//! Trait surface and core types for the Paigasus Helikon AI SDK.
//!
//! This crate is the dependency root of the workspace; the facade crate
//! [`paigasus-helikon`] re-exports its surface unconditionally.
//!
//! The seven object-safe traits ([`Model`], [`Tool`], [`Session`],
//! [`Guardrail`], [`Hook`], [`Agent`], [`Runner`]) and their carrier
//! types form the contract every other Paigasus Helikon crate depends on.
//!
//! See the [project documentation site] for conceptual material; this
//! crate's rustdoc is the canonical reference for the trait signatures and
//! carrier types.
//!
//! [`paigasus-helikon`]: https://docs.rs/paigasus-helikon
//! [project documentation site]: https://smk1085.github.io/paigasus-helikon/

pub mod context;
pub mod model;

pub use context::*;
pub use model::*;
```

- [ ] **Step 2: Create `crates/paigasus-helikon-core/src/model.rs`**

```rust
//! The [`Model`] trait — the single canonical async interface to an LLM
//! provider — and its carrier types.
//!
//! One trait covers OpenAI Chat Completions, OpenAI Responses, Anthropic
//! Messages, Bedrock Converse, and Gemini `FunctionDeclaration`. Capability
//! differences are surfaced via [`ModelCapabilities`], not split traits.

use async_trait::async_trait;
use futures_core::stream::BoxStream;

use crate::CancellationToken;

/// An LLM provider. The single canonical async interface.
///
/// One trait covers Chat Completions, Responses, Anthropic Messages,
/// Bedrock Converse, and Gemini `FunctionDeclaration`. Capability
/// differences are surfaced via [`ModelCapabilities`], not split traits.
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use futures_core::stream::BoxStream;
/// use paigasus_helikon_core::{
///     CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent,
///     ModelRequest,
/// };
///
/// struct NoopModel;
///
/// #[async_trait]
/// impl Model for NoopModel {
///     async fn invoke(
///         &self,
///         _request: ModelRequest,
///         _cancel: CancellationToken,
///     ) -> Result<
///         BoxStream<'static, Result<ModelEvent, ModelError>>,
///         ModelError,
///     > {
///         Err(ModelError::Unavailable)
///     }
///
///     fn capabilities(&self) -> ModelCapabilities {
///         ModelCapabilities::default()
///     }
/// }
/// ```
#[async_trait]
pub trait Model: Send + Sync {
    /// Invoke the model. Returns a stream of [`ModelEvent`]s on success or a
    /// [`ModelError`] if the request could not be sent. Individual events in
    /// the stream may themselves carry a [`ModelError`].
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError>;

    /// Provider capabilities. Stable across calls.
    fn capabilities(&self) -> ModelCapabilities;
}

/// The request envelope crossing the model boundary.
///
/// Field shape (messages, tools, `tool_choice`, `response_format`,
/// temperature, `previous_response_id`, …) lands with the provider tickets
/// that exercise it. Today the type exists so trait signatures resolve and
/// rustdoc examples compile.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ModelRequest {}

impl ModelRequest {
    /// Construct an empty [`ModelRequest`].
    pub fn new() -> Self {
        Self::default()
    }
}

/// Streaming union — token, reasoning, tool-call delta, finish.
///
/// See ADR-1 (*Single Model trait with capabilities flags*).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ModelEvent {
    /// A chunk of assistant text.
    TokenDelta {
        /// The text fragment.
        text: String,
    },
    /// A chunk of reasoning/scratchpad text (for providers that emit it
    /// separately from the assistant text channel).
    ReasoningDelta {
        /// The text fragment.
        text: String,
    },
    /// A partial tool call. `name` is `Some` on the first delta for a given
    /// `call_id`, then `None` on subsequent deltas as `args_delta` chunks
    /// arrive.
    ToolCallDelta {
        /// Provider-assigned identifier for the call.
        call_id: String,
        /// Tool name; `Some` on the first delta only.
        name: Option<String>,
        /// JSON-encoded argument fragment.
        args_delta: String,
    },
    /// Terminal event for a single response.
    Finish {
        /// Why the response ended.
        reason: FinishReason,
    },
}

/// Why a single model response stopped emitting tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FinishReason {
    /// Natural stop.
    Stop,
    /// Hit the model's max-output-tokens limit.
    Length,
    /// Model emitted tool calls and is awaiting their results.
    ToolCalls,
    /// Provider's content filter rejected the response.
    ContentFilter,
    /// Provider-specific stop reason that does not map to a known variant.
    Other(String),
}

/// Provider capability flags. See ADR-1.
///
/// Capability flags inform the agent loop's behavior (e.g. whether to use
/// JSON-mode structured output, whether to expect parallel tool calls).
/// They are stable per [`Model`] instance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelCapabilities {
    /// Provider streams tokens.
    pub streaming: bool,
    /// Provider supports tool/function calling.
    pub tools: bool,
    /// Provider can emit multiple tool calls in a single response.
    pub parallel_tool_calls: bool,
    /// Provider supports schema-constrained structured output.
    pub structured_output: bool,
    /// Provider holds conversation state server-side (e.g. OpenAI
    /// Responses' `previous_response_id`).
    pub server_managed_state: bool,
    /// Provider emits reasoning tokens distinct from the main channel.
    pub reasoning: bool,
    /// Provider accepts image inputs.
    pub vision: bool,
    /// Provider accepts audio inputs.
    pub audio: bool,
}

/// Errors raised by [`Model::invoke`] or surfaced through the
/// [`ModelEvent`] stream.
///
/// Per ADR-10 (*No silent auto-retry inside the loop*), the runner never
/// retries on these — retries are an application-layer concern configured
/// via `RunConfig::retry_policy` (lands with the runner ticket).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModelError {
    /// Provider returned a no-route / 503 / connection-refused style error.
    #[error("model provider unavailable")]
    Unavailable,

    /// Provider rate-limited the request. `retry_after_ms` carries the
    /// provider's hint when one is supplied (e.g. via `Retry-After`).
    #[error("rate limited (retry after {retry_after_ms:?} ms)")]
    RateLimited {
        /// Provider-supplied retry hint in milliseconds.
        retry_after_ms: Option<u64>,
    },

    /// Request exceeded the provider's context-length limit.
    #[error("context length exceeded")]
    ContextLengthExceeded,

    /// Provider refused the request (content policy, account state, …).
    #[error("model refused: {reason}")]
    Refused {
        /// Human-readable reason supplied by the provider.
        reason: String,
    },

    /// Transport-level failure (DNS, TLS, socket reset). The string is
    /// provider-formatted.
    #[error("transport error: {0}")]
    Transport(String),

    /// Escape hatch for arbitrary upstream failures. See ADR-10.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

- [ ] **Step 3: Verify build, doc-tests, and clippy**

```bash
cargo fmt --all
cargo build -p paigasus-helikon-core
cargo test --doc -p paigasus-helikon-core
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p paigasus-helikon-core
```

Expected: every command exits 0. The `Model` rustdoc example compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon-core/src/model.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-312 add Model trait and carrier types

Adds the Model trait (async-trait, object-safe per ADR-2),
ModelRequest, ModelEvent, ModelCapabilities, FinishReason, and ModelError.
ModelRequest is a named placeholder per the design — field shape lands
with the provider tickets that exercise it.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `Tool<Ctx>` trait + supporting types + `ToolError`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/lib.rs`
- Create: `crates/paigasus-helikon-core/src/tool.rs`

- [ ] **Step 1: Add the module to `lib.rs`**

Edit `crates/paigasus-helikon-core/src/lib.rs`. The full file after this edit:

```rust
//! Trait surface and core types for the Paigasus Helikon AI SDK.
//!
//! This crate is the dependency root of the workspace; the facade crate
//! [`paigasus-helikon`] re-exports its surface unconditionally.
//!
//! The seven object-safe traits ([`Model`], [`Tool`], [`Session`],
//! [`Guardrail`], [`Hook`], [`Agent`], [`Runner`]) and their carrier
//! types form the contract every other Paigasus Helikon crate depends on.
//!
//! See the [project documentation site] for conceptual material; this
//! crate's rustdoc is the canonical reference for the trait signatures and
//! carrier types.
//!
//! [`paigasus-helikon`]: https://docs.rs/paigasus-helikon
//! [project documentation site]: https://smk1085.github.io/paigasus-helikon/

pub mod context;
pub mod model;
pub mod tool;

pub use context::*;
pub use model::*;
pub use tool::*;
```

- [ ] **Step 2: Create `crates/paigasus-helikon-core/src/tool.rs`**

```rust
//! The [`Tool`] trait and its carrier types.
//!
//! Tools are object-safe by design — applications hold heterogeneous
//! registries as `Vec<Arc<dyn Tool<Ctx>>>`.

use std::marker::PhantomData;

use async_trait::async_trait;

/// A tool an agent can call.
///
/// Object-safe by design — applications hold heterogeneous registries as
/// `Vec<Arc<dyn Tool<Ctx>>>`.
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use paigasus_helikon_core::{Tool, ToolContext, ToolError, ToolOutput};
/// use serde_json::{json, Value};
///
/// struct EchoTool {
///     schema: Value,
/// }
///
/// #[async_trait]
/// impl Tool<()> for EchoTool {
///     fn name(&self) -> &str { "echo" }
///     fn description(&self) -> &str { "Returns the input verbatim." }
///     fn schema(&self) -> &Value { &self.schema }
///
///     async fn invoke(
///         &self,
///         _ctx: &ToolContext<()>,
///         args: Value,
///     ) -> Result<ToolOutput, ToolError> {
///         Ok(ToolOutput { content: args })
///     }
/// }
///
/// let _tool = EchoTool {
///     schema: json!({ "type": "object" }),
/// };
/// ```
#[async_trait]
pub trait Tool<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Tool name, unique per registry. Used by the model to address calls.
    fn name(&self) -> &str;
    /// Human-readable description, shown to the model.
    fn description(&self) -> &str;
    /// JSON Schema for the argument payload.
    fn schema(&self) -> &serde_json::Value;
    /// Optional JSON Schema for the return payload. Default is `None`.
    fn output_schema(&self) -> Option<&serde_json::Value> {
        None
    }

    /// Execute the tool with `args` (a JSON value matching [`Tool::schema`]).
    async fn invoke(
        &self,
        ctx: &ToolContext<Ctx>,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError>;
}

/// A narrower view of [`crate::RunContext`] passed to [`Tool::invoke`].
///
/// Field shape lands with the agent-loop ticket.
pub struct ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    _ctx: PhantomData<fn() -> Ctx>,
}

impl<Ctx> ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct a bare [`ToolContext`].
    pub fn new() -> Self {
        Self { _ctx: PhantomData }
    }
}

impl<Ctx> Default for ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// The result of a successful [`Tool::invoke`] call.
///
/// Field shape (multi-modal content, metadata) lands with later tickets.
/// Today `content` is the raw JSON value the tool returned.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ToolOutput {
    /// The tool's return payload, as JSON.
    pub content: serde_json::Value,
}

/// Errors raised by [`Tool::invoke`].
///
/// `InvalidArgs` is the single recoverable variant per ADR-10: the runner
/// is permitted to feed the schema errors back to the model once before
/// surfacing [`crate::AgentError::InvalidStructuredOutput`]. No other
/// variant is recoverable.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    /// Arguments did not match [`Tool::schema`].
    ///
    /// Recoverable per ADR-10 — the runner may feed `schema_errors` back to
    /// the model once before surfacing
    /// [`crate::AgentError::InvalidStructuredOutput`].
    #[error("invalid tool arguments: {schema_errors:?}")]
    InvalidArgs {
        /// Human-readable schema-validation errors.
        schema_errors: Vec<String>,
    },

    /// Escape hatch for arbitrary tool failures. See ADR-10.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

- [ ] **Step 3: Verify**

```bash
cargo fmt --all
cargo build -p paigasus-helikon-core
cargo test --doc -p paigasus-helikon-core
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p paigasus-helikon-core
```

Expected: every command exits 0.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon-core/src/tool.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-312 add Tool trait and carrier types

Adds Tool<Ctx>, ToolContext<Ctx>, ToolOutput, and ToolError. ToolError's
InvalidArgs is the only recoverable error variant in the surface, per
ADR-10's one-shot structured-output repair path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `Session` trait + supporting types + `SessionError`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/lib.rs`
- Create: `crates/paigasus-helikon-core/src/session.rs`

- [ ] **Step 1: Add the module to `lib.rs`**

Add `pub mod session;` and `pub use session::*;` to `lib.rs`. The full file after this edit:

```rust
//! Trait surface and core types for the Paigasus Helikon AI SDK.
//!
//! This crate is the dependency root of the workspace; the facade crate
//! [`paigasus-helikon`] re-exports its surface unconditionally.
//!
//! The seven object-safe traits ([`Model`], [`Tool`], [`Session`],
//! [`Guardrail`], [`Hook`], [`Agent`], [`Runner`]) and their carrier
//! types form the contract every other Paigasus Helikon crate depends on.
//!
//! See the [project documentation site] for conceptual material; this
//! crate's rustdoc is the canonical reference for the trait signatures and
//! carrier types.
//!
//! [`paigasus-helikon`]: https://docs.rs/paigasus-helikon
//! [project documentation site]: https://smk1085.github.io/paigasus-helikon/

pub mod context;
pub mod model;
pub mod session;
pub mod tool;

pub use context::*;
pub use model::*;
pub use session::*;
pub use tool::*;
```

- [ ] **Step 2: Create `crates/paigasus-helikon-core/src/session.rs`**

```rust
//! The [`Session`] trait and its carrier types.
//!
//! `Session` models conversation persistence as an **append-only event
//! log**, not a flat message list. The event-log shape gives evals
//! (deterministic replay), durability (Temporal/Restate-style event
//! sourcing), and an audit trail for regulated deployments.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Conversation persistence as an append-only event log.
///
/// See the *Sessions* concept page for the rationale.
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use paigasus_helikon_core::{
///     ConversationSnapshot, SequenceId, Session, SessionError, SessionEvent,
/// };
///
/// struct MemorySession;
///
/// #[async_trait]
/// impl Session for MemorySession {
///     async fn append(
///         &self,
///         _events: &[SessionEvent],
///     ) -> Result<(), SessionError> {
///         Ok(())
///     }
///
///     async fn events(
///         &self,
///         _since: Option<SequenceId>,
///     ) -> Result<Vec<SessionEvent>, SessionError> {
///         Ok(Vec::new())
///     }
///
///     async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
///         Ok(ConversationSnapshot::default())
///     }
/// }
/// ```
#[async_trait]
pub trait Session: Send + Sync {
    /// Append events to the log.
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError>;

    /// Read events from the log, optionally only those after `since`.
    async fn events(
        &self,
        since: Option<SequenceId>,
    ) -> Result<Vec<SessionEvent>, SessionError>;

    /// Compute (or read) a [`ConversationSnapshot`] projection of the log.
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError>;
}

/// One entry in the conversation event log.
///
/// Variant fields (content/timestamp shapes) will graduate from the
/// placeholder types here to the canonical `Item` and `DateTime<Utc>`
/// types with later tickets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionEvent {
    /// A user-authored message.
    UserMessage {
        /// Message text.
        text: String,
    },
    /// An assistant-authored message attributed to a named agent.
    AssistantMessage {
        /// Message text.
        text: String,
        /// Name of the emitting [`crate::Agent`].
        agent: String,
    },
    /// The runner invoked a tool.
    ToolCalled {
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        name: String,
        /// JSON arguments.
        args: serde_json::Value,
    },
    /// The tool returned.
    ToolReturned {
        /// Matching call identifier.
        call_id: String,
        /// JSON output.
        output: serde_json::Value,
    },
    /// Control transferred from one agent to another.
    HandoffOccurred {
        /// Outgoing agent name.
        from: String,
        /// Incoming agent name.
        to: String,
    },
    /// Older events were compacted into a summary.
    Compacted {
        /// LLM-produced summary.
        summary: String,
        /// Number of events the summary replaces.
        original_count: usize,
    },
}

/// Monotonic position in a [`Session`]'s append-only log.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct SequenceId(pub u64);

/// A computed projection of a [`Session`]'s log into a single conversation
/// state. Field shape lands with later tickets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConversationSnapshot {}

/// Errors raised by [`Session`] methods.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// Backend unreachable (database down, file locked, …).
    #[error("session backend unavailable")]
    Unavailable,

    /// Escape hatch.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

- [ ] **Step 3: Verify**

```bash
cargo fmt --all
cargo build -p paigasus-helikon-core
cargo test --doc -p paigasus-helikon-core
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p paigasus-helikon-core
```

Expected: every command exits 0.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon-core/src/session.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-312 add Session trait and event-log carriers

Adds Session, SessionEvent (six variants from the Sessions concept page),
SequenceId, ConversationSnapshot, and SessionError. Models conversation
persistence as an append-only event log to support eval replay,
durability, and audit.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `Guardrail<Ctx>` trait + supporting types + `GuardrailError`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/lib.rs`
- Create: `crates/paigasus-helikon-core/src/guardrail.rs`

- [ ] **Step 1: Add the module to `lib.rs`**

Add `pub mod guardrail;` and `pub use guardrail::*;` to `lib.rs`. The full file:

```rust
//! Trait surface and core types for the Paigasus Helikon AI SDK.
//!
//! This crate is the dependency root of the workspace; the facade crate
//! [`paigasus-helikon`] re-exports its surface unconditionally.
//!
//! The seven object-safe traits ([`Model`], [`Tool`], [`Session`],
//! [`Guardrail`], [`Hook`], [`Agent`], [`Runner`]) and their carrier
//! types form the contract every other Paigasus Helikon crate depends on.
//!
//! See the [project documentation site] for conceptual material; this
//! crate's rustdoc is the canonical reference for the trait signatures and
//! carrier types.
//!
//! [`paigasus-helikon`]: https://docs.rs/paigasus-helikon
//! [project documentation site]: https://smk1085.github.io/paigasus-helikon/

pub mod context;
pub mod guardrail;
pub mod model;
pub mod session;
pub mod tool;

pub use context::*;
pub use guardrail::*;
pub use model::*;
pub use session::*;
pub use tool::*;
```

- [ ] **Step 2: Create `crates/paigasus-helikon-core/src/guardrail.rs`**

```rust
//! The [`Guardrail`] trait and its carrier types.
//!
//! Guardrails validate input/output **in parallel** with the agent
//! (optimistic execution). When a tripwire fires, the run halts. See the
//! *Permissions, Guardrails & Hooks* concept page.

use async_trait::async_trait;

use crate::RunContext;

/// Input/output safety check that runs in parallel with the agent.
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use paigasus_helikon_core::{
///     Guardrail, GuardrailError, GuardrailInput, GuardrailVerdict, RunContext,
/// };
///
/// struct NoopGuardrail;
///
/// #[async_trait]
/// impl Guardrail<()> for NoopGuardrail {
///     async fn check(
///         &self,
///         _ctx: &RunContext<()>,
///         _input: GuardrailInput<'_>,
///     ) -> Result<GuardrailVerdict, GuardrailError> {
///         Ok(GuardrailVerdict::Pass)
///     }
/// }
/// ```
#[async_trait]
pub trait Guardrail<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Inspect `input` and return a [`GuardrailVerdict`]. A `Tripwire`
    /// verdict halts the run.
    async fn check(
        &self,
        ctx: &RunContext<Ctx>,
        input: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError>;
}

/// What a [`Guardrail`] inspects.
///
/// Variants will grow alongside the wire-format ticket.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum GuardrailInput<'a> {
    /// User-supplied text entering the agent.
    UserText(&'a str),
    /// Model-emitted text leaving the agent.
    ModelOutput(&'a str),
}

/// The outcome of a [`Guardrail::check`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum GuardrailVerdict {
    /// All clear — the run continues.
    Pass,
    /// Tripwire fired — the run halts and the runner emits a corresponding
    /// agent event.
    Tripwire {
        /// Which kind of tripwire fired.
        kind: GuardrailKind,
        /// Free-form auxiliary information.
        info: serde_json::Value,
    },
}

/// The category of a fired tripwire.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuardrailKind {
    /// Input failed a policy check.
    InputPolicy,
    /// Output failed a policy check.
    OutputPolicy,
    /// User-defined category.
    Other(String),
}

/// Errors raised by [`Guardrail::check`] itself (distinct from a tripwire
/// firing — a tripwire is a *successful* verdict that halts the run).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GuardrailError {
    /// Escape hatch.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

- [ ] **Step 3: Verify**

```bash
cargo fmt --all
cargo build -p paigasus-helikon-core
cargo test --doc -p paigasus-helikon-core
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p paigasus-helikon-core
```

Expected: every command exits 0.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon-core/src/guardrail.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-312 add Guardrail trait and verdict carriers

Adds Guardrail<Ctx>, GuardrailInput, GuardrailVerdict, GuardrailKind,
and GuardrailError. A tripwire is modeled as a successful verdict, not
an error — the GuardrailError enum is reserved for the guardrail itself
crashing.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: `Hook<Ctx>` trait + supporting types

**Files:**
- Modify: `crates/paigasus-helikon-core/src/lib.rs`
- Create: `crates/paigasus-helikon-core/src/hook.rs`

`Hook` has no dedicated error enum — a hook returns a [`HookDecision`] for every event, including failures (the application can model failures via `HookDecision::Deny`).

- [ ] **Step 1: Add the module to `lib.rs`**

Add `pub mod hook;` and `pub use hook::*;`. The full file:

```rust
//! Trait surface and core types for the Paigasus Helikon AI SDK.
//!
//! This crate is the dependency root of the workspace; the facade crate
//! [`paigasus-helikon`] re-exports its surface unconditionally.
//!
//! The seven object-safe traits ([`Model`], [`Tool`], [`Session`],
//! [`Guardrail`], [`Hook`], [`Agent`], [`Runner`]) and their carrier
//! types form the contract every other Paigasus Helikon crate depends on.
//!
//! See the [project documentation site] for conceptual material; this
//! crate's rustdoc is the canonical reference for the trait signatures and
//! carrier types.
//!
//! [`paigasus-helikon`]: https://docs.rs/paigasus-helikon
//! [project documentation site]: https://smk1085.github.io/paigasus-helikon/

pub mod context;
pub mod guardrail;
pub mod hook;
pub mod model;
pub mod session;
pub mod tool;

pub use context::*;
pub use guardrail::*;
pub use hook::*;
pub use model::*;
pub use session::*;
pub use tool::*;
```

- [ ] **Step 2: Create `crates/paigasus-helikon-core/src/hook.rs`**

```rust
//! The [`Hook`] trait and its carrier types.
//!
//! Hooks intercept lifecycle events (`PreToolUse`, `PostToolUse`,
//! `OnTurnStart`, `OnHandoff`, …). They are *observation and side effects*
//! — distinct from permissions (authorization) and guardrails (content).

use async_trait::async_trait;

use crate::RunContext;

/// Lifecycle interceptor.
///
/// Hooks fire on the events listed in [`HookEvent`]. Each hook returns a
/// [`HookDecision`] that the runner honors before continuing.
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use paigasus_helikon_core::{Hook, HookDecision, HookEvent, RunContext};
///
/// struct NoopHook;
///
/// #[async_trait]
/// impl Hook<()> for NoopHook {
///     async fn on_event(
///         &self,
///         _ctx: &RunContext<()>,
///         _event: &HookEvent,
///     ) -> HookDecision {
///         HookDecision::Allow
///     }
/// }
/// ```
#[async_trait]
pub trait Hook<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Fire on `event` and return a [`HookDecision`].
    async fn on_event(
        &self,
        ctx: &RunContext<Ctx>,
        event: &HookEvent,
    ) -> HookDecision;
}

/// A lifecycle event seen by a [`Hook`].
///
/// Variants mirror the Claude Agent SDK's hook taxonomy. Additional
/// variants land with the agent-loop ticket.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum HookEvent {
    /// Fired once at the start of a run.
    OnRunStart,
    /// Fired at the start of each turn.
    OnTurnStart {
        /// Zero-based turn index.
        turn: u32,
    },
    /// Fired just before a tool is invoked.
    PreToolUse {
        /// Tool name.
        tool: String,
        /// JSON arguments about to be passed.
        args: serde_json::Value,
    },
    /// Fired just after a tool returns.
    PostToolUse {
        /// Tool name.
        tool: String,
        /// JSON output the tool produced.
        output: serde_json::Value,
    },
    /// Fired at a handoff from one agent to another.
    OnHandoff {
        /// Outgoing agent name.
        from: String,
        /// Incoming agent name.
        to: String,
    },
    /// Fired once at the end of a run.
    OnRunComplete,
}

/// A [`Hook`]'s reply to a [`HookEvent`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum HookDecision {
    /// Allow the event to proceed unchanged.
    Allow,
    /// Block the event with a human-readable reason.
    Deny {
        /// Reason surfaced to the agent.
        reason: String,
    },
    /// Replace the input value the runner is about to use (e.g. sanitize
    /// `PreToolUse` arguments).
    ReplaceInput {
        /// Replacement value.
        value: serde_json::Value,
    },
    /// Replace the output value the runner just observed (e.g. redact
    /// `PostToolUse` output).
    ReplaceOutput {
        /// Replacement value.
        value: serde_json::Value,
    },
    /// Inject a system message into the next model call.
    InjectSystemMessage {
        /// Text to inject.
        text: String,
    },
}
```

- [ ] **Step 3: Verify**

```bash
cargo fmt --all
cargo build -p paigasus-helikon-core
cargo test --doc -p paigasus-helikon-core
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p paigasus-helikon-core
```

Expected: every command exits 0.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon-core/src/hook.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-312 add Hook trait and lifecycle event carriers

Adds Hook<Ctx>, HookEvent (six variants from the Claude Agent SDK hook
taxonomy), and HookDecision. No dedicated error enum — hook failures are
modeled via HookDecision::Deny.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: `Agent<Ctx>` trait + supporting types + `AgentError`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/lib.rs`
- Create: `crates/paigasus-helikon-core/src/agent.rs`

- [ ] **Step 1: Add the module to `lib.rs`**

Add `pub mod agent;` and `pub use agent::*;`. The full file:

```rust
//! Trait surface and core types for the Paigasus Helikon AI SDK.
//!
//! This crate is the dependency root of the workspace; the facade crate
//! [`paigasus-helikon`] re-exports its surface unconditionally.
//!
//! The seven object-safe traits ([`Model`], [`Tool`], [`Session`],
//! [`Guardrail`], [`Hook`], [`Agent`], [`Runner`]) and their carrier
//! types form the contract every other Paigasus Helikon crate depends on.
//!
//! See the [project documentation site] for conceptual material; this
//! crate's rustdoc is the canonical reference for the trait signatures and
//! carrier types.
//!
//! [`paigasus-helikon`]: https://docs.rs/paigasus-helikon
//! [project documentation site]: https://smk1085.github.io/paigasus-helikon/

pub mod agent;
pub mod context;
pub mod guardrail;
pub mod hook;
pub mod model;
pub mod session;
pub mod tool;

pub use agent::*;
pub use context::*;
pub use guardrail::*;
pub use hook::*;
pub use model::*;
pub use session::*;
pub use tool::*;
```

- [ ] **Step 2: Create `crates/paigasus-helikon-core/src/agent.rs`**

```rust
//! The [`Agent`] trait and its carrier types.
//!
//! One trait covers LLM-driven agents (`LlmAgent`) and workflow agents
//! (`SequentialAgent`, `ParallelAgent`, `LoopAgent`, `SwarmAgent`,
//! `GraphAgent`) — see ADR-11.

use async_trait::async_trait;
use futures_core::stream::BoxStream;

use crate::{GuardrailKind, ModelError, RunContext, SessionError, ToolError};

/// One trait for both LLM-driven and workflow agents.
///
/// See ADR-11 (*Single Agent trait subsumes LLM-driven and workflow
/// agents*).
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use futures_core::stream::BoxStream;
/// use paigasus_helikon_core::{
///     Agent, AgentError, AgentEvent, AgentInput, RunContext,
/// };
///
/// struct NoopAgent;
///
/// #[async_trait]
/// impl Agent<()> for NoopAgent {
///     fn name(&self) -> &str { "noop" }
///     fn description(&self) -> &str { "Does nothing." }
///
///     async fn run(
///         &self,
///         _ctx: RunContext<()>,
///         _input: AgentInput,
///     ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
///         use futures_core::stream::Stream;
///         use std::pin::Pin;
///
///         let empty: Pin<Box<dyn Stream<Item = AgentEvent> + Send>> =
///             Box::pin(futures_core::stream::empty());
///         Ok(empty)
///     }
/// }
/// ```
#[async_trait]
pub trait Agent<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Agent name. Used as the `agent` field in `SessionEvent::AssistantMessage`
    /// and `HookEvent::OnHandoff`.
    fn name(&self) -> &str;
    /// Human-readable description.
    fn description(&self) -> &str;

    /// Run the agent.
    ///
    /// The outer `Result` covers failure to *start* the stream; fatal
    /// errors during the run surface as [`AgentEvent::RunFailed`] inside
    /// the stream.
    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError>;
}

/// The input envelope crossing the agent boundary.
///
/// Field shape (user text, attachments, previous-response handles) lands
/// with the agent-loop ticket.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AgentInput {}

/// The unified event stream emitted by an [`Agent`].
///
/// The full 14-variant ADT (token deltas, semantic items, approvals,
/// guardrail signals, …) lands with the agent-loop ticket. This trimmed
/// set covers the lifecycle the trait surface needs.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AgentEvent {
    /// The run has started; the named agent is active.
    RunStarted {
        /// Agent name.
        agent: String,
    },
    /// A token-level delta in the assistant channel (for low-latency UIs).
    TokenDelta {
        /// Text fragment.
        text: String,
    },
    /// The run finished normally.
    RunCompleted,
    /// The run finished with an error.
    RunFailed {
        /// Human-readable error message.
        error: String,
    },
}

/// Errors raised by [`Agent::run`] or [`crate::Runner`] methods.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentError {
    /// A downstream model call failed.
    #[error("model failed: {0}")]
    Model(#[from] ModelError),

    /// A downstream tool call failed.
    #[error("tool failed: {0}")]
    Tool(#[from] ToolError),

    /// A session-backend call failed.
    #[error("session failed: {0}")]
    Session(#[from] SessionError),

    /// A guardrail tripwire fired and halted the run.
    #[error("guardrail tripped: {kind:?}")]
    Guardrail {
        /// Which kind of tripwire fired.
        kind: GuardrailKind,
    },

    /// The model produced output that could not be coerced into the
    /// requested structured type, even after the one-shot repair attempt
    /// allowed by ADR-10.
    #[error("invalid structured output after one repair attempt")]
    InvalidStructuredOutput,

    /// Escape hatch.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

- [ ] **Step 3: Verify**

```bash
cargo fmt --all
cargo build -p paigasus-helikon-core
cargo test --doc -p paigasus-helikon-core
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p paigasus-helikon-core
```

Expected: every command exits 0. (The `Agent` doctest pulls in `futures_core::stream::empty()` — this is part of the `futures-core` 0.3 surface; it works for doctests because `futures-core` is a direct dep of the crate.)

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon-core/src/agent.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-312 add Agent trait and event/error carriers

Adds Agent<Ctx>, AgentInput, AgentEvent (4-variant lifecycle subset; full
ADT lands with the agent-loop ticket), and AgentError. AgentError carries
From conversions for ModelError, ToolError, and SessionError so the runner
can use the `?` operator naturally.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: `Runner<Ctx>` trait + supporting types + `RunError`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/lib.rs`
- Create: `crates/paigasus-helikon-core/src/runner.rs`

- [ ] **Step 1: Add the module to `lib.rs`**

Add `pub mod runner;` and `pub use runner::*;`. The full file (final form for this task, before the docs update):

```rust
//! Trait surface and core types for the Paigasus Helikon AI SDK.
//!
//! This crate is the dependency root of the workspace; the facade crate
//! [`paigasus-helikon`] re-exports its surface unconditionally.
//!
//! The seven object-safe traits ([`Model`], [`Tool`], [`Session`],
//! [`Guardrail`], [`Hook`], [`Agent`], [`Runner`]) and their carrier
//! types form the contract every other Paigasus Helikon crate depends on.
//!
//! See the [project documentation site] for conceptual material; this
//! crate's rustdoc is the canonical reference for the trait signatures and
//! carrier types.
//!
//! [`paigasus-helikon`]: https://docs.rs/paigasus-helikon
//! [project documentation site]: https://smk1085.github.io/paigasus-helikon/

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

- [ ] **Step 2: Create `crates/paigasus-helikon-core/src/runner.rs`**

```rust
//! The [`Runner`] trait and its carrier types.
//!
//! The runner is the durability seam (per ADR-6): swappable between
//! ephemeral tokio (`paigasus-helikon-runtime-tokio`), durable Temporal
//! (`paigasus-helikon-runtime-temporal`), and AWS AgentCore
//! (`paigasus-helikon-runtime-agentcore`).

use async_trait::async_trait;

use crate::{Agent, AgentError, AgentInput, RunContext};

/// Pluggable execution backend.
///
/// `Runner` is object-safe: the per-method bound `A: Agent<Ctx> + ?Sized`
/// (rather than a `<A: Agent<Ctx>>` parameter on the trait) keeps the
/// trait itself dyn-safe while accepting both concrete `&LlmAgent<…>` and
/// `&dyn Agent<Ctx>` at the call site.
///
/// See ADR-6 (*Library + pluggable Runner trait*).
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use paigasus_helikon_core::{
///     Agent, AgentInput, RunConfig, RunContext, RunError, RunResult,
///     RunResultStreaming, Runner,
/// };
///
/// struct NoopRunner;
///
/// #[async_trait]
/// impl Runner<()> for NoopRunner {
///     async fn run<A>(
///         &self,
///         _agent: &A,
///         _ctx: RunContext<()>,
///         _input: AgentInput,
///         _config: RunConfig,
///     ) -> Result<RunResult, RunError>
///     where
///         A: Agent<()> + ?Sized,
///     {
///         Ok(RunResult::default())
///     }
///
///     async fn run_streamed<A>(
///         &self,
///         _agent: &A,
///         _ctx: RunContext<()>,
///         _input: AgentInput,
///         _config: RunConfig,
///     ) -> Result<RunResultStreaming, RunError>
///     where
///         A: Agent<()> + ?Sized,
///     {
///         Ok(RunResultStreaming::default())
///     }
/// }
/// ```
#[async_trait]
pub trait Runner<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Run the agent to completion and return the aggregated result.
    async fn run<A>(
        &self,
        agent: &A,
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResult, RunError>
    where
        A: Agent<Ctx> + ?Sized;

    /// Run the agent and return a streaming result handle.
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

/// Configuration for a single [`Runner::run`] / [`Runner::run_streamed`]
/// invocation. Field shape (max iterations, retry policy, tracing
/// settings) lands with the runner ticket.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RunConfig {}

/// The aggregated outcome of a non-streaming [`Runner::run`]. Field shape
/// (final response, trajectory, token counts) lands with the runner
/// ticket.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct RunResult {}

/// A streaming handle returned by [`Runner::run_streamed`]. Field shape
/// (the inner `BoxStream<AgentEvent>` and the final-result future) lands
/// with the runner ticket.
#[derive(Default)]
#[non_exhaustive]
pub struct RunResultStreaming {}

/// Errors raised by [`Runner`] methods.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    /// The underlying agent failed.
    #[error("agent failed: {0}")]
    Agent(#[from] AgentError),

    /// The runner hit the configured maximum iteration count.
    #[error("max iterations reached")]
    MaxIterations,

    /// The run was cancelled (e.g. via [`crate::CancellationToken`]).
    #[error("cancelled")]
    Cancelled,

    /// Escape hatch.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

- [ ] **Step 3: Verify**

```bash
cargo fmt --all
cargo build -p paigasus-helikon-core
cargo test --doc -p paigasus-helikon-core
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p paigasus-helikon-core
```

Expected: every command exits 0. If the `Runner` doctest fails with an object-safety error, double-check that the bound on each method is `where A: Agent<Ctx> + ?Sized` (not `<A: Agent<Ctx>>` on the trait header).

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon-core/src/runner.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-312 add Runner trait and run-result carriers

Adds Runner<Ctx> (object-safe via per-method A: Agent<Ctx> + ?Sized
bounds), RunConfig, RunResult, RunResultStreaming, and RunError. The
seventh and final trait in the SMA-312 surface.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: `tests/object_safety.rs` integration test

**Files:**
- Create: `crates/paigasus-helikon-core/tests/object_safety.rs`

This locks acceptance criterion #2: every object-safe trait can be held behind `Box<dyn _>` and `Vec<Arc<dyn _>>`. The body of the test is mostly type ascriptions — the real verification is that the file *compiles*. A future refactor that accidentally breaks object-safety (e.g. by adding a generic method to a trait header without `async-trait`'s help) fails to compile here and fails CI.

- [ ] **Step 1: Create the test file**

Create `crates/paigasus-helikon-core/tests/object_safety.rs`:

```rust
//! Locks acceptance criterion #2 of SMA-312: every object-safe trait can
//! be held behind `Box<dyn _>` and `Vec<Arc<dyn _>>`. A compile failure
//! here is an AC regression.

use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::{BoxStream, Stream};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, CancellationToken,
    ConversationSnapshot, Guardrail, GuardrailError, GuardrailInput,
    GuardrailVerdict, Hook, HookDecision, HookEvent, Model, ModelCapabilities,
    ModelError, ModelEvent, ModelRequest, RunConfig, RunContext, RunError,
    RunResult, RunResultStreaming, Runner, SequenceId, Session, SessionError,
    SessionEvent, Tool, ToolContext, ToolError, ToolOutput,
};
use serde_json::{json, Value};

struct NoopModel;

#[async_trait]
impl Model for NoopModel {
    async fn invoke(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        Err(ModelError::Unavailable)
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

struct NoopTool {
    schema: Value,
}

#[async_trait]
impl Tool<()> for NoopTool {
    fn name(&self) -> &str {
        "noop"
    }
    fn description(&self) -> &str {
        "Does nothing."
    }
    fn schema(&self) -> &Value {
        &self.schema
    }

    async fn invoke(
        &self,
        _ctx: &ToolContext<()>,
        _args: Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::default())
    }
}

struct NoopSession;

#[async_trait]
impl Session for NoopSession {
    async fn append(
        &self,
        _events: &[SessionEvent],
    ) -> Result<(), SessionError> {
        Ok(())
    }

    async fn events(
        &self,
        _since: Option<SequenceId>,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        Ok(Vec::new())
    }

    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        Ok(ConversationSnapshot::default())
    }
}

struct NoopGuardrail;

#[async_trait]
impl Guardrail<()> for NoopGuardrail {
    async fn check(
        &self,
        _ctx: &RunContext<()>,
        _input: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError> {
        Ok(GuardrailVerdict::Pass)
    }
}

struct NoopHook;

#[async_trait]
impl Hook<()> for NoopHook {
    async fn on_event(
        &self,
        _ctx: &RunContext<()>,
        _event: &HookEvent,
    ) -> HookDecision {
        HookDecision::Allow
    }
}

struct NoopAgent;

#[async_trait]
impl Agent<()> for NoopAgent {
    fn name(&self) -> &str {
        "noop"
    }
    fn description(&self) -> &str {
        "Does nothing."
    }

    async fn run(
        &self,
        _ctx: RunContext<()>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let empty: std::pin::Pin<Box<dyn Stream<Item = AgentEvent> + Send>> =
            Box::pin(futures_core::stream::empty());
        Ok(empty)
    }
}

struct NoopRunner;

#[async_trait]
impl Runner<()> for NoopRunner {
    async fn run<A>(
        &self,
        _agent: &A,
        _ctx: RunContext<()>,
        _input: AgentInput,
        _config: RunConfig,
    ) -> Result<RunResult, RunError>
    where
        A: Agent<()> + ?Sized,
    {
        Ok(RunResult::default())
    }

    async fn run_streamed<A>(
        &self,
        _agent: &A,
        _ctx: RunContext<()>,
        _input: AgentInput,
        _config: RunConfig,
    ) -> Result<RunResultStreaming, RunError>
    where
        A: Agent<()> + ?Sized,
    {
        Ok(RunResultStreaming::default())
    }
}

#[test]
fn trait_objects_construct() {
    let _: Box<dyn Model> = Box::new(NoopModel);

    let _: Vec<Arc<dyn Tool<()>>> = vec![Arc::new(NoopTool {
        schema: json!({ "type": "object" }),
    })];

    let _: Box<dyn Session> = Box::new(NoopSession);
    let _: Box<dyn Guardrail<()>> = Box::new(NoopGuardrail);
    let _: Box<dyn Hook<()>> = Box::new(NoopHook);
    let _: Box<dyn Agent<()>> = Box::new(NoopAgent);
    let _: Box<dyn Runner<()>> = Box::new(NoopRunner);
}
```

- [ ] **Step 2: Verify**

```bash
cargo fmt --all
cargo test --test object_safety -p paigasus-helikon-core
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
```

Expected: `cargo test --test object_safety` exits 0 with one passing test (`trait_objects_construct`). `cargo clippy` exits 0.

If any trait is not object-safe, you will see an error of the form `the trait `Foo` cannot be made into an object`. The fix is on the trait definition, not in this test file.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-core/tests/object_safety.rs
git commit -m "$(cat <<'EOF'
test(core): SMA-312 lock object-safety of every trait

Integration test that constructs Box<dyn _> and Vec<Arc<dyn _>> for each
of the seven traits against a trivial impl. The real verification is
that the file compiles — a future refactor that accidentally breaks
object-safety fails here and fails CI.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Update `docs/book/src/concepts/core-primitives.md`

**Files:**
- Modify: `docs/book/src/concepts/core-primitives.md`

The mdBook page is currently a stub. Now that the traits exist in rustdoc, swap the "Stub" callout for a one-paragraph pointer to rustdoc, leaving the page intentionally light until the SDK ships a user-facing release.

- [ ] **Step 1: Replace the file contents**

Overwrite `docs/book/src/concepts/core-primitives.md`:

```markdown
# Core Primitives

The seven object-safe traits — `Model`, `Tool<Ctx>`, `Agent<Ctx>`, `Session`, `Guardrail<Ctx>`, `Hook<Ctx>`, `Runner<Ctx>` — and the concrete carrier types they share.

The trait surface lives in the [`paigasus-helikon-core`](https://github.com/SMK1085/paigasus-helikon/tree/main/crates/paigasus-helikon-core) crate. Each trait carries a worked rustdoc example. Until the workspace publishes to crates.io, the source itself is the canonical reference; rustdoc HTML will become available on docs.rs after the first published release.

The seven traits were chosen as the minimum viable surface. Other primitives users may expect — `Memory`, `KnowledgeBase`, `Toolset`, `Plugin` — are either compositions of these seven (e.g. a `Toolset` is a function returning `Vec<Arc<dyn Tool<Ctx>>>`) or premature.
```

- [ ] **Step 2: Verify the book still builds**

```bash
cd docs/book && mdbook build && cd ../..
```

Expected: exits 0. If the `mdbook` binary is not installed locally, skip this step — the `book-build` CI job runs the same check on PR.

- [ ] **Step 3: Commit**

```bash
git add docs/book/src/concepts/core-primitives.md
git commit -m "$(cat <<'EOF'
docs(repo): SMA-312 replace core-primitives stub with rustdoc pointer

The trait surface now lives in paigasus-helikon-core; the book page
points readers to the source until docs.rs publishes the first release.

The `repo` scope (rather than `book`) follows the SMA-311 precedent —
`book` is not in the .versionrc scope allowlist.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Full local CI gate verification

**Files:** none (verification only)

Reproduce every CI job locally before opening the PR. This is the same gate that runs on the PR; running it locally surfaces failures before a CI round-trip.

- [ ] **Step 1: Format check**

```bash
cargo fmt --all -- --check
```

Expected: exits 0 with no output. If anything fails, run `cargo fmt --all` and amend the most recent commit (it's still local, not pushed).

- [ ] **Step 2: Clippy on the whole workspace, all features, all targets**

```bash
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: exits 0. Any warning becomes an error.

- [ ] **Step 3: Test the whole workspace, all features**

```bash
cargo test --workspace --all-features
```

Expected: exits 0. The `trait_objects_construct` test in `paigasus-helikon-core` runs; every doc-test compiles and runs.

- [ ] **Step 4: Rustdoc**

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: exits 0. The known non-fatal facade/CLI filename-collision warning (`paigasus-helikon` lib vs. `paigasus-helikon` CLI binary) is documented in `CLAUDE.md` as an accepted noise; if `-D warnings` were to escalate it to an error, the docs job in CI would already be broken — which it isn't.

- [ ] **Step 5: Doc-coverage**

```bash
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
```

Expected: exits 0 with the per-crate coverage table showing `paigasus-helikon-core` at ≥80%. The crate is densely-documented because every public item has rustdoc; expect well above the threshold.

If the nightly toolchain isn't installed locally: `rustup toolchain install nightly-2026-05-01`. If the script reports `paigasus-helikon-core` below 80%, the most likely culprit is a public item with no `///` comment — `cargo doc --no-deps -p paigasus-helikon-core` with `-D warnings` would have caught it in Step 4, so this should never trigger in practice.

- [ ] **Step 6: MSRV verification**

```bash
cargo msrv --path crates/paigasus-helikon-core verify
```

Expected: exits 0. The seven runtime deps all support MSRV 1.75.

If `cargo-msrv` is not installed locally: `cargo install cargo-msrv --locked`. If MSRV verification fails, do **not** downgrade a dep — bump `rust-version` in the workspace `Cargo.toml`'s `[workspace.package]` to what cargo demands, per CLAUDE.md.

- [ ] **Step 7: Verify the facade re-export still works**

The `paigasus-helikon` facade re-exports `paigasus-helikon-core` unconditionally (per SMA-304). Confirm the new surface flows through:

```bash
cargo check -p paigasus-helikon
```

Expected: exits 0. No facade edit was needed; the wildcard re-export from SMA-304 picks up every new public item automatically.

- [ ] **Step 8: Push the branch and open the PR**

```bash
git push -u origin feature/sma-312-define-core-trait-surface-model-tool-agent-session-guardrail
gh pr create --title "feat(core): SMA-312 define core trait surface" --body "$(cat <<'EOF'
## Summary

Lands the seven canonical object-safe traits of `paigasus-helikon-core`:

- `Model` — single async LLM interface, capability-flagged per ADR-1.
- `Tool<Ctx>` — heterogeneous tool registry.
- `Session` — append-only conversation event log.
- `Guardrail<Ctx>` — input/output safety check, runs in parallel with the agent.
- `Hook<Ctx>` — lifecycle interceptor.
- `Agent<Ctx>` — one trait for LLM-driven and workflow agents (per ADR-11).
- `Runner<Ctx>` — pluggable execution backend (per ADR-6, the durability seam).

Each trait uses `#[async_trait::async_trait]` for dyn-safety (per ADR-2). Errors are six `thiserror` enums, all `#[non_exhaustive]`, all with an `Other(anyhow::Error)` escape hatch. No silent retries anywhere — `ToolError::InvalidArgs` is the single recoverable variant per ADR-10.

Carrier types (`ModelRequest`, `AgentInput`, `RunConfig`, `RunResult`, …) are named publicly with minimum-viable shape; full field shape lands in follow-up tickets (agent loop, provider crates, `Item` wire format).

See [`docs/superpowers/specs/2026-05-21-sma-312-core-trait-surface-design.md`](docs/superpowers/specs/2026-05-21-sma-312-core-trait-surface-design.md) for the full design.

## Test plan

- [x] `cargo fmt --all -- --check` passes locally.
- [x] `cargo clippy --workspace --all-features --all-targets -- -D warnings` passes locally.
- [x] `cargo test --workspace --all-features` passes locally (including the `trait_objects_construct` integration test and every rustdoc example).
- [x] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps` passes locally.
- [x] `DOC_COVERAGE_THRESHOLD=80 bash scripts/check-doc-coverage.sh` passes locally.
- [x] `cargo msrv --path crates/paigasus-helikon-core verify` passes locally.
- [x] CI green on this PR.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: the PR is created and CI starts. The PR URL is printed; paste it into the chat.

- [ ] **Step 9: Watch CI**

```bash
gh pr checks --watch
```

Expected: all required checks (`fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`) report green. If any matrix variant (`test (macos-latest, …)`, `test (windows-latest, …)`, `test (…, 1.75)`) fails, debug locally and push a fix commit — these are required signals in spirit even if not gated as required-status-checks.

When CI is green, the PR is ready for review and merge. Linear will auto-close SMA-312 on merge.

---

## Out of scope (will be separate tickets)

Listed in §10 of the spec, reproduced here for quick reference so the executing engineer can recognize them and not scope-creep:

- `LoopState` enum and the runner's agent-loop implementation.
- `PermissionMode` / `PermissionPolicy` / `PermissionDecision`.
- The canonical `Item` wire format (used by `ModelRequest`, `SessionEvent`, etc.).
- `Instructions<Ctx>`, the typestate builder, `LlmAgent<Ctx, M>`.
- Default backends (`MemorySession`, `SqliteSession`, `OpenAiModel`, `AnthropicModel`, …).
- The `#[tool]` procedural macro.
- `RunConfig::retry_policy` sub-shape.
- `paigasus::schema::strict()` helper for per-provider JSON Schema rewriting.

If any of these feel like they need to land alongside the trait surface to be useful, push back through the spec — the design explicitly defers them. Don't sneak them in.
