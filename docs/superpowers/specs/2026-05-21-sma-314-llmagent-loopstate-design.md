# SMA-314 — `LlmAgent` + explicit `LoopState` state machine — design

- **Linear:** [SMA-314](https://linear.app/smaschek/issue/SMA-314/llmagent-explicit-loopstate-state-machine)
- **Branch:** `feature/sma-314-llmagent-explicit-loopstate-state-machine`
- **Status:** design (awaiting implementation plan)
- **Author:** Sven Maschek
- **Date:** 2026-05-21

## 1. Goal

Land the heart of the SDK: the concrete `LlmAgent<Ctx, M>` and the explicit `LoopState` state machine that drives one run. The loop is data, not a callback maze — per the ADR *"Explicit `LoopState` enum, not a callback maze"* this enables resumability, deterministic state-machine testing, and predictable streaming semantics for the unified `AgentEvent` stream from SMA-313.

The Linear ticket's acceptance criteria are:

1. Single-turn run on a fixture `MockModel` completes with `RunCompleted`.
2. Multi-turn run with tool calls emits the expected event sequence.
3. Two parallel tool calls execute concurrently (verifiable via instrumented `MockTool`).

This design meets all three. AC #1 and AC #2 are locked by `tests/loop_happy_path.rs` (§9); AC #3 is locked by `tests/loop_parallel_tools.rs` (§9). The pure transition function gets its own deterministic unit-test file (`tests/transition_unit.rs`) at the state-machine layer, independent of tokio.

### 1.1 Scope boundary (against peer tickets)

SMA-312 left four types as `#[non_exhaustive] struct Foo {}` placeholders. SMA-313 explicitly did not touch them. Linear sweep confirmed there is no upstream ticket landing them before SMA-314, but two **downstream** tickets own the bulk of the field shapes:

- **SMA-321 — TokioRunner (Backlog).** Enumerates `RunConfig` fields it owns: `max_turns`, `timeout`, `parallel_tool_call_limit`, `retry_policy`, `cancellation`.
- **SMA-316 / SMA-317 — OpenAI / Anthropic providers (Todo).** Will drive `ModelRequest` fields beyond messages + tools: `tool_choice`, `response_format`, `temperature`, `previous_response_id`.

SMA-314 graduates each placeholder **just far enough to drive the loop and lock the AC**, leaving every other field on the table for its respective downstream owner:

| Placeholder | What SMA-314 adds | What SMA-314 leaves alone |
|---|---|---|
| `RunConfig` | `max_turns: u32` (default `16`) | Everything else → SMA-321 |
| `ModelRequest` | `messages: Vec<Item>` + `tools: Vec<ToolDef>` + `model_settings: ModelSettings` | `tool_choice`, `response_format`, `temperature`, `previous_response_id` → SMA-316/317 |
| `AgentInput` | `messages: Vec<Item>` + `AgentInput::from_user_text` convenience | nothing — SMA-314 is the de-facto owner |
| `RunResultStreaming` | `events: BoxStream<'static, AgentEvent>` + `async fn collect(self) -> Result<RunResult, RunError>` | nothing — SMA-314 is the de-facto owner |

`LlmAgent::run` (the `Agent` trait impl) **contains the loop driver itself**. No concrete `Runner<Ctx>` impl ships in this ticket — that's SMA-321. The ticket scope's phrase "loop driver in the Runner" is loose terminology for "the runtime that drives transitions"; the actual code lives inside `LlmAgent::run` and the pure `transition` function. Tests drive the `BoxStream<AgentEvent>` directly with `#[tokio::test]`, no Runner needed.

## 2. Decisions and rationale

Eight decisions, scoped to the SMA-314 surface. Each row that says "per ADR" or "per ticket" is non-discretionary; the others are local choices made in this brainstorming pass.

| Decision | Choice | Rationale |
|---|---|---|
| Loop driver architecture | **Pure transition fn + thin async driver.** `fn transition(state: &LoopState, input: TransitionInput, ctx: &TransitionCtx) -> TransitionOutcome` is pure data-in/data-out (no async, no tokio, no IO). `LlmAgent::run` is a thin `async_stream::stream!` block that calls `transition`, awaits the `NextAction`, and feeds the result back. | Durable runners (Temporal / AgentCore in Stage 3 tickets) reuse the same `transition` function with their own driver. Resumability is real, not a slogan. State-machine determinism gets deterministic unit tests with zero async. The Notion ADR ("LoopState enum, not a callback maze") is honored at the *data shape* level either way; pure transitions go one further and honor it at the *control-flow* level too. |
| `Instructions<Ctx>` shape | **Trait + blanket impls.** `pub trait Instructions<Ctx>: Send + Sync { fn render(&self, ctx: &RunContext<Ctx>) -> String; }` with blanket impls for `String`, `&'static str`, and `F: Fn(&RunContext<Ctx>) -> String + Send + Sync`. LlmAgent holds `instructions: Arc<dyn Instructions<Ctx>>`. | Ergonomic: both `Arc::new("You are a helpful assistant.".to_string())` and `Arc::new(\|ctx\| format!("…{}", ctx.user_ctx().name))` work behind the same field type. Cost is one trait + three blanket impls + one doctest. |
| Driveable variants | **Happy path only.** `CallingModel`, `ExecutingTools`, `Done`, `Failed` are driven by `transition` for real; `ApplyingHandoff`, `Compacting`, `NeedsApproval` exist in the enum and are forward-compatible, but reaching them transitions to `Failed(AgentError::NotImplemented { feature })`. | Matches the SMA-314 AC scope. Hooks, permissions, agent-to-agent handoff, and session compaction each have their own downstream tickets; SMA-314 ships the *shape* of the state machine they will plug into. |
| Event stream primitive | **`async_stream::stream!` macro** for the driver, **`futures_util::future::join_all`** for parallel tool execution. | `async_stream::stream!` is executor-agnostic, no channel needed, supports inline `yield` for raw model deltas. `join_all` is the ticket-prescribed primitive. Both crates pull in nothing tokio-specific. |
| `ModelRequest` minimum shape | **`{ messages: Vec<Item>, tools: Vec<ToolDef>, model_settings: ModelSettings }`.** `ToolDef { name: String, description: String, schema: serde_json::Value }`. `ModelSettings` stays a `#[non_exhaustive] {}` placeholder. | Smallest shape the loop driver needs. Provider tickets (SMA-316/317) will add `tool_choice`, `response_format`, `temperature`, `previous_response_id` and the inside of `ModelSettings`. |
| `RunConfig` minimum shape | **Only `max_turns: u32`**, `Default { max_turns: 16 }`. | Ticket scope explicitly names "Bounded by `max_turns` (config in `RunConfig`)" and nothing else. SMA-321 owns the rest. Default 16 is a reasonable safety bound — high enough that legitimate multi-turn runs land inside it, low enough that runaway loops fail fast. |
| `RunResultStreaming` shape | **`{ events: BoxStream<'static, AgentEvent> }` with `async fn collect(self) -> Result<RunResult, RunError>`** that drains the stream, aggregates `TokenDelta` text into `final_output`, captures usage from terminal `RunCompleted`, surfaces `RunFailed` as `RunError`. | Avoids the trickier "stream + future co-existing in one struct" design. Raw streaming callers take `events` directly. The `RunResult` aggregation lives in `collect`, mirroring SMA-313's `RunResult<T>` shape. |
| Token usage path | **Stubbed.** `AgentEvent::RunCompleted { usage }` and `FinalOutput.usage` both report `TokenUsage::default()` (zeros) until SMA-316/317 add `ModelEvent::Usage`. `ModelEvent` is `#[non_exhaustive]`, so the future variant addition is a non-breaking change. | Avoids speculatively designing the model-level usage carrier without a real provider to validate against. The AC does not require usage accuracy. Documented as a known SMA-314 limitation in §13. |

## 3. Files added / modified

### Added

| Path | Purpose |
|---|---|
| `crates/paigasus-helikon-core/src/loop_state.rs` | `LoopState`, `NextAction`, `TransitionInput`, `TransitionOutcome`, `TransitionCtx`, `ToolCallRequest`, `ToolCallOutcome`, `FinalOutput`, the pure `transition(...)` function. |
| `crates/paigasus-helikon-core/tests/common/mod.rs` | Shared fixtures: `MockModel`, `MockTool`, `MockToolBarrier`, `NoopSession`, `noop_run_context()`. |
| `crates/paigasus-helikon-core/tests/transition_unit.rs` | Six pure-function unit tests on `transition(...)` — no tokio, no async. |
| `crates/paigasus-helikon-core/tests/loop_happy_path.rs` | AC #1 + AC #2 — `#[tokio::test]`s driving `LlmAgent::run`'s event stream. |
| `crates/paigasus-helikon-core/tests/loop_parallel_tools.rs` | AC #3 — `tokio::sync::Barrier`-based concurrent-tool verification. |
| `docs/superpowers/specs/2026-05-21-sma-314-llmagent-loopstate-design.md` | This design. |

### Modified

| Path | Change |
|---|---|
| `crates/paigasus-helikon-core/src/lib.rs` | Add `pub mod loop_state; pub use loop_state::*;`. |
| `crates/paigasus-helikon-core/src/agent.rs` | Add `LlmAgent<Ctx, M>` struct + `Agent` impl for it. Add `Instructions<Ctx>` trait + blanket impls. Promote `AgentInput { messages: Vec<Item> }` with `AgentInput::from_user_text(...)`. Add `OutputType { schema: schemars::Schema }`. Add `AgentError::MaxTurnsExceeded(u32)` and `AgentError::NotImplemented { feature: &'static str }`. |
| `crates/paigasus-helikon-core/src/model.rs` | Promote `ModelRequest { messages: Vec<Item>, tools: Vec<ToolDef>, model_settings: ModelSettings }`. Add `ToolDef { name, description, schema }` and `#[non_exhaustive] ModelSettings {}` (still a placeholder; field shape lands with SMA-316/317). |
| `crates/paigasus-helikon-core/src/runner.rs` | Promote `RunConfig { max_turns: u32 }` with `Default { max_turns: 16 }`. Promote `RunResultStreaming { events: BoxStream<'static, AgentEvent> }` with `async fn collect(self) -> Result<RunResult, RunError>`. |
| `crates/paigasus-helikon-core/tests/object_safety.rs` | Update trivial impls to use the new `AgentInput { messages: vec![] }` / `RunConfig::default()` etc. The `Box<dyn Trait>` ascriptions themselves are unchanged. |
| `crates/paigasus-helikon-core/Cargo.toml` | Add `futures-util` and `async-stream` to `[dependencies]`. Add `tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time", "sync"] }` to `[dev-dependencies]`. |
| `Cargo.toml` (workspace root) | Add `futures-util` and `async-stream` to `[workspace.dependencies]`. |

### Not modified

- **Facade crate `paigasus-helikon`** — re-exports `paigasus-helikon-core` unconditionally per SMA-304; the new surface flows through automatically.
- **`CLAUDE.md`** — no new non-obvious convention. The "pure transition + thin driver" pattern is documented in this spec but doesn't warrant a top-level callout yet.
- **`.github/workflows/*` and `.github/rulesets/main-protection-checks.json`** — no new CI gates; existing `test` job runs the new tests.
- **`deny.toml`** — no new license categories. `futures-util` is MIT/Apache-2.0; `async-stream` is MIT.
- **The `Runner<Ctx>` trait** — signature unchanged from SMA-312. SMA-314 ships no concrete `Runner` impl; SMA-321 does.
- **`ModelEvent` (the variants)** — SMA-313 shape preserved. Usage flows as zeros until SMA-316/317 add `ModelEvent::Usage` (a non-breaking variant addition because `ModelEvent` is `#[non_exhaustive]`).

## 4. Module layout (post-change)

```
crates/paigasus-helikon-core/
├── Cargo.toml
├── src/
│   ├── lib.rs             # adds: pub mod loop_state; pub use loop_state::*;
│   ├── agent.rs           # +LlmAgent, +Instructions, AgentInput shape, +OutputType,
│   │                      # +AgentError::{MaxTurnsExceeded, NotImplemented}
│   ├── context.rs         # unchanged
│   ├── guardrail.rs       # unchanged
│   ├── hook.rs            # unchanged
│   ├── item.rs            # unchanged
│   ├── loop_state.rs      # NEW: LoopState, NextAction, TransitionInput,
│   │                      # TransitionOutcome, TransitionCtx, ToolCallRequest,
│   │                      # ToolCallOutcome, FinalOutput, transition()
│   ├── model.rs           # ModelRequest shape, +ToolDef, +ModelSettings
│   ├── runner.rs          # RunConfig shape, RunResultStreaming shape
│   ├── session.rs         # unchanged
│   └── tool.rs            # unchanged
└── tests/
    ├── common/mod.rs              # NEW: MockModel, MockTool, MockToolBarrier,
    │                              # NoopSession, noop_run_context
    ├── compile_run_result_typed.rs  # unchanged (SMA-313)
    ├── loop_happy_path.rs         # NEW (AC #1, AC #2)
    ├── loop_parallel_tools.rs     # NEW (AC #3)
    ├── object_safety.rs           # modified (constructors only)
    ├── serde_roundtrip.rs         # unchanged (SMA-313)
    ├── snapshots/                 # unchanged (SMA-313)
    └── transition_unit.rs         # NEW (state-machine determinism)
```

`lib.rs` re-exports stay flat:

```rust
pub mod agent;
pub mod context;
pub mod guardrail;
pub mod hook;
pub mod item;
pub mod loop_state;       // NEW
pub mod model;
pub mod runner;
pub mod session;
pub mod tool;

pub use agent::*;
pub use context::*;
pub use guardrail::*;
pub use hook::*;
pub use item::*;
pub use loop_state::*;    // NEW
pub use model::*;
pub use runner::*;
pub use session::*;
pub use tool::*;
```

## 5. Type shapes — `loop_state.rs`

```rust
use crate::{
    AgentError, AgentEvent, ContentPart, FinishReason, Item, ModelRequest,
    ModelSettings, TokenUsage, ToolDef,
};

/// The explicit, observable state of the agent loop. One variant per
/// high-level phase. Per ADR *"Explicit `LoopState` enum, not a callback
/// maze"* the state machine is data, not control flow.
///
/// Does **not** derive `Clone`: `Failed(AgentError)` wraps
/// `anyhow::Error` (not `Clone`). The transition function takes input
/// and returns outcome by value; tests use `assert_matches!` on
/// `next_state` instead of equality.
#[derive(Debug)]
#[non_exhaustive]
pub enum LoopState {
    /// About to call the model for turn `turn`.
    CallingModel { turn: u32 },
    /// The model produced tool calls; about to execute them. `turn` is
    /// the turn that produced the calls — the next `CallingModel`
    /// state will be `turn + 1`.
    ExecutingTools { calls: Vec<ToolCallRequest>, turn: u32 },
    /// Handing off to another agent.
    ///
    /// **Not driveable in SMA-314.** Agent-to-Agent handoff machinery
    /// lands in a follow-up ticket. Reaching this variant from
    /// `transition` returns `LoopState::Failed(AgentError::NotImplemented
    /// { feature: "handoff" })`.
    ApplyingHandoff { target: String, transcript: Vec<Item> },
    /// Compacting session history.
    ///
    /// **Not driveable in SMA-314.** See `ApplyingHandoff` note.
    Compacting,
    /// Awaiting user or operator approval for a sensitive tool call.
    ///
    /// **Not driveable in SMA-314.** Permissions land in a follow-up ticket.
    NeedsApproval { pending: Vec<ToolCallRequest> },
    /// Terminal: run completed successfully.
    Done(FinalOutput),
    /// Terminal: run failed.
    Failed(AgentError),
}

/// One tool call the model has requested. Pure data.
#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    pub call_id: String,
    pub name: String,
    pub args: serde_json::Value,
}

