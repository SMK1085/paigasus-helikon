# SMA-313 — Concrete shared types: `Item`, `AgentEvent`, `RunContext`, `RunResult`, `ToolContext` — design

- **Linear:** [SMA-313](https://linear.app/smaschek/issue/SMA-313/concrete-shared-types-item-agentevent-runcontext-runresult-toolcontext)
- **Branch:** `feature/sma-313-concrete-shared-types-item-agentevent-runcontext-runresult`
- **Status:** design (awaiting implementation plan)
- **Author:** Sven Maschek
- **Date:** 2026-05-21

## 1. Goal

Fill in the **data plane** that the seven SMA-312 traits exchange. Five named types graduate from placeholders or stubs to real shapes:

| Type | Today (post-SMA-312) | After SMA-313 |
|---|---|---|
| `Item` | does not exist | new canonical wire-format ADT — superset of OpenAI Chat Completions, OpenAI Responses, Anthropic Messages, and Bedrock Converse shapes |
| `AgentEvent` | 4 stub variants (`RunStarted`, `TokenDelta`, `RunCompleted`, `RunFailed`) | full 14-variant ADT spanning lifecycle, raw deltas, semantic items, agent transitions, control, and terminal events |
| `RunContext<Ctx>` | `PhantomData` placeholder | real fields: `user_ctx: Arc<Ctx>`, `session: Arc<dyn Session>`, `hooks: HookRegistry<Ctx>`, `tracer: TracerHandle`, `cancel: CancellationToken` |
| `ToolContext<Ctx>` | `PhantomData` placeholder | narrower view: `user_ctx`, `tracer`, `cancel` only (no session, no hooks) |
| `RunResult` | unit-struct `{}` | `RunResult<T = String> { final_output: T, events: Vec<AgentEvent>, usage: TokenUsage }` |

One new supporting type lands too: `TokenUsage { input_tokens, output_tokens, cached_input_tokens, reasoning_tokens, total_tokens: u64 }`. Two new opaque carriers (`HookRegistry<Ctx>`, `TracerHandle`) exist as the minimum shape needed for `RunContext` to compile; their full surface lands with the agent-loop and observability tickets respectively.

The Linear ticket's acceptance criteria are:

1. **Round-trip serde on all variants** (snapshot tests).
2. **`RunResult<MyStruct>` compiles** when `MyStruct: DeserializeOwned + JsonSchema`.

This design meets both. AC #1 is locked by `tests/serde_roundtrip.rs` (§7); AC #2 is locked by `tests/compile_run_result_typed.rs` (§7).

### Cohesive-scope call

The Linear ticket lists five types but the title says "the data plane that the seven traits exchange". Two readings:

- **Tight** — define the five new types; leave `SessionEvent`'s `String`-bearing variants for a mechanical follow-up.
- **Cohesive (this design)** — define the five new types **and** graduate `SessionEvent::{UserMessage, AssistantMessage, ToolReturned}` to carry `Vec<ContentPart>` so the data plane is end-to-end coherent in one ticket. `ConversationSnapshot` gains `messages: Vec<Item>`.

The cohesive scope is chosen because the ticket title and the SMA-312 spec §10 ("`SessionEvent` … will later use `Item`") both point at it, and because `SessionEvent` was shipped on 2026-05-21 with no downstream consumers yet — the migration is safe to bundle.

### Object-safety call

The `Runner<Ctx>` trait surface stays **exactly as shipped in SMA-312**. `Runner::run`'s return type is `Result<RunResult, RunError>`, which now resolves to `Result<RunResult<String>, RunError>` via the default type parameter on `RunResult<T = String>`. Object-safety is preserved; no `where Self: Sized` machinery is needed. The structured-output path is `runner.run(...).await?.parse_final::<MyStruct>()?` — a method on `RunResult<String>`, not a method on the trait.

A future ticket may introduce a `Runner::run_structured<T>` method gated by `where Self: Sized` for callers holding `&LlmRunner` directly (not `&dyn Runner<_>`), but it is out of scope here.

### Item-shape call (superset-of-providers strategy)

`Item` and `ContentPart` deliberately carry the union of OpenAI Chat-Completions-style "sibling role" tool calls and Anthropic Messages-style "content block" tool calls. The duplication (`Item::ToolCall` ⇄ `ContentPart::ToolUse`, `Item::ToolResult` ⇄ `ContentPart::ToolResult`) is a feature: each provider crate serializes the variant native to its wire format and deserializes the variant the provider returns. Lossless round-trip in both directions.

## 2. Decisions and rationale

| Decision | Choice | Rationale |
|---|---|---|
| Scope | **Cohesive** — five new types **plus** `SessionEvent` migration. See §1. | Title says "data plane that the seven traits exchange"; SMA-312 §10 names the `SessionEvent` migration explicitly. Safe to bundle: SMA-312 just merged with no downstream consumers. |
| `Item` shape | **Five top-level variants** (`UserMessage`, `AssistantMessage`, `ToolCall`, `ToolResult`, `System`) **plus six content-part variants** (`Text`, `Image`, `Audio`, `ToolUse`, `ToolResult`, `Reasoning`). OpenAI/Anthropic duplication on tool-call/tool-result is intentional. | Both wire formats round-trip without lossy translation in provider crates. |
| Media representation | **`MediaSource::{Url, Base64}`** with explicit `mime_type` on `Base64`. | OpenAI Chat Completions accepts URLs; Anthropic Messages requires base64; both shapes round-trip. |
| `AgentEvent` semantic-item carrier | **`AgentEvent::{MessageOutput, ToolCallItem, ToolOutputItem}` carry `item: Item`** (instead of dedicated payload types). Doc comments call out which `Item` variant is expected. | Reuses `Item` as the single canonical content carrier; avoids parallel hierarchies. The mild loss of compile-time guarantee (any `Item` variant fits the field) is acceptable for the simpler type lattice. |
| `RunResult<T>` default | **`T = String`**; the typed path is `RunResult::<String>::parse_final::<T>()`. | Keeps `Runner::run` object-safe (no method-level generic); makes the common case ergonomic; satisfies AC #2 via a compile test. |
| `RunResult` `Default` | `#[derive(Default)]` with no explicit bound; relies on `String: Default` (and any other `T: Default` users pick). | One-liner that works for the default case; structured-output users provide their own initial value. |
| `Runner::run` signature | **Unchanged from SMA-312.** Returns `Result<RunResult, RunError>` which now means `Result<RunResult<String>, RunError>`. | Object-safety preserved (no method-level generic in a `dyn Runner<_>` vtable). Structured-output integration with `Runner` deferred to a follow-up. |
| `TokenUsage` shape | Five `u64` fields — `input_tokens`, `output_tokens`, `cached_input_tokens`, `reasoning_tokens`, `total_tokens`. `#[non_exhaustive]`. `add(&mut self, other)` for per-turn aggregation. | Covers OpenAI prompt-caching and o-series reasoning tokens, Anthropic prompt-caching and extended-thinking tokens. `u64` for cross-architecture serde safety (same rationale as `SessionEvent::Compacted::original_count` in SMA-312). |
| `RunContext` field shapes | `Arc<Ctx>`, `Arc<dyn Session>`, `HookRegistry<Ctx>`, `TracerHandle`, `CancellationToken`. Private fields; `pub fn new(...)` constructor; `pub fn user_ctx()` / `session()` / `hooks()` / `tracer()` / `cancel()` accessors. | `Arc<dyn Session>` because sessions are shared across the run; `HookRegistry<Ctx>` owns its hooks. Private fields so the struct can grow without breaking struct-literal construction. |
| `ToolContext` is narrower | `user_ctx: Arc<Ctx>`, `tracer: TracerHandle`, `cancel: CancellationToken`. **No** session, **no** hooks. Built via `RunContext::to_tool_context(&self)`. | Tools must not bypass the runner's persistence by directly appending to the session log; hooks fire *around* tool invocations, not from inside them. The narrower context enforces the invariant by construction. |
| `TracerHandle` shape today | Unit struct with `_private: ()`. `Default` + `Clone`. | Lets `RunContext` / `ToolContext` signatures resolve; gains real fields with the observability ticket. |
| `HookRegistry<Ctx>` shape today | `Vec<Arc<dyn Hook<Ctx>>>` with `new` / `push` / `iter` / `is_empty` / `Default`. | Minimum surface needed for the agent loop to iterate registered hooks. |
| `SessionEvent` migration shape | `UserMessage { content: Vec<ContentPart> }`, `AssistantMessage { content: Vec<ContentPart>, agent: String }`, `ToolReturned { call_id, content: Vec<ContentPart> }`. `ToolCalled`, `HandoffOccurred`, `Compacted` unchanged. | Variants carry `Vec<ContentPart>` directly (not `Item`) because the variant *is* the role — wrapping `Item::UserMessage` inside `SessionEvent::UserMessage` would double-tag. |
| `ConversationSnapshot` shape | `messages: Vec<Item>`. `#[non_exhaustive]` retained. | Single canonical projection a session can return; consumers iterate `Item`s without rebuilding from `SessionEvent`s. |
| JSON Schema dep | `schemars` (already pinned `= "1"` in workspace deps from SMA-304); `JsonSchema` derived on `Item`, `ContentPart`, `MediaSource` (not `AgentEvent` — see §6). Wire into `paigasus-helikon-core`'s `[dependencies]` as `schemars = { workspace = true }`. | `JsonSchema` on `Item` enables future schema-generation for structured-output contracts. SMA-304 already pinned `schemars = "1"`; no workspace-deps edit needed. |
| Snapshot testing | Add `insta = "1"` to workspace deps; reference in `paigasus-helikon-core`'s `[dev-dependencies]` as `insta = { workspace = true, features = ["yaml", "json"] }`. | Industry-standard Rust snapshot tester. Apache-2.0 (already on the deny.toml allowlist). |
| Serde tagging | `#[serde(tag = "type", rename_all = "snake_case")]` on every new enum (`Item`, `ContentPart`, `MediaSource`, `AgentEvent`) and on the migrated `SessionEvent`. | Consistent wire format; matches SMA-312's existing `SessionEvent` tagging. |

## 3. Files added / modified

### Added

| Path | Purpose |
|---|---|
| `crates/paigasus-helikon-core/src/item.rs` | `Item`, `ContentPart`, `MediaSource` |
| `crates/paigasus-helikon-core/tests/serde_roundtrip.rs` | AC #1 lock — snapshot + re-serialize round-trip per variant |
| `crates/paigasus-helikon-core/tests/compile_run_result_typed.rs` | AC #2 lock — `RunResult<MyStruct>` direct construction + `parse_final::<MyStruct>()` |
| `crates/paigasus-helikon-core/tests/snapshots/*.snap` | ~33 `insta` snapshot files (one per round-trip variant) |
| `docs/superpowers/specs/2026-05-21-sma-313-concrete-shared-types-design.md` | This design |

### Modified

| Path | Change |
|---|---|
| `crates/paigasus-helikon-core/src/lib.rs` | Add `pub mod item;` and `pub use item::*;` |
| `crates/paigasus-helikon-core/src/agent.rs` | Replace 4-variant stub `AgentEvent` with the 14-variant ADT; derive `Serialize` / `Deserialize` |
| `crates/paigasus-helikon-core/src/context.rs` | `RunContext` with real fields; add `HookRegistry<Ctx>` and `TracerHandle` |
| `crates/paigasus-helikon-core/src/tool.rs` | `ToolContext` with real fields; remove `PhantomData` placeholder |
| `crates/paigasus-helikon-core/src/runner.rs` | `RunResult<T = String>` with fields; add `TokenUsage`; `RunResult<String>::parse_final::<T>()` helper |
| `crates/paigasus-helikon-core/src/session.rs` | Graduate `UserMessage` / `AssistantMessage` / `ToolReturned` to `Vec<ContentPart>`; `ConversationSnapshot.messages: Vec<Item>` |
| `crates/paigasus-helikon-core/src/guardrail.rs` | Add `Serialize` + `Deserialize` derives to `GuardrailKind` so `AgentEvent::GuardrailTriggered` round-trips. See §11. |
| `crates/paigasus-helikon-core/tests/object_safety.rs` | Update trivial impls to use the new `RunContext` / `ToolContext` constructors; the dyn-cast assertions themselves are unchanged |
| `crates/paigasus-helikon-core/Cargo.toml` | Add `schemars = { workspace = true }` to `[dependencies]`; add `insta`, `schemars`, `serde_json` to `[dev-dependencies]` |
| `Cargo.toml` (workspace root) | Add `insta = "1"` to `[workspace.dependencies]`. `schemars = "1"` already pinned from SMA-304 — no change. |

### Not modified

- **Facade crate `paigasus-helikon`** — re-exports `paigasus-helikon-core` per SMA-304; new surface flows through automatically.
- **`CLAUDE.md`** — no new non-obvious convention. The "object-safety preserved by default-type-parameter on return type" technique is documented in this spec but doesn't warrant a top-level callout yet.
- **`.github/workflows/*` and `.github/rulesets/main-protection-checks.json`** — no new CI gates; existing `test` job runs the new snapshot tests and compile tests.
- **`deny.toml`** — no new license categories. `schemars` is MIT/Apache-2.0; `insta` is Apache-2.0; both already on the allowlist.
- **`Runner` trait signature** — unchanged from SMA-312. `Runner::run`'s return type changes meaning (now `RunResult<String>` instead of unit `RunResult`) but the source-level signature is identical.

## 4. Module layout (post-change)

```
crates/paigasus-helikon-core/
├── Cargo.toml
├── src/
│   ├── lib.rs             # adds: pub mod item; pub use item::*;
│   ├── agent.rs           # AgentEvent: 4 -> 14 variants
│   ├── context.rs         # RunContext: PhantomData -> real fields; +HookRegistry, +TracerHandle
│   ├── guardrail.rs       # unchanged
│   ├── hook.rs            # unchanged
│   ├── item.rs            # NEW: Item, ContentPart, MediaSource
│   ├── model.rs           # unchanged
│   ├── runner.rs          # RunResult -> RunResult<T = String>; +TokenUsage; +parse_final
│   ├── session.rs         # SessionEvent String -> Vec<ContentPart>; ConversationSnapshot.messages
│   └── tool.rs            # ToolContext: PhantomData -> real fields
└── tests/
    ├── compile_run_result_typed.rs   # NEW (AC #2)
    ├── object_safety.rs              # modified (trivial impls only)
    ├── serde_roundtrip.rs            # NEW (AC #1)
    └── snapshots/                    # NEW (insta)
        ├── ...item_user_message...snap
        ├── ...content_part_text...snap
        ├── ...agent_event_run_started...snap
        └── (~33 files total)
```

`lib.rs` re-exports stay flat:

```rust
pub mod agent;
pub mod context;
pub mod guardrail;
pub mod hook;
pub mod item;
pub mod model;
pub mod runner;
pub mod session;
pub mod tool;

pub use agent::*;
pub use context::*;
pub use guardrail::*;
pub use hook::*;
pub use item::*;
pub use model::*;
pub use runner::*;
pub use session::*;
pub use tool::*;
```

## 5. Type shapes

### 5.1 `Item`, `ContentPart`, `MediaSource` (`item.rs`)

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Canonical wire-format message. Superset of OpenAI Chat Completions,
/// OpenAI Responses, Anthropic Messages, and Bedrock Converse shapes.
///
/// `Item::ToolCall` and `Item::ToolResult` mirror OpenAI's sibling "tool"
/// role. Anthropic providers emit equivalent [`ContentPart::ToolUse`] and
/// [`ContentPart::ToolResult`] blocks nested inside `AssistantMessage` /
/// `UserMessage`. Both shapes round-trip cleanly through this type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Item {
    UserMessage {
        content: Vec<ContentPart>,
    },
    AssistantMessage {
        content: Vec<ContentPart>,
        /// Name of the agent that produced this message, when known.
        /// `Option` (not `String`) because the wire format can lose
        /// attribution — e.g. a raw provider response deserialized
        /// without any agent context. The session log keeps `String`
        /// because the runner always knows which agent emitted.
        ///
        /// `skip_serializing_if = "Option::is_none"` so the field is
        /// omitted from the JSON (not emitted as `"agent": null`) when
        /// unknown. Provider APIs conventionally treat omitted optional
        /// fields differently from explicit `null`.
        #[serde(skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    System {
        content: Vec<ContentPart>,
    },
    /// OpenAI-style sibling-role tool call.
    ToolCall {
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    /// OpenAI-style "tool" role response.
    ToolResult {
        call_id: String,
        content: Vec<ContentPart>,
    },
}

/// One content block inside an [`Item`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentPart {
    Text {
        text: String,
    },
    Image {
        source: MediaSource,
    },
    Audio {
        source: MediaSource,
    },
    /// Anthropic-style tool_use block nested inside an `AssistantMessage`.
    /// Equivalent to a top-level [`Item::ToolCall`].
    ToolUse {
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    /// Anthropic-style tool_result block nested inside a `UserMessage`.
    /// Equivalent to a top-level [`Item::ToolResult`]. The inner content
    /// is itself a `Vec<ContentPart>` because Anthropic permits text + image
    /// blocks inside a tool_result.
    ToolResult {
        call_id: String,
        content: Vec<ContentPart>,
    },
    /// Provider-emitted reasoning trace (e.g. Anthropic extended thinking,
    /// OpenAI reasoning summaries).
    Reasoning {
        text: String,
    },
}

/// Source of a multimedia content block.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MediaSource {
    Url {
        url: String,
    },
    Base64 {
        mime_type: String,
        data: String,
    },
}
```

### 5.2 `AgentEvent` — full ADT (`agent.rs`)

```rust
use serde::{Deserialize, Serialize};

