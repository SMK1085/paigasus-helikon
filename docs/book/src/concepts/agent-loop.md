# Agent Loop & State Machine

The agent loop is an explicit `enum LoopState`, driven by a pure `transition` function and surfaced as a typed `AgentEvent` stream. The state machine is *data*: `transition` is `async`-free, allocation-light, and never touches IO. An async driver runs the side effects between transitions. This is what makes the loop resumable — a durable runner can persist the state plus the conversation and rehydrate at any transition boundary.

All of this lives in [`paigasus-helikon-core`](https://github.com/SMK1085/paigasus-helikon/tree/main/crates/paigasus-helikon-core).

## The state machine

`LoopState` has one variant per high-level phase:

- `CallingModel { turn, usage }` — about to invoke the model for `turn`.
- `ExecutingTools { calls, turn, usage }` — the model requested tool calls; run them concurrently.
- `ApplyingHandoff { target, transcript, usage }` — delegate to another agent, threading the transcript.
- `Finalizing { turn, usage }` — a constrained turn that asks the model to emit the configured `output_type`.
- `RepairingOutput { turn, usage }` — the one allowed repair turn after a failed structured-output validation.
- `Compacting` and `NeedsApproval { pending }` — reserved phases, not yet driven.
- `Done(FinalOutput)` — terminal success.
- `Failed(AgentError)` — terminal failure.

`transition` is the whole machine:

```rust
pub fn transition(
    state: &LoopState,
    input: TransitionInput,
    ctx: &TransitionCtx<'_>,
) -> TransitionOutcome;
```

It takes the current state, the most recent `TransitionInput` (`Start`, `ModelResponse`, or `ToolResults`), and a `TransitionCtx` (the tool defs, `ModelSettings`, `max_turns`, the accumulated `conversation`, the optional `OutputType`, and any handoff descriptors). It returns a `TransitionOutcome`:

```rust
pub struct TransitionOutcome {
    pub next_state: LoopState,
    pub events: Vec<AgentEvent>,
    pub next_action: NextAction,
    pub conversation_appends: Vec<Item>,
}
```

`next_action` tells the driver what side effect to run before the next step:

- `CallModel { request }` — call the model, feed the result back as `TransitionInput::ModelResponse`.
- `ExecuteTools { calls }` — fan out the tool calls, feed outcomes back as `TransitionInput::ToolResults`.
- `Handoff` — read the target/transcript from `ApplyingHandoff` and run the target agent.
- `Terminate` — the state is terminal; stop driving.

A typical run walks `CallingModel → ExecutingTools → CallingModel → … → Done`. Each tool round bumps `turn`; reaching `RunConfig::max_turns` fails the run with `AgentError::MaxTurnsExceeded`. With an `output_type` configured, the loop adds a `Finalizing` (and, on a validation miss, one `RepairingOutput`) turn before `Done`. See [Structured Output](./structured-output-builder.md) for that path.

## The event stream

`Agent::run` returns a `BoxStream<'static, AgentEvent>`. The outer `Result` only covers failure to *start* the stream; a fatal error mid-run arrives as an `AgentEvent::RunFailed` inside the stream, not as an `Err`.

`AgentEvent` is a single, serializable enum spanning lifecycle, raw deltas, post-aggregation semantic items, transitions, control signals, and terminal outcomes. The variants:

- Lifecycle: `RunStarted { agent }`, `TurnStarted { turn }`.
- Raw deltas (for low-latency UIs): `TokenDelta { text }`, `ReasoningDelta { text }`, `ToolCallDelta { call_id, name, args_delta }`.
- Semantic items (carry a full `Item`): `MessageOutput { item }`, `ToolCallItem { item }`, `ToolOutputItem { item }`, `HandoffItem { from, to }`.
- Transitions: `AgentUpdated { agent }`.
- Control: `GuardrailTriggered { kind, info }`, `ApprovalRequested { call_id, tool, args }`, `PermissionDenied { tool, reason }`, `RepairStarted { attempt }`, `StructuredOutputFailed { schema_errors, final_text }`.
- Terminal: `RunCompleted { usage }`, `RunFailed { error }`.

The raw `TokenDelta` / `ToolCallDelta` events stream as the provider emits them; the `MessageOutput` / `ToolCallItem` / `ToolOutputItem` events carry the same content re-aggregated into a complete `Item` once the turn settles.

## Driving a run to completion

The common case: call `run`, then drain the stream into a `RunResult` with `RunResultStreaming`.

```rust
use std::sync::Arc;
use paigasus_helikon::core::{
    Agent, AgentInput, CancellationToken, HookRegistry, LlmAgent, MemorySession,
    RunContext, RunResultStreaming, TracerHandle,
};
use paigasus_helikon::openai::OpenAiModel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let agent = LlmAgent::builder::<()>()
        .name("budget-assistant")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions("You are a budgeting assistant.")
        .build();

    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::<()>::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );

    let input = AgentInput::from_user_text("How am I doing on my dining budget this month?");

    let stream = agent.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream).collect().await?;

    println!("{}", result.final_output);
    Ok(())
}
```

`RunResultStreaming::new(stream).collect().await?` drains every event, returning a `RunResult` whose `final_output: String` is the concatenated text of the *last* `MessageOutput` assistant message, alongside the full `events` vector and aggregated `usage`. A terminal `RunFailed` surfaces as `Err(RunError)`. For structured output, use `collect_typed::<T>()` instead.

## Consuming events live

To react as the run unfolds — for example streaming tokens to a console — match on the stream directly instead of collecting:

```rust
use futures_util::StreamExt;
use paigasus_helikon::core::AgentEvent;

let mut stream = agent.run(ctx, input).await?;
while let Some(event) = stream.next().await {
    match event {
        AgentEvent::TokenDelta { text } => print!("{text}"),
        AgentEvent::RunFailed { error } => anyhow::bail!("run failed: {error}"),
        _ => {}
    }
}
```

You can drain the stream yourself and still build a `RunResult` later, or mix the two — `RunResultStreaming::collect` is just one consumer of the same `AgentEvent` stream.

## Tuning the loop

`RunConfig` controls the run; pass it through a `Runner`, or set it on the agent via the builder. The knobs:

- `max_turns: u32` — model turns before the loop fails with `MaxTurnsExceeded`. Default `16`. Honored by the core loop driver, including a bare `agent.run()`.
- `parallel_tool_call_limit: Option<NonZeroUsize>` — cap on concurrently-executing tool calls. `None` = unbounded. Honored by the core loop driver.
- `max_agent_depth: u32` — maximum agent-nesting depth across handoff chains and agent-as-tool sub-runs. Default `8`; exceeding it fails with `MaxAgentDepthExceeded`.
- `timeout: Option<Duration>` — wall-clock deadline for the whole run. Honored *only* by a runtime backend such as the [`runtime-tokio`](../reference/crates.md) runner; a bare `agent.run()` has no timer and cannot time out.

The builders are chainable:

```rust
use std::num::NonZeroUsize;
use std::time::Duration;
use paigasus_helikon::core::RunConfig;

let config = RunConfig::new()
    .with_timeout(Duration::from_secs(30))
    .with_parallel_tool_call_limit(NonZeroUsize::new(4).unwrap())
    .with_max_agent_depth(4);
```

Cancellation is *not* a `RunConfig` field — the canonical `CancellationToken` lives on `RunContext`. Cancellation and timeout are best-effort: a genuine terminal event that already occurred wins over a late cancel or timeout, so a caller cannot assume that calling `cancel()` always yields `RunError::Cancelled`.