/// Outcome of one tool execution. The error path is stringified so the
/// outcome implements `Clone` — `ToolError` carries `anyhow::Error`,
/// which is not `Clone`. The driver records the serialized error in the
/// conversation so the model can see it on the next turn.
#[derive(Debug, Clone)]
pub struct ToolCallOutcome {
    pub call_id: String,
    pub result: Result<Vec<ContentPart>, String>,
}

/// Final assistant content + aggregated token usage at run termination.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FinalOutput {
    pub content: Vec<ContentPart>,
    pub usage: TokenUsage,
}

impl FinalOutput {
    /// Concatenate all `ContentPart::Text` parts into one string. This is
    /// the canonical rendering that feeds `RunResult.final_output` when
    /// `T = String`.
    pub fn as_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

/// Data fed *into* the next `transition` call.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TransitionInput {
    /// Seeds the loop with the initial conversation.
    Start { messages: Vec<Item> },
    /// One model turn aggregated.
    ModelResponse {
        items: Vec<Item>,
        usage: TokenUsage,
        finish_reason: FinishReason,
    },
    /// All tool calls for one turn have completed.
    ToolResults { outcomes: Vec<ToolCallOutcome> },
}

/// Side effect the async driver must run before the next transition.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum NextAction {
    CallModel { request: ModelRequest },
    ExecuteTools { calls: Vec<ToolCallRequest> },
    Terminate,
}