use crate::{GuardrailKind, Item, TokenUsage};

/// The unified event stream emitted by an [`Agent`]. Fourteen variants
/// covering lifecycle, raw streaming deltas, post-aggregation semantic
/// items, agent transitions, control signals, and terminal outcomes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentEvent {
    // --- Lifecycle ---
    /// The run has started; the named agent is active.
    RunStarted { agent: String },
    /// A new turn (one model invocation + any tool calls) has begun.
    TurnStarted { turn: u32 },

    // --- Raw deltas (for low-latency UIs) ---
    /// An incremental assistant-text chunk.
    TokenDelta { text: String },
    /// An incremental reasoning-text chunk.
    ReasoningDelta { text: String },
    /// An incremental tool-call-arguments chunk.
    ToolCallDelta {
        call_id: String,
        name: Option<String>,
        args_delta: String,
    },

    // --- Semantic items (post-aggregation, carry Item) ---
    /// A complete assistant message. The inner [`Item`] is expected to be
    /// [`Item::AssistantMessage`].
    MessageOutput { item: Item },
    /// A complete tool call resolved during the turn. The inner [`Item`]
    /// is expected to be [`Item::ToolCall`].
    ToolCallItem { item: Item },
    /// A complete tool result returned by a tool. The inner [`Item`] is
    /// expected to be [`Item::ToolResult`].
    ToolOutputItem { item: Item },
    /// A handoff item recorded in the trajectory.
    HandoffItem { from: String, to: String },

    // --- Agent transitions ---
    /// The currently-active agent changed.
    AgentUpdated { agent: String },

    // --- Control ---
    /// A guardrail tripwire fired during the run.
    GuardrailTriggered {
        kind: GuardrailKind,
        info: serde_json::Value,
    },
    /// The runner is awaiting an approval decision before proceeding.
    ApprovalRequested {
        call_id: String,
        tool: String,
        args: serde_json::Value,
    },

    // --- Terminal ---
    /// The run finished normally.
    RunCompleted { usage: TokenUsage },
    /// The run finished with an error.
    RunFailed { error: String },
}
```

`GuardrailKind` already implements `Serialize` / `Deserialize` after SMA-312? It does not — SMA-312 derives only `Debug, Clone, PartialEq, Eq`. SMA-313 must add `Serialize, Deserialize` to `GuardrailKind` so `AgentEvent::GuardrailTriggered` can round-trip. This is captured in §3 as a `guardrail.rs` modification implied by the `agent.rs` change.

### 5.3 `RunContext<Ctx>`, `HookRegistry<Ctx>`, `TracerHandle` (`context.rs`)

```rust
use std::sync::Arc;

