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
- `Compacting` — a reserved phase, not yet driven.
- `NeedsApproval { pending }` — terminal. The run stops here when a tool call is deferred. See [Deferred tool calls](./permissions-guardrails-hooks.md#deferred-tool-calls).
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

A model turn can end with `FinishReason::Length` or `FinishReason::ContentFilter` and still hold tool calls. The provider cut that turn, so the tool call arguments can be incomplete. The loop fails the run with `AgentError::ToolCallsNotRun`. No tool call and no handoff from that turn runs. A stream that ended early with tool calls and no `Finish` event fails in the same way, with `finish_reason: None`. A custom `Model` must emit `Finish`.

## The event stream

`Agent::run` returns a `BoxStream<'static, AgentEvent>`. The outer `Result` only covers failure to *start* the stream; a fatal error mid-run arrives as an `AgentEvent::RunFailed` inside the stream, not as an `Err`.

`AgentEvent` is a single, serializable enum spanning lifecycle, raw deltas, post-aggregation semantic items, transitions, control signals, and terminal outcomes. The variants:

- Lifecycle: `RunStarted { agent }`, `TurnStarted { turn }`.
- Raw deltas (for low-latency UIs): `TokenDelta { text }`, `ReasoningDelta { text }`, `ToolCallDelta { call_id, name, args_delta }`.
  A provider whose wire format splits a tool name across several deltas buffers
  the fragments, so `name` is `Some` on the first delta the provider can
  establish the whole name from — usually the one carrying the first arguments
  chunk — and `None` on the rest. In those providers, a tool call that never
  carries arguments has its name emitted at end-of-stream.
- Semantic items (carry a full `Item`): `MessageOutput { item }`, `ToolCallItem { item }`, `ToolOutputItem { item }`, `HandoffItem { from, to }`.
- Transitions: `AgentUpdated { agent }`.
- Control: `GuardrailTriggered { kind, info }`, `ApprovalRequested { call_id, tool, args }` (the `args` are the effective arguments; they are redacted when output redaction is on), `PermissionDenied { tool, reason }`, `RepairStarted { attempt }`, `StructuredOutputFailed { schema_errors, final_text }`, `UserMessageInjected { content }`.
- Terminal: `RunCompleted { usage }`, `RunFailed { error }`, `RunDeferred { calls, usage }`, `RunStopped { reason, usage }`. A run ends with exactly one of these. `RunDeferred` means the run stopped on tool calls that wait for a host decision. `RunStopped` means the host stopped the run at a turn boundary through the turn checkpoint.

The raw `TokenDelta` / `ToolCallDelta` events stream as the provider emits them; the `MessageOutput` / `ToolCallItem` / `ToolOutputItem` events carry the same content re-aggregated into a complete `Item` once the turn settles.

A Gemini 3 turn that has only tool calls also yields a `MessageOutput`. Its message holds only provider data (see `Item::is_provider_data_only()`). It has no text. The core runner, the workflow agents, the CLI, AG-UI and the HTTP DTO text output skip such a message. A custom consumer that prints text must skip it too. A tool-only turn on `claude-sonnet-5-5` in which the model thinks gives the same kind of `MessageOutput`, because the thinking text of that model is empty. Adaptive thinking thinks only on some prompts.

## Driving a run to completion

The common case: call `run`, then drain the stream into a `RunResult` with `RunResultStreaming`.

```rust
use paigasus_helikon::core::{
    Agent, AgentInput, LlmAgent, RunContext, RunResultStreaming,
};
use paigasus_helikon::openai::OpenAiModel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let agent = LlmAgent::builder::<()>()
        .name("budget-assistant")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions("You are a budgeting assistant.")
        .build();

    let ctx: RunContext<()> = RunContext::ephemeral(());

    let input = AgentInput::from_user_text("How am I doing on my dining budget this month?");

    let stream = agent.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream).collect().await?;

    println!("{}", result.final_output);
    Ok(())
}
```

`RunResultStreaming::new(stream).collect().await?` drains every event, returning a `RunResult` whose `final_output: String` is the concatenated text of the *last* `MessageOutput` assistant message, alongside the full `events` vector and aggregated `usage`. A terminal `RunFailed` surfaces as `Err(RunError)`. For structured output, use `collect_typed::<T>()` instead. A run that ends with `RunDeferred` has no completed answer. For such a run, `collect` returns a `RunResult` with `StopReason::Deferred { calls }` in `stop_reason`. Its `final_output` can be non-empty: it keeps the text of an earlier `MessageOutput`, for example "I will ask for approval". That text is not a completed answer. Check `stop_reason` before you use `final_output`. Do not expect an empty string. `collect_typed` returns `AgentError::Incomplete`. A run that ends with `RunStopped` also has no completed answer. For such a run, `collect` returns `StopReason::Stopped { reason }` in `stop_reason`, and `collect_typed` returns `AgentError::Incomplete`. `RunResult.stop_reason` is `StopReason::Completed` for a run that finished.

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

## Turn-boundary checkpoint

A host can talk to a run while the run is in progress. For this, set a `TurnCheckpoint<Ctx>` on the `RunContext`. The loop calls it at each turn boundary. The checkpoint is optional. A `RunContext` without a checkpoint behaves as before.

A turn boundary is the point after the tool results of a turn and before the next model call. These points are not boundaries: the first model call, a structured-output final turn, a repair turn, a turn that ends the run with a final answer, and a turn that ends the run with `RunDeferred`. A handoff child loop has its own boundaries.

The checkpoint returns one `CheckpointDecision`:

- `Continue` — call the model as normal.
- `Inject(Vec<ContentPart>)` — add the parts as one user message, then call the model.
- `Stop { reason }` — end the run at this boundary.

```rust
use async_trait::async_trait;
use paigasus_helikon::core::{
    CheckpointDecision, ContentPart, RunContext, TurnBoundary, TurnCheckpoint,
};
use std::sync::{Arc, Mutex};

/// Hands out the newest message of a host inbox at each boundary.
struct InboxCheckpoint {
    inbox: Mutex<Vec<String>>,
}

#[async_trait]
impl TurnCheckpoint<()> for InboxCheckpoint {
    async fn at_boundary(
        &self,
        _ctx: &RunContext<()>,
        _boundary: &TurnBoundary,
    ) -> CheckpointDecision {
        match self.inbox.lock().unwrap().last().cloned() {
            Some(text) => CheckpointDecision::Inject(vec![ContentPart::Text { text }]),
            None => CheckpointDecision::Continue,
        }
    }
}

let checkpoint = Arc::new(InboxCheckpoint {
    inbox: Mutex::new(vec!["Use the savings account.".to_owned()]),
});
let ctx = RunContext::ephemeral(()).with_turn_checkpoint(checkpoint);
```

The checkpoint does not remove the item from the inbox. The host removes it when it sees `UserMessageInjected` in the event stream (see "Commit rule" below). If a guardrail rejects the item, or a cancel ends the run first, the item stays in the inbox. This example needs `Runner::run_streamed`. The stream gives `UserMessageInjected` to the host before the loop reaches the next boundary. `Runner::run` returns the events only when the run ends, so with `run` this checkpoint injects the same item again at each boundary.

### Event order

At a boundary, the loop emits the `ToolOutputItem` events of all tool results first. Then the checkpoint runs. On `Inject`, the loop emits `UserMessageInjected { content }`. Then it emits `TurnStarted` for the next turn. The injected user message always comes after the tool results. It never comes between a tool call and its result.

### Inject rules

- The parts must be user parts: `Text`, `Image` or `Audio`. A `ToolUse`, `ToolResult` or `Reasoning` part fails the run with `AgentError::InvalidInjection`.
- `Inject` with an empty list is the same as `Continue`.
- The input guardrails check the text of the injected content. A guardrail cannot check image or audio parts.
- A guardrail that returns an error (`Err`) counts as a tripwire. The run stops in the same way. An outage of a moderation service therefore stops every run that injects content.
- A guardrail tripwire ends the run with `RunStopped`, not with `RunFailed`. The loop does not emit `UserMessageInjected`. It does not store the rejected content.
- An injected message is one user message. A host with several waiting items merges them into one `Vec<ContentPart>`.
- `SessionRecorder` stores the message as a `SessionEvent::UserMessage`. Later runs of the session see it as a normal user message.

### Commit rule

The run owns the injected content only after `UserMessageInjected` is in the event stream. Before that event, the host owns the content. After that event, the host must not inject the content again. This is true also if the run ends before the model answers.

### Stop rules

`Stop` ends the run with `AgentEvent::RunStopped { reason, usage }` and `StopReason::Stopped`. It is not a failure. All tool calls of the last turn have results. The output guardrails do not run, and no final turn runs. `RunResultStreaming::collect` returns `Ok` with `stop_reason` set to `StopReason::Stopped { reason }` and with the `usage` of `RunStopped`. `final_output` keeps the text of the last assistant message, if there is one. That text is not a final answer, and it did not pass the output guardrails. `collect_typed` returns `AgentError::Incomplete { stop_reason }`.

A guardrail stop has a `GuardrailTriggered` event before `RunStopped`, and no `UserMessageInjected` between them. The text of `reason` alone does not identify a guardrail stop.

`Stop` is best effort. A run that ends with a final answer has no next boundary. A `Stop` decision cannot apply to it. The checkpoint does not run when the next turn would exceed `RunConfig::max_turns`. In that case the run fails with `AgentError::MaxTurnsExceeded`.

### Rules for the checkpoint code

1. Injected content has the trust of a user message. It has more trust than a tool result.
2. Do not write to `ctx.session()` from the checkpoint. The recorder appends the events of the run when the run ends.
3. The wait in `at_boundary` counts against `RunConfig::timeout`. The loop has no separate timeout for it.
4. The runner can drop the stream while `at_boundary` waits. Then the future is dropped. Write `at_boundary` so that it is cancel-safe.
5. Do not cancel the run from the checkpoint. A cancel ends the run as `Cancelled`. Return `Stop` instead.
6. `TokioRunner` stores the session on a best-effort basis. If the store fails, the injected content can be missing from the session, although `UserMessageInjected` was in the stream. A host that needs a guarantee must check the stored session before it delivers the content again.

### Where the checkpoint does not run

- `TemporalRunner` ignores the checkpoint and logs a warning.
- The HTTP runtimes (axum, actix and agentcore) clear the checkpoint on the context that they build. They log a warning if one was set.
- Workflow, swarm and graph sub-agents do not call the checkpoint.
- `AgentAsTool` sub-runs do not call the checkpoint.
- The checkpoint does not run inside a tool call or before the first model call of a run.