/// What the pure `transition` function needs to know about the agent and
/// config for one step. Doesn't carry user `Ctx` — that's the async
/// driver's concern.
pub struct TransitionCtx<'a> {
    pub tools: &'a [ToolDef],
    pub model_settings: &'a ModelSettings,
    pub max_turns: u32,
    /// The driver's accumulated conversation. The driver appends incoming
    /// items (from `Start.messages`, `ModelResponse.items`, or
    /// stringified `ToolResults.outcomes`) to its own `Vec<Item>` before
    /// calling `transition` and passes the slice in. The transition
    /// function never mutates conversation — it reads.
    pub conversation: &'a [Item],
}

/// One transition step's result. Not `Clone` (carries `LoopState`).
#[derive(Debug)]
pub struct TransitionOutcome {
    pub next_state: LoopState,
    pub events: Vec<AgentEvent>,
    pub next_action: NextAction,
}

/// Pure state-machine step. **No async, no tokio, no IO.**
///
/// Given the current state and the most recent input, produce the next
/// state, the events to emit, and the side effect to perform. Resumable
/// by construction: a durable runner can persist `LoopState` plus the
/// accumulated conversation and rehydrate the loop at any transition
/// boundary.
pub fn transition(
    state: &LoopState,
    input: TransitionInput,
    ctx: &TransitionCtx<'_>,
) -> TransitionOutcome {
    // Implementation lives here. See §6 for the per-case behavior table.
}
```

## 6. Transition function behavior

The transition function is a 7×3 case table over `(LoopState, TransitionInput)`, but most cells are unreachable in practice. The reachable cells are:

| State | Input | Next state | Events emitted | Next action |
|---|---|---|---|---|
| `CallingModel { turn }` (with `turn < max_turns`) | `Start { messages }` | `CallingModel { turn }` (unchanged) | `[TurnStarted { turn }]` | `CallModel { request: ModelRequest { messages: conversation, tools, model_settings } }` |
| `CallingModel { turn }` (with `turn >= max_turns`) | any | `Failed(MaxTurnsExceeded(max_turns))` | `[RunFailed { error }]` | `Terminate` |
| `CallingModel { turn }` | `ModelResponse { items, usage, finish_reason }` — no tool calls | `Done(FinalOutput { content, usage })` where `content` is the last `AssistantMessage`'s content | `[MessageOutput { item } for each AssistantMessage, RunCompleted { usage }]` | `Terminate` |
| `CallingModel { turn }` | `ModelResponse { items, usage, finish_reason: ToolCalls }` — at least one `ToolCall` | `ExecutingTools { calls: <extracted ToolCallRequest list> }` | `[MessageOutput, ToolCallItem ×N]` | `ExecuteTools { calls }` |
| `ExecutingTools { calls }` | `ToolResults { outcomes }` | `CallingModel { turn: <next> }` | `[ToolOutputItem ×N, TurnStarted { turn: next }]` | `CallModel { request: <fresh request> }` |
| `ApplyingHandoff` / `Compacting` / `NeedsApproval` | any | `Failed(NotImplemented { feature })` | `[RunFailed { error }]` | `Terminate` |
| `Done(_)` / `Failed(_)` | any | unchanged | `[]` | `Terminate` |

The "next turn" calculation in the `ExecutingTools → CallingModel` transition is `turn + 1` where `turn` is the turn that originally produced the tool calls. The transition function carries `turn` forward via the `ExecutingTools { calls, turn }` variant declared in §5 — that's why the variant has both fields.

Unreachable cells (e.g. `(ExecutingTools, Start)`) return `Failed(AgentError::Other(anyhow!("invalid transition: {state:?} ← {input:?}")))` — a defensive catch-all that surfaces driver bugs as test failures rather than silent wedges.

## 7. `LlmAgent` + `Instructions` — `agent.rs`

```rust
use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;