use crate::{CancellationToken, Hook, Session, ToolContext};

/// Carries the per-run state shared across the agent loop, tools,
/// guardrails, and hooks.
pub struct RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    user_ctx: Arc<Ctx>,
    session: Arc<dyn Session>,
    hooks: HookRegistry<Ctx>,
    tracer: TracerHandle,
    cancel: CancellationToken,
}

impl<Ctx> RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct a new [`RunContext`].
    pub fn new(
        user_ctx: Arc<Ctx>,
        session: Arc<dyn Session>,
        hooks: HookRegistry<Ctx>,
        tracer: TracerHandle,
        cancel: CancellationToken,
    ) -> Self {
        Self { user_ctx, session, hooks, tracer, cancel }
    }

    pub fn user_ctx(&self) -> &Arc<Ctx> { &self.user_ctx }
    pub fn session(&self) -> &Arc<dyn Session> { &self.session }
    pub fn hooks(&self) -> &HookRegistry<Ctx> { &self.hooks }
    pub fn tracer(&self) -> &TracerHandle { &self.tracer }
    pub fn cancel(&self) -> &CancellationToken { &self.cancel }

    /// Project the narrower [`ToolContext`] from this [`RunContext`].
    pub fn to_tool_context(&self) -> ToolContext<Ctx> {
        ToolContext::new(
            Arc::clone(&self.user_ctx),
            self.tracer.clone(),
            self.cancel.clone(),
        )
    }
}

/// Registry of hooks active for one run.
pub struct HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    hooks: Vec<Arc<dyn Hook<Ctx>>>,
}

impl<Ctx> HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    pub fn new() -> Self { Self { hooks: Vec::new() } }
    pub fn push(&mut self, hook: Arc<dyn Hook<Ctx>>) { self.hooks.push(hook); }
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Hook<Ctx>>> { self.hooks.iter() }
    pub fn is_empty(&self) -> bool { self.hooks.is_empty() }
}

impl<Ctx> Default for HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn default() -> Self { Self::new() }
}

/// Opaque handle to the per-run tracer. Field shape lands with the
/// observability ticket; today the struct exists so signatures resolve.
#[derive(Debug, Clone, Default)]
pub struct TracerHandle {
    _private: (),
}

pub use tokio_util::sync::CancellationToken;
```

`RunContext::new` is the public constructor. Default is **not** provided — a `RunContext` without a session handle is meaningless, and forcing the caller to provide one prevents silent zero-value pitfalls. The `Default` impl that exists today for the `PhantomData`-only `RunContext` is removed.

### 5.4 `ToolContext<Ctx>` (`tool.rs`)

```rust
use std::sync::Arc;

use crate::{CancellationToken, TracerHandle};

/// Narrower view of [`crate::RunContext`] passed to [`Tool::invoke`].
///
/// Deliberately excludes the session handle and hook registry: tools must
/// not bypass the runner's persistence by writing directly to the session
/// log, and hooks fire *around* tool invocations, not from inside them.
pub struct ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    user_ctx: Arc<Ctx>,
    tracer: TracerHandle,
    cancel: CancellationToken,
}