use crate::{
    Agent, AgentError, AgentEvent, ContentPart, FinishReason, Guardrail,
    Hook, Item, LoopState, Model, ModelEvent, ModelRequest, ModelSettings,
    NextAction, OutputType, RunConfig, RunContext, TokenUsage, Tool,
    ToolCallOutcome, ToolCallRequest, ToolDef, TransitionCtx,
    TransitionInput, TransitionOutcome, transition,
};

/// Renders the system prompt for one turn of the loop.
///
/// Implemented for `String`, `&'static str`, and any
/// `Fn(&RunContext<Ctx>) -> String + Send + Sync`.
///
/// ```
/// # use std::sync::Arc;
/// # use paigasus_helikon_core::{Instructions, RunContext};
/// let a: Arc<dyn Instructions<()>> = Arc::new("You are a helpful assistant.".to_string());
/// let b: Arc<dyn Instructions<()>> = Arc::new(|_: &RunContext<()>| "Dynamic".into());
/// ```
pub trait Instructions<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    fn render(&self, ctx: &RunContext<Ctx>) -> String;
}

impl<Ctx> Instructions<Ctx> for String
where Ctx: Send + Sync + 'static,
{
    fn render(&self, _ctx: &RunContext<Ctx>) -> String { self.clone() }
}

impl<Ctx> Instructions<Ctx> for &'static str
where Ctx: Send + Sync + 'static,
{
    fn render(&self, _ctx: &RunContext<Ctx>) -> String { (*self).to_owned() }
}

impl<Ctx, F> Instructions<Ctx> for F
where
    Ctx: Send + Sync + 'static,
    F: Fn(&RunContext<Ctx>) -> String + Send + Sync,
{
    fn render(&self, ctx: &RunContext<Ctx>) -> String { (self)(ctx) }
}

/// The concrete LLM-driven agent. Implements [`Agent`].
///
/// Constructed via direct field assignment in SMA-314; the ergonomic
/// typestate builder lands in SMA-319.
pub struct LlmAgent<Ctx, M>
where
    Ctx: Send + Sync + 'static,
    M: Model + 'static,
{
    /// Agent identifier.
    pub name: String,
    /// One-line description used in agent registries and trace spans.
    pub description: String,
    /// System-prompt renderer. See [`Instructions`].
    pub instructions: Arc<dyn Instructions<Ctx>>,
    /// The model the agent calls each turn.
    pub model: Arc<M>,
    /// Tools the model may call. Each invocation snapshots these into
    /// `ModelRequest.tools` via [`ToolDef`].
    pub tools: Vec<Arc<dyn Tool<Ctx>>>,
    /// Candidate agents this one may hand off to. Field exists for
    /// forward-compatibility; SMA-314's transition function never
    /// emits a handoff.
    pub handoffs: Vec<Arc<dyn Agent<Ctx>>>,
    /// Structured-output type marker. SMA-320 makes this honest.
    pub output_type: Option<OutputType>,
    /// Pre-input guardrails. Stored but not driven in SMA-314.
    pub input_guardrails: Vec<Arc<dyn Guardrail<Ctx>>>,
    /// Post-output guardrails. Stored but not driven in SMA-314.
    pub output_guardrails: Vec<Arc<dyn Guardrail<Ctx>>>,
    /// Lifecycle hooks. Stored but not driven in SMA-314.
    pub hooks: Vec<Arc<dyn Hook<Ctx>>>,
    /// Provider tuning knobs (sampling, max tokens, ...). Fields land
    /// with SMA-316/317.
    pub model_settings: ModelSettings,
    /// Per-run config — at SMA-314, only `max_turns` is meaningful.
    pub config: RunConfig,
}