impl<Ctx> ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    pub fn new(
        user_ctx: Arc<Ctx>,
        tracer: TracerHandle,
        cancel: CancellationToken,
    ) -> Self {
        Self { user_ctx, tracer, cancel }
    }

    pub fn user_ctx(&self) -> &Arc<Ctx> { &self.user_ctx }
    pub fn tracer(&self) -> &TracerHandle { &self.tracer }
    pub fn cancel(&self) -> &CancellationToken { &self.cancel }
}
```

The previous `Default` impl is removed for the same reason as `RunContext` — a context with no real user data is a silent footgun.

### 5.5 `RunResult<T>` + `TokenUsage` (`runner.rs`)

```rust
use serde::{Deserialize, Serialize};

use crate::AgentEvent;

/// Aggregated outcome of a non-streaming [`Runner::run`].
///
/// Generic over the structured-output type. Default `T = String` makes
/// the common case ergonomic; structured-output callers build
/// `RunResult<MyStruct>` via [`RunResult::parse_final`].
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct RunResult<T = String> {
    /// The model's final assistant output, deserialized into `T`. For the
    /// default `T = String` this is the literal text.
    pub final_output: T,
    /// Every [`AgentEvent`] emitted during the run, in order.
    pub events: Vec<AgentEvent>,
    /// Aggregated token usage across every turn of the run.
    pub usage: TokenUsage,
}

impl RunResult<String> {
    /// Deserialize `final_output` into `T`, producing a typed
    /// [`RunResult`]. The `T: JsonSchema` bound is the marker that the
    /// caller has configured structured output upstream — without it,
    /// `parse_final` is just a JSON parse over unstructured text.
    pub fn parse_final<T>(self) -> Result<RunResult<T>, serde_json::Error>
    where
        T: serde::de::DeserializeOwned + schemars::JsonSchema,
    {
        let final_output = serde_json::from_str::<T>(&self.final_output)?;
        Ok(RunResult { final_output, events: self.events, usage: self.usage })
    }
}