#[async_trait]
impl<Ctx, M> Agent<Ctx> for LlmAgent<Ctx, M>
where
    Ctx: Send + Sync + 'static,
    M: Model + 'static,
{
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }

    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        // Snapshot everything the stream needs — the stream may outlive
        // the borrow of `&self`.
        let model = Arc::clone(&self.model);
        let tools = self.tools.clone();
        let max_turns = self.config.max_turns;
        let model_settings = self.model_settings.clone();
        let agent_name = self.name.clone();
        let instructions_text = self.instructions.render(&ctx);
        let tool_defs: Vec<ToolDef> = tools
            .iter()
            .map(|t| ToolDef {
                name: t.name().to_owned(),
                description: t.description().to_owned(),
                schema: t.schema().clone(),
            })
            .collect();

        let stream = async_stream::stream! {
            // Seed conversation: optional system message + user input.
            let mut conversation: Vec<Item> = Vec::new();
            if !instructions_text.is_empty() {
                conversation.push(Item::System {
                    content: vec![ContentPart::Text { text: instructions_text }],
                });
            }
            conversation.extend(input.messages.iter().cloned());

            let mut loop_state = LoopState::CallingModel { turn: 0 };
            let mut tx_input = TransitionInput::Start { messages: input.messages };

            yield AgentEvent::RunStarted { agent: agent_name.clone() };

            loop {
                let tx_ctx = TransitionCtx {
                    tools: &tool_defs,
                    model_settings: &model_settings,
                    max_turns,
                    conversation: &conversation,
                };
                let TransitionOutcome { next_state, events, next_action } =
                    transition(&loop_state, tx_input, &tx_ctx);
                for ev in events { yield ev; }
                loop_state = next_state;

                match next_action {
                    NextAction::CallModel { request } => {
                        let mut model_stream = match model.invoke(request, ctx.cancel().clone()).await {
                            Ok(s) => s,
                            Err(e) => {
                                yield AgentEvent::RunFailed { error: e.to_string() };
                                return;
                            }
                        };

                        // Aggregate items inline; yield raw deltas as we go.
                        let mut text = String::new();
                        let mut reasoning = String::new();
                        let mut tool_accum: std::collections::HashMap<String, ToolCallAccum> =
                            std::collections::HashMap::new();
                        let mut finish_reason = FinishReason::Stop;

                        while let Some(evt) = futures_util::stream::StreamExt::next(&mut model_stream).await {
                            match evt {
                                Ok(ModelEvent::TokenDelta { text: t }) => {
                                    text.push_str(&t);
                                    yield AgentEvent::TokenDelta { text: t };
                                }
                                Ok(ModelEvent::ReasoningDelta { text: t }) => {
                                    reasoning.push_str(&t);
                                    yield AgentEvent::ReasoningDelta { text: t };
                                }
                                Ok(ModelEvent::ToolCallDelta { call_id, name, args_delta }) => {
                                    let a = tool_accum.entry(call_id.clone()).or_default();
                                    if let Some(n) = name.as_deref() { a.name = Some(n.into()); }
                                    a.args_str.push_str(&args_delta);
                                    yield AgentEvent::ToolCallDelta { call_id, name, args_delta };
                                }
                                Ok(ModelEvent::Finish { reason }) => { finish_reason = reason; }
                                Err(e) => {
                                    yield AgentEvent::RunFailed { error: e.to_string() };
                                    return;
                                }
                            }
                        }

                        let items = build_items(&agent_name, text, reasoning, tool_accum);
                        conversation.extend(items.iter().cloned());
                        // Usage stubbed until SMA-316/317 add ModelEvent::Usage.
                        let usage = TokenUsage::default();
                        tx_input = TransitionInput::ModelResponse { items, usage, finish_reason };
                    }
                    NextAction::ExecuteTools { calls } => {
                        let tool_ctx = ctx.to_tool_context();
                        let outcomes = run_tools_concurrent(&tools, &calls, &tool_ctx).await;
                        for o in &outcomes {
                            conversation.push(Item::ToolResult {
                                call_id: o.call_id.clone(),
                                content: o.result.clone().unwrap_or_else(|e| {
                                    vec![ContentPart::Text { text: e }]
                                }),
                            });
                        }
                        tx_input = TransitionInput::ToolResults { outcomes };
                    }
                    NextAction::Terminate => return,
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

/// Accumulates the in-progress tool call across `ToolCallDelta` chunks.
#[derive(Default)]
struct ToolCallAccum {
    name: Option<String>,
    args_str: String,
}

/// Reassemble streamed model output into `Item`s.
fn build_items(
    agent_name: &str,
    text: String,
    reasoning: String,
    tool_accum: std::collections::HashMap<String, ToolCallAccum>,
) -> Vec<Item> {
    let mut items = Vec::new();
    if !text.is_empty() || !reasoning.is_empty() {
        let mut content = Vec::new();
        if !reasoning.is_empty() {
            content.push(ContentPart::Reasoning { text: reasoning });
        }
        if !text.is_empty() {
            content.push(ContentPart::Text { text });
        }
        items.push(Item::AssistantMessage {
            content,
            agent: Some(agent_name.to_owned()),
        });
    }
    for (call_id, accum) in tool_accum {
        items.push(Item::ToolCall {
            call_id,
            name: accum.name.unwrap_or_default(),
            args: serde_json::from_str(&accum.args_str)
                .unwrap_or(serde_json::Value::Null),
        });
    }
    items
}

async fn run_tools_concurrent<Ctx>(
    tools: &[Arc<dyn Tool<Ctx>>],
    calls: &[ToolCallRequest],
    tool_ctx: &crate::ToolContext<Ctx>,
) -> Vec<ToolCallOutcome>
where Ctx: Send + Sync + 'static,
{
    let futures = calls.iter().map(|call| {
        let tool = tools.iter().find(|t| t.name() == call.name).cloned();
        let call_id = call.call_id.clone();
        let args = call.args.clone();
        let name = call.name.clone();
        async move {
            match tool {
                Some(t) => match t.invoke(tool_ctx, args).await {
                    Ok(output) => ToolCallOutcome {
                        call_id,
                        result: Ok(tool_output_to_content_parts(&output)),
                    },
                    Err(e) => ToolCallOutcome { call_id, result: Err(e.to_string()) },
                },
                None => ToolCallOutcome {
                    call_id,
                    result: Err(format!("unknown tool: {name}")),
                },
            }
        }
    });
    futures_util::future::join_all(futures).await
}

/// Conversion convention: `ToolOutput.content` (currently
/// `serde_json::Value` from SMA-313) becomes one `ContentPart::Text`.
/// `Value::String(s) -> ContentPart::Text { text: s }`; other JSON
/// shapes get `serde_json::to_string`-ified.
fn tool_output_to_content_parts(output: &crate::ToolOutput) -> Vec<ContentPart> {
    let text = match &output.content {
        serde_json::Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    vec![ContentPart::Text { text }]
}
```

`AgentInput` (also in `agent.rs`):

```rust
/// User-supplied input that seeds the run.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AgentInput {
    pub messages: Vec<Item>,
}

impl AgentInput {
    pub fn new() -> Self { Self::default() }

    /// Convenience: seed the run with one user text message.
    pub fn from_user_text(text: impl Into<String>) -> Self {
        Self {
            messages: vec![Item::UserMessage {
                content: vec![ContentPart::Text { text: text.into() }],
            }],
        }
    }
}

/// Structured-output type marker — the JSON Schema the model must produce.
/// SMA-320 promotes the typed-output path; this ticket only defines the
/// field type so [`LlmAgent`] has a place to store it.
#[derive(Debug, Clone)]
pub struct OutputType {
    pub schema: schemars::Schema,
}

impl OutputType {
    pub fn from_schema<T: schemars::JsonSchema>() -> Self {
        Self { schema: schemars::schema_for!(T) }
    }
}
```

New `AgentError` variants (also in `agent.rs`):

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentError {
    // ... existing SMA-312/313 variants ...

    /// `max_turns` budget exhausted (SMA-314).
    #[error("max turns ({0}) exceeded")]
    MaxTurnsExceeded(u32),

    /// A state-machine variant was reached that SMA-314 does not yet
    /// drive (handoff, compaction, approval).
    #[error("not yet implemented: {feature}")]
    NotImplemented { feature: &'static str },
}
```

## 8. `ModelRequest` / `RunConfig` / `RunResultStreaming` — promoted shapes

`model.rs`:

```rust
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ModelRequest {
    pub messages: Vec<Item>,
    pub tools: Vec<ToolDef>,
    pub model_settings: ModelSettings,
}

impl ModelRequest {
    pub fn new() -> Self { Self::default() }
}

/// Owned snapshot of a [`Tool`] for cross-async-boundary serialization
/// inside [`ModelRequest`].
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
}

/// Provider-tuning knobs (temperature, max tokens, sampling, …). Field
/// shape lands with SMA-316/317; today this is a `#[non_exhaustive]`
/// placeholder so `ModelRequest.model_settings` has a type.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ModelSettings {}

impl ModelSettings {
    pub fn new() -> Self { Self::default() }
}
```

`runner.rs`:

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunConfig {
    /// Maximum number of model turns before the loop fails with
    /// `AgentError::MaxTurnsExceeded`. Default `16`.
    pub max_turns: u32,
}

impl Default for RunConfig {
    fn default() -> Self { Self { max_turns: 16 } }
}

impl RunConfig {
    pub fn new() -> Self { Self::default() }
}

/// Streaming counterpart of [`RunResult`]. Wraps the event stream and
/// provides an `async fn collect` that drains to a `RunResult<String>`.
pub struct RunResultStreaming {
    /// The unified `AgentEvent` stream. Callers may consume it directly
    /// for raw streaming, or call [`RunResultStreaming::collect`] for
    /// the aggregated `RunResult`.
    pub events: futures_core::stream::BoxStream<'static, AgentEvent>,
}

impl RunResultStreaming {
    pub fn new(events: futures_core::stream::BoxStream<'static, AgentEvent>) -> Self {
        Self { events }
    }

    /// Drain the event stream and aggregate into a `RunResult<String>`.
    ///
    /// `final_output` is the concatenated `TokenDelta` text (the
    /// canonical string rendering). Structured-output callers go through
    /// `RunResult::<String>::parse_final::<T>()` (SMA-313).
    pub async fn collect(mut self) -> Result<RunResult, RunError> {
        use futures_util::stream::StreamExt;
        let mut events = Vec::new();
        let mut final_text = String::new();
        let mut usage = TokenUsage::default();
        let mut failed: Option<String> = None;

        while let Some(ev) = self.events.next().await {
            match &ev {
                AgentEvent::TokenDelta { text } => final_text.push_str(text),
                AgentEvent::RunCompleted { usage: u } => usage = *u,
                AgentEvent::RunFailed { error } => failed = Some(error.clone()),
                _ => {}
            }
            events.push(ev);
        }

        if let Some(e) = failed {
            return Err(RunError::Other(anyhow::anyhow!(e)));
        }
        Ok(RunResult { final_output: final_text, events, usage })
    }
}
```

`RunResultStreaming` does **not** itself implement `Stream` — keeping `events` as a public field means callers can pattern-match on the carrier struct ergonomically without an extra method-call hop.

## 9. Testing strategy

The crate gains four new test files plus a shared fixtures module. CI runs them via the existing `cargo test --workspace --all-features` matrix.

### 9.1 `tests/transition_unit.rs` — state-machine determinism

Six cases on `transition(...)` directly (no tokio, no async, fully deterministic):

1. **Start seeds turn 0.** `(CallingModel{0}, Start{messages: [user]})` → `next_action = CallModel{..}`, `next_state = CallingModel{0}`, `events = [TurnStarted{0}]`.
2. **Pure assistant response terminates.** `(CallingModel{0}, ModelResponse{items: [AssistantMessage], finish_reason: Stop})` → `next_state = Done(FinalOutput)`, `next_action = Terminate`, `events = [MessageOutput{..}, RunCompleted{..}]`.
3. **Tool calls fan out.** `(CallingModel{0}, ModelResponse{items: [AssistantMessage, ToolCall×2], finish_reason: ToolCalls})` → `next_state = ExecutingTools{calls, turn: 0}`, `next_action = ExecuteTools{calls}`, `events = [MessageOutput, ToolCallItem×2]`.
4. **Tool results re-enter the model.** `(ExecutingTools{calls, turn: 0}, ToolResults{outcomes×2})` → `next_state = CallingModel{1}`, `next_action = CallModel{..}`, `events = [ToolOutputItem×2, TurnStarted{1}]`.
5. **Max turns trips.** `(CallingModel{16}, Start{..})` with `max_turns = 16` → `next_state = Failed(MaxTurnsExceeded(16))`, `next_action = Terminate`, `events = [RunFailed{..}]`.
6. **Unreachable variant errors cleanly.** `(ApplyingHandoff{..}, _)` → `next_state = Failed(NotImplemented{"handoff"})`. Same shape for `Compacting` and `NeedsApproval`.

Each test uses `assert_matches!(outcome.next_action, NextAction::CallModel { .. })` and `assert!(matches!(outcome.next_state, ...))` — exact field shapes not asserted, so the internals can evolve without churning these tests.

### 9.2 `tests/loop_happy_path.rs` — AC #1 + AC #2

Two `#[tokio::test]`s:

```rust
#[tokio::test]
async fn single_turn_run_completes() {
    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::TokenDelta { text: "hello".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
    ]);
    let agent = build_agent(model, vec![]);
    let stream = agent.run(noop_run_context(), AgentInput::from_user_text("hi")).await.unwrap();
    let result = RunResultStreaming::new(stream).collect().await.unwrap();

    assert_eq!(result.final_output, "hello");
    assert!(matches!(result.events.last(), Some(AgentEvent::RunCompleted { .. })));
    assert!(result.events.iter().any(|e| matches!(e, AgentEvent::TokenDelta { .. })));
}

#[tokio::test]
async fn multi_turn_with_tool_call() {
    // Script: turn 0 = model emits one ToolCall; turn 1 = model emits final text.
    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta { call_id: "1".into(), name: Some("echo".into()), args_delta: "{\"msg\":\"hi\"}".into() },
            ModelEvent::Finish { reason: FinishReason::ToolCalls },
        ],
        vec![
            ModelEvent::TokenDelta { text: "done".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
    ]);
    let tool = MockTool::new("echo", serde_json::json!("ok"));
    let agent = build_agent(model, vec![tool.clone()]);
    let stream = agent.run(noop_run_context(), AgentInput::from_user_text("go")).await.unwrap();
    let result = RunResultStreaming::new(stream).collect().await.unwrap();

    assert_eq!(result.final_output, "done");
    assert_eq!(tool.invocations().len(), 1);

    let kinds: Vec<&str> = result.events.iter().map(event_kind).collect();
    insta::assert_yaml_snapshot!(kinds);  // snapshots the event-kind sequence
}
```

`event_kind` maps each `AgentEvent` to a short string for the snapshot — keeps the test forgiving of inner-field churn while still asserting ordering.

### 9.3 `tests/loop_parallel_tools.rs` — AC #3

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_tool_calls_execute_concurrently() {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let tool_a = MockToolBarrier::new("a", Arc::clone(&barrier));
    let tool_b = MockToolBarrier::new("b", Arc::clone(&barrier));

    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta { call_id: "1".into(), name: Some("a".into()), args_delta: "{}".into() },
            ModelEvent::ToolCallDelta { call_id: "2".into(), name: Some("b".into()), args_delta: "{}".into() },
            ModelEvent::Finish { reason: FinishReason::ToolCalls },
        ],
        vec![
            ModelEvent::TokenDelta { text: "done".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
    ]);
    let agent = build_agent(model, vec![tool_a, tool_b]);
    let stream = agent.run(noop_run_context(), AgentInput::from_user_text("go")).await.unwrap();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        RunResultStreaming::new(stream).collect(),
    )
    .await
    .expect("timeout — tools likely ran serially (Barrier deadlocked)")
    .unwrap();

    assert!(matches!(result.events.last(), Some(AgentEvent::RunCompleted { .. })));
}
```

`Barrier::new(2)` requires both invocations to arrive at `barrier.wait().await` before either proceeds. Serial execution deadlocks the first invocation; `tokio::time::timeout` surfaces that as a clear test failure. More robust than wall-clock comparisons (no flakes on slow CI runners).

### 9.4 `tests/common/mod.rs` — fixture shapes

```rust
pub struct MockModel {
    scripts: std::sync::Mutex<std::collections::VecDeque<Vec<ModelEvent>>>,
}
impl MockModel {
    pub fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> { /* … */ }
}
#[async_trait]
impl Model for MockModel {
    async fn invoke(&self, _request: ModelRequest, _cancel: CancellationToken)
        -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError>
    {
        let script = self.scripts.lock().unwrap().pop_front()
            .ok_or_else(|| ModelError::Other(anyhow::anyhow!("no more scripted responses")))?;
        Ok(Box::pin(futures_util::stream::iter(script.into_iter().map(Ok))))
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities { tools: true, parallel_tool_calls: true, ..Default::default() }
    }
}

pub struct MockTool {
    name: String,
    description: String,
    schema: serde_json::Value,
    invocations: std::sync::Mutex<Vec<(serde_json::Value, std::time::Instant)>>,
    output: serde_json::Value,
}
impl MockTool {
    pub fn new(name: &str, output: serde_json::Value) -> Arc<Self>;
    pub fn invocations(&self) -> Vec<(serde_json::Value, std::time::Instant)>;
}
#[async_trait]
impl<Ctx> Tool<Ctx> for MockTool where Ctx: Send + Sync + 'static { /* records + returns output */ }

pub struct MockToolBarrier {
    name: String,
    schema: serde_json::Value,
    barrier: Arc<tokio::sync::Barrier>,
}
impl MockToolBarrier {
    pub fn new(name: &str, barrier: Arc<tokio::sync::Barrier>) -> Arc<Self>;
}
#[async_trait]
impl<Ctx> Tool<Ctx> for MockToolBarrier where Ctx: Send + Sync + 'static {
    async fn invoke(&self, _ctx: &ToolContext<Ctx>, _args: serde_json::Value)
        -> Result<ToolOutput, ToolError>
    {
        self.barrier.wait().await;
        Ok(ToolOutput { content: serde_json::json!({"ok": true}) })
    }
    // ... name/description/schema accessors ...
}