/// Token usage aggregated across all turns of a run.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[non_exhaustive]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Cached input tokens (OpenAI prompt-caching, Anthropic prompt-caching).
    pub cached_input_tokens: u64,
    /// Reasoning tokens (OpenAI o-series, Anthropic extended thinking).
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    /// Add another usage record (per-turn aggregation across a run).
    pub fn add(&mut self, other: TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.total_tokens += other.total_tokens;
    }
}
```

The existing `Runner` trait, `RunConfig`, `RunResultStreaming`, and `RunError` definitions are unchanged. `RunResultStreaming` stays a `#[non_exhaustive]` placeholder until the runner ticket.

### 5.6 `SessionEvent` migration + `ConversationSnapshot` (`session.rs`)

```rust
use serde::{Deserialize, Serialize};

use crate::{ContentPart, Item};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionEvent {
    UserMessage {
        content: Vec<ContentPart>,
    },
    AssistantMessage {
        content: Vec<ContentPart>,
        agent: String,
    },
    ToolCalled {
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    ToolReturned {
        call_id: String,
        content: Vec<ContentPart>,
    },
    HandoffOccurred { from: String, to: String },
    Compacted {
        summary: String,
        /// `u64`: this value is serialized into the persisted event log,
        /// so a 32-bit consumer must read what a 64-bit producer wrote
        /// without truncation. (Preserved from SMA-312.)
        original_count: u64,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConversationSnapshot {
    pub messages: Vec<Item>,
}
```

`ToolCalled::args` stays `serde_json::Value` (not `Vec<ContentPart>`) because tool *arguments* are structured JSON payloads, not chat content. `ToolReturned::content` becomes `Vec<ContentPart>` because tool *outputs* can legitimately carry text + images (Anthropic's tool_result content shape).

## 6. Doc-comment rules for the new public surface

Every public item (enum variant, struct field, free function) carries a `///` doc comment so the workspace's `missing_docs = "warn"` lint and the doc-coverage script stay green. Three style decisions to keep consistency:

1. **One-line variant docs** — e.g. `/// The run has started.` Multi-paragraph descriptions live in the type-level doc, not the variant-level doc.
2. **Cross-link expected `Item` shapes** — `AgentEvent::MessageOutput`'s doc names `Item::AssistantMessage` as the expected inner variant. Same for `ToolCallItem` and `ToolOutputItem`. Runtime checks are intentionally absent; the docs are the contract.
3. **`// SMA-3xx —` markers** are reserved for cross-ticket references that genuinely won't be obvious from grep, not for routine TODOs. One lands in this ticket — on `TracerHandle::_private` (`// SMA-3xx — gains real fields with the observability ticket`).

`AgentEvent` derives `Serialize` and `Deserialize` but **not** `JsonSchema`. Reason: `AgentEvent` is an event stream, not a contract callers exchange with the model — `JsonSchema` on it would add coverage burden with no consumer. `Item`, `ContentPart`, and `MediaSource` do derive `JsonSchema` because the canonical wire format may show up in schema-generated structured-output contracts later.

## 7. Testing strategy

The crate has minimal *behavior* to test — only API shape and serde round-trip. Three test files cover the acceptance criteria; CI re-runs each on every PR via the existing `ci.yml` matrix.

### 7.1 `tests/serde_roundtrip.rs` (AC #1)

One test per round-tripped variant. Each test:

1. Constructs an instance of the variant with representative data.
2. Serializes to pretty JSON with `serde_json::to_string_pretty`.
3. Asserts the JSON via `insta::assert_snapshot!`.
4. Deserializes the JSON back to the type.
5. Re-serializes and asserts equality with step 2 (round-trip).

Variant coverage:

| Type | Variants | Count |
|---|---|---|
| `Item` | `UserMessage`, `AssistantMessage` (×2 — `Some(agent)` and `None`), `System`, `ToolCall`, `ToolResult` | 6 |
| `ContentPart` | `Text`, `Image`, `Audio`, `ToolUse`, `ToolResult`, `Reasoning` | 6 |
| `MediaSource` | `Url`, `Base64` | 2 |
| `AgentEvent` | 14 variants per §5.2 | 14 |
| `SessionEvent` | `UserMessage`, `AssistantMessage`, `ToolCalled`, `ToolReturned`, `HandoffOccurred`, `Compacted` | 6 |
| **Total** | | **34** |

The second `Item::AssistantMessage` test exercises the `agent: None` path and locks the `skip_serializing_if` wire shape — i.e. confirms the `"agent"` key is **omitted** from the JSON when unknown.

Snapshot files live in `tests/snapshots/` and are checked into git per `insta` convention. Reviewers diff the JSON shapes; the round-trip equality is a separate assertion (not snapshotted) so a JSON-formatting hiccup in `serde_json` doesn't masquerade as a semantic regression.

Skeleton:

```rust
//! Locks AC #1: every serializable variant round-trips through JSON.

use paigasus_helikon_core::*;
use serde_json::json;

#[test]
fn item_user_message_roundtrip() {
    let item = Item::UserMessage {
        content: vec![
            ContentPart::Text { text: "hello".into() },
            ContentPart::Image {
                source: MediaSource::Url { url: "https://example.com/cat.png".into() },
            },
        ],
    };
    let json = serde_json::to_string_pretty(&item).unwrap();
    insta::assert_snapshot!(json);

    let roundtripped: Item = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string_pretty(&roundtripped).unwrap();
    assert_eq!(json, json2);
}

// ... 32 more tests, one per variant ...
```

### 7.2 `tests/compile_run_result_typed.rs` (AC #2)

```rust
//! Locks AC #2: RunResult<MyStruct> compiles when
//! MyStruct: DeserializeOwned + JsonSchema, and round-trips via
//! RunResult::<String>::parse_final.

use paigasus_helikon_core::{RunResult, TokenUsage};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, PartialEq, Deserialize, JsonSchema)]
struct Answer {
    answer: u32,
}

#[test]
fn run_result_default_t_is_string() {
    let r: RunResult = RunResult {
        final_output: "hi".into(),
        events: vec![],
        usage: TokenUsage::default(),
    };
    assert_eq!(r.final_output, "hi");
}

#[test]
fn run_result_with_user_struct_compiles() {
    let _: RunResult<Answer> = RunResult {
        final_output: Answer { answer: 42 },
        events: vec![],
        usage: TokenUsage::default(),
    };
}

#[test]
fn parse_final_deserializes_json_output() {
    let from_runner = RunResult::<String> {
        final_output: r#"{"answer": 42}"#.into(),
        events: vec![],
        usage: TokenUsage::default(),
    };
    let typed: RunResult<Answer> = from_runner.parse_final::<Answer>().unwrap();
    assert_eq!(typed.final_output, Answer { answer: 42 });
}
```

### 7.3 `tests/object_safety.rs` (modified)

The trivial impls used to construct trait objects need updated `RunContext` / `ToolContext` calls (now they require `Arc<dyn Session>` + `HookRegistry<()>` + `TracerHandle` + `CancellationToken` instead of zero-arg `::new()`). The structure of the test (the `Box<dyn Trait>` and `Vec<Arc<dyn Trait>>` ascriptions) is unchanged — those are the actual AC locks from SMA-312.

The test file gains one tiny `NoopSession` impl to satisfy `Arc<dyn Session>` construction. Everything else stays as-shipped.

### 7.4 Verification commands (mirror `ci.yml`)

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features                              # snapshot + compile + object-safety
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
cargo msrv --path crates/paigasus-helikon-core verify
```

All six commands must exit 0 locally before requesting review.

## 8. Workspace dependency adds

One new pin in root `Cargo.toml` `[workspace.dependencies]`:

```toml
insta = "1"
```

- **`schemars = "1"`** — **already pinned** in `[workspace.dependencies]` from the SMA-304 bootstrap; no workspace edit needed. SMA-313 only wires it into `paigasus-helikon-core/Cargo.toml`'s `[dependencies]` block via `schemars = { workspace = true }`. License: MIT OR Apache-2.0. MSRV well below 1.75.
- **`insta = "1"`** — new workspace pin, major-version only per the existing convention (`serde = "1"`, `thiserror = "2"`, etc.). Used only as `[dev-dependencies]` of `paigasus-helikon-core` in this ticket. License: Apache-2.0. The `yaml` and `json` features are enabled at the per-crate dev-dep site (not the workspace pin); `yaml` selects the `.snap` format, `json` enables `insta::assert_json_snapshot!` for the rare case where YAML masks JSON-key-order issues.

`paigasus-helikon-core/Cargo.toml`:

```toml
[dependencies]
async-trait  = { workspace = true }
thiserror    = { workspace = true }
anyhow       = { workspace = true }
serde        = { workspace = true }
serde_json   = { workspace = true }
futures-core = { workspace = true }
tokio-util   = { workspace = true }
schemars     = { workspace = true }    # NEW

[dev-dependencies]
insta        = { workspace = true, features = ["yaml", "json"] }    # NEW
schemars     = { workspace = true }    # NEW (the JsonSchema bound on the compile test's struct)
serde_json   = { workspace = true }    # NEW (already in [dependencies] but reused in tests)

[lints]
workspace = true
```

`serde_json` already appears in `[dependencies]`; restating it in `[dev-dependencies]` is the Cargo idiom for "also used in tests" and is a no-op for the dep graph.

## 9. Out of scope (deferred to follow-ups)

| Item | Tracked in / lands with |
|---|---|
| `Item` flowing into `ModelRequest` / `ModelEvent` (e.g. `ModelRequest.messages: Vec<Item>`) | Provider tickets (OpenAI, Anthropic, Bedrock crates) |
| Runner-side `run_structured<T>` integration with `where Self: Sized` | Future runner ticket once the agent loop lands |
| Real fields on `RunConfig` (`max_iterations`, `retry_policy`, `tracing`, …) | Runner ticket |
| Real fields on `RunResultStreaming` (`events: BoxStream<AgentEvent>` + final-result future) | Runner ticket |
| Real fields on `AgentInput` (user text, attachments, previous-response handles) | Agent-loop ticket |
| Real fields on `TracerHandle` and the tracer surface itself | Observability ticket |
| `HookRegistry::remove` / `len` / index access | Agent-loop ticket (only `push` / `iter` / `is_empty` are needed in this one) |
| `LoopState` (the agent loop's typestate machine) | Agent-loop ticket |
| `PermissionMode` / `PermissionPolicy` / `PermissionDecision` | Permissions ticket |
| `Instructions<Ctx>` / `LlmAgent<Ctx, M>` / typestate builder | Concrete-agent ticket |
| Default backends — `MemorySession`, `SqliteSession`, `OpenAiModel`, `AnthropicModel` | Per-crate tickets |
| `#[tool]` proc macro | `paigasus-helikon-macros` macro ticket |
| `paigasus::schema::strict()` JSON-schema rewriter | Provider tickets / open question |

## 10. Commit shape

Single PR on `feature/sma-313-concrete-shared-types-item-agentevent-runcontext-runresult`. Implementation commit type: `feat(core): SMA-313 …`. release-plz bumps `paigasus-helikon-core` to its next pre-1.0 version on merge.

This design document lands on the same feature branch (not pre-merged to `main`) as `docs(spec): SMA-313 add design for concrete shared types`.

## 11. Risks and notes

- **Snapshot churn from variant-shape evolution.** ~33 snapshots is meaningful surface area. Mitigation: snapshots only capture prettified JSON; round-trip equality is a separate assertion. If a variant gains a field in a follow-up, only that one snapshot regenerates.
- **`Item::ToolCall` / `Item::ToolResult` looks redundant with `ContentPart::ToolUse` / `ContentPart::ToolResult`.** This is deliberate (OpenAI vs Anthropic wire-format split) and documented inline on every involved variant. Provider crates serialize the variant native to their wire format; the other is unused in their context but never goes away on the type.
- **`Runner::run` signature is "the same" but its return type *means* `RunResult<String>` now.** A consumer who previously wrote `let r: RunResult = runner.run(...).await?;` keeps compiling because the default type parameter kicks in. A consumer who writes `let r: RunResult<MyStruct> = runner.run(...).await?;` will *not* compile, and that's correct — they must go through `parse_final::<MyStruct>()`. The compile-test in `tests/compile_run_result_typed.rs` documents the supported path.
- **`SessionEvent` migration is a breaking change to a freshly-shipped enum.** Acceptable because SMA-312 merged on 2026-05-21 with no downstream consumers yet, and the migration is the explicit purpose of SMA-313. release-plz handles the pre-1.0 bump automatically.
- **`GuardrailKind` gains `Serialize` + `Deserialize` derives.** Needed for `AgentEvent::GuardrailTriggered` to round-trip. Minor surface-area expansion of a SMA-312 type — captured in §3 as an implied `guardrail.rs` modification.
- **`schemars 1` was pinned during SMA-304 bootstrap.** No workspace-deps edit in this ticket; SMA-313 only wires the dep into `paigasus-helikon-core`'s manifest. If schemars 2.0 ships before the workspace bumps, the migration is a separate chore — same playbook as the SMA-305 nightly toolchain pin.
- **`RunContext::new` has five required arguments.** Awkward at the call site. A builder pattern would help but is premature here; the agent-loop / Runner crates will introduce convenience constructors when their concrete agent / runner types know which defaults make sense.
- **Removing `Default` on `RunContext` / `ToolContext` is a (theoretical) source-compat break** from SMA-312, which derived `Default` on the `PhantomData`-only versions. Acceptable for the same reason as the `SessionEvent` migration: no downstream consumers yet.

## 12. Acceptance criteria (verification plan)

| AC | Lock |
|---|---|
| **AC #1** — Round-trip serde on all variants (snapshot tests) | `tests/serde_roundtrip.rs` (33 tests; one snapshot file per variant under `tests/snapshots/`). `cargo test --workspace --all-features` must exit 0. |
| **AC #2** — `RunResult<MyStruct>` compiles when `MyStruct: DeserializeOwned + JsonSchema` | `tests/compile_run_result_typed.rs`. `cargo test --workspace --all-features` must exit 0. |
| **Object-safety from SMA-312 still holds** | `tests/object_safety.rs` (modified to use new `RunContext` / `ToolContext` constructors). |
| **Workspace lints clean** | `cargo clippy --workspace --all-features --all-targets -- -D warnings` exits 0. |
| **Rustdoc clean** | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps` exits 0. |
| **Doc coverage ≥ 80%** | `DOC_COVERAGE_THRESHOLD=80 bash scripts/check-doc-coverage.sh` exits 0. |
| **MSRV holds** | `cargo msrv --path crates/paigasus-helikon-core verify` exits 0. `schemars 1` (already pinned) and `insta 1` both have MSRV ≤ 1.75. |