pub struct NoopSession;
#[async_trait]
impl Session for NoopSession { /* append → Ok(()); events → Ok(vec![]); snapshot → Ok(default) */ }

pub fn noop_run_context<Ctx: Default + Send + Sync + 'static>() -> RunContext<Ctx> {
    RunContext::new(
        Arc::new(Ctx::default()),
        Arc::new(NoopSession),
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
}
```

`build_agent` is a small helper in each test file that wires `LlmAgent { name: "test", description: "", instructions: Arc::new(""), model, tools, …, config: RunConfig::default() }`.

### 9.5 Verification commands (mirror `ci.yml` job-for-job)

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
cargo msrv --path crates/paigasus-helikon-core verify
```

All six must exit 0 locally before requesting review.

## 10. Workspace dependency adds

Two new pins in root `Cargo.toml` `[workspace.dependencies]`:

```toml
futures-util = { version = "0.3", default-features = false, features = ["std"] }
async-stream = "0.3"
```

- **`futures-util`** — exports `join_all` (the ticket-prescribed primitive for parallel tool calls) and `StreamExt::next`. `default-features = false, features = ["std"]` keeps the surface minimum — no executor pull-in. MSRV well below 1.75. License: MIT/Apache-2.0.
- **`async-stream`** — provides the `stream!` macro for building `Stream<T>` from an `async` block with inline `yield`. Executor-agnostic (pure macro generating `Generator`-like code on top of stable Rust). MSRV well below 1.75. License: MIT.

**`tokio` is already pinned** in `[workspace.dependencies]` at `version = "1", features = ["full"]` from the SMA-304 bootstrap. SMA-314 only references it as a dev-dep in `paigasus-helikon-core/Cargo.toml`. The "full" feature is heavier than this ticket strictly needs (`macros`, `rt-multi-thread`, `time`, `sync` would suffice for our tests) but matching the workspace pin keeps things consistent — tightening the workspace-wide tokio feature set is a separate chore if anyone cares to.

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
schemars     = { workspace = true }
futures-util = { workspace = true }    # NEW
async-stream = { workspace = true }    # NEW

[dev-dependencies]
insta        = { workspace = true, features = ["yaml", "json"] }
schemars     = { workspace = true }
serde_json   = { workspace = true }
tokio        = { workspace = true, features = ["macros", "rt-multi-thread", "time", "sync"] }   # NEW
```

`deny.toml` requires no changes — both new licenses (MIT, Apache-2.0) are already on the allowlist from SMA-306.

## 11. Out of scope (deferred to follow-ups)

Each item below is named because the SMA-314 ticket text or the Notion ADRs reference it; we are deliberately not landing it here.

| Item | Tracked in / lands with |
|---|---|
| Concrete `Runner<Ctx>` impl (`TokioRunner` driving `Agent::run`) | **SMA-321** |
| Full `RunConfig` field set (`timeout`, `parallel_tool_call_limit`, `retry_policy`, `cancellation`) | **SMA-321** |
| Full `ModelRequest` field set (`tool_choice`, `response_format`, `temperature`, `previous_response_id`) | **SMA-316 / SMA-317** |
| Full `ModelSettings` field set | **SMA-316 / SMA-317** |
| Real token usage via `ModelEvent::Usage` variant | **SMA-316 / SMA-317** (non-breaking variant addition; `ModelEvent` is `#[non_exhaustive]`) |
| Typestate builder for `LlmAgent` | **SMA-319** |
| Honest `output_type::<T>()` (RunResult<T>::final_output is T, not serialized JSON in a String) | **SMA-320** |
| Agent-to-Agent handoff machinery (`ApplyingHandoff` transitions) | follow-up Stage-1 ticket |
| Session compaction (`Compacting` transitions) | follow-up Stage-1 ticket |
| Permissions / approval flow (`NeedsApproval` transitions) | follow-up Stage-1 ticket |
| Hook fan-out at lifecycle points | follow-up Stage-1 ticket |
| Input / output guardrail orchestration | follow-up Stage-1 ticket |
| OTel spans around the loop | **SMA-322** |
| Provider-specific schema rewriter (`paigasus::schema::strict()`) | open question; SMA-316/317 |

## 12. Commit shape

Single PR on `feature/sma-314-llmagent-explicit-loopstate-state-machine`. Implementation commit type: `feat(core): SMA-314 …`. release-plz bumps `paigasus-helikon-core` to its next pre-1.0 version on merge.

This design document lands on the same feature branch (not pre-merged to `main`) as `docs(spec): SMA-314 add design for LlmAgent + LoopState`.

## 13. Risks and notes

- **Usage zeros.** `AgentEvent::RunCompleted { usage }` and `FinalOutput.usage` will both report `TokenUsage::default()` until SMA-316/317 add `ModelEvent::Usage`. This is a documented limitation, not a bug. The MockModel fixtures do not exercise usage; integration tests assert event-stream shape, not usage accuracy.
- **`#[non_exhaustive]` cost on `LlmAgent`.** `LlmAgent` is **not** `#[non_exhaustive]` (unlike most other structs in this crate). That's deliberate: SMA-319 will add a typestate builder, which requires the ability to do struct-literal construction from outside the crate. Until 319 lands, callers in tests construct via plain field assignment.
- **`LoopState` is not `Clone`.** `AgentError` wraps `anyhow::Error` (not Clone) in its `Other` variant, so `LoopState::Failed(AgentError)` cannot derive Clone. The transition function therefore takes `TransitionInput` by value and returns `TransitionOutcome` by value (also not Clone, since it carries `LoopState`). The driver moves state and outcome through the loop without ever cloning. Unit tests use `assert_matches!(outcome.next_state, LoopState::Done(_))` instead of equality checks. `TransitionInput`, `NextAction`, `ToolCallRequest`, `ToolCallOutcome`, and `FinalOutput` *are* `Clone` — they contain no `AgentError`.
- **`Send` across stream boundaries.** The `async_stream::stream!` block holds `Arc<M>`, `Arc<dyn Tool<Ctx>>`s, `RunContext<Ctx>`, and various local state. Each must be `Send` for the resulting `BoxStream<'static, AgentEvent>` to compile. The SMA-312/313 trait bounds already require `Send + Sync` everywhere, so this is enforced by the type system at compile time — no extra discipline needed.
- **`ModelEvent::Finish` does not currently carry usage.** Future-compatibility note: when SMA-316/317 add `ModelEvent::Usage`, the loop's aggregation block in §7 gains one match arm. No other code changes.
- **`max_turns = 16` default.** Chosen because typical multi-turn agentic runs land in 3–8 turns; runaway loops fail fast. Users override per-run via `RunConfig { max_turns: N }`. SMA-321 may revise the default once timeout/budget machinery lands; today the value is local to this ticket.
- **Tokio `"full"` features in dev-deps.** The workspace pin enables tokio's `full` feature set. SMA-314's tests only need `macros`, `rt-multi-thread`, `time`, `sync` — narrowing this is a separate workspace-hygiene chore, not in scope. (See §10.)

## 14. Acceptance criteria (verification plan)

| AC | Lock |
|---|---|
| **AC #1** — Single-turn run on a fixture `MockModel` completes with `RunCompleted` | `tests/loop_happy_path.rs::single_turn_run_completes`. |
| **AC #2** — Multi-turn run with tool calls emits the expected event sequence | `tests/loop_happy_path.rs::multi_turn_with_tool_call` (insta-snapshotted event kind sequence). |
| **AC #3** — Two parallel tool calls execute concurrently | `tests/loop_parallel_tools.rs::two_tool_calls_execute_concurrently` (`tokio::sync::Barrier` + `tokio::time::timeout`). |
| **State-machine determinism (sanity)** | `tests/transition_unit.rs` — six pure-function unit tests. |
| **Object-safety preserved from SMA-312** | `tests/object_safety.rs` — modified constructors, same `Box<dyn Trait>` ascriptions. |
| **Workspace lints clean** | `cargo clippy --workspace --all-features --all-targets -- -D warnings` exits 0. |
| **Rustdoc clean** | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps` exits 0. |
| **Doc coverage ≥ 80%** | `DOC_COVERAGE_THRESHOLD=80 bash scripts/check-doc-coverage.sh` exits 0. |
| **MSRV holds** | `cargo msrv --path crates/paigasus-helikon-core verify` exits 0. `futures-util 0.3` and `async-stream 0.3` both have MSRV well below 1.75. |

Each command in §9.5 must exit 0 locally before requesting review on the implementation PR, matching the CI matrix job-for-job.
