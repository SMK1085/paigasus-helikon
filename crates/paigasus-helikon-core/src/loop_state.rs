//! Explicit state machine for the agent loop.
//!
//! Per ADR *"Explicit `LoopState` enum, not a callback maze"*, the
//! state machine is data: a pure [`transition`] function takes the
//! current state plus the most recent input and returns the next
//! state, the events to emit, and the side effect to perform. Durable
//! runners (Temporal, AgentCore in later tickets) reuse the same
//! function with their own driver.

use crate::{
    AgentError, AgentEvent, ContentPart, FinishReason, Item, ModelRequest, ModelSettings,
    TokenUsage, ToolDef,
};

/// The explicit, observable state of the agent loop.
///
/// One variant per high-level phase. Does **not** derive `Clone`:
/// `Failed(AgentError)` wraps `anyhow::Error` (not `Clone`). The
/// transition function takes input and returns outcome by value; tests
/// use `assert_matches!` on `next_state` instead of equality.
#[derive(Debug)]
#[non_exhaustive]
pub enum LoopState {
    /// About to call the model for turn `turn`.
    CallingModel {
        /// Zero-indexed turn counter.
        turn: u32,
    },
    /// The model produced tool calls; about to execute them. `turn` is
    /// the turn that produced the calls — the next [`LoopState::CallingModel`]
    /// state will be `turn + 1`.
    ExecutingTools {
        /// The tool calls to execute concurrently.
        calls: Vec<ToolCallRequest>,
        /// The turn that produced these calls.
        turn: u32,
    },
    /// Handing off to another agent.
    ///
    /// **Not driveable in SMA-314.** Reaching this variant via
    /// [`transition`] returns
    /// [`LoopState::Failed`]`(`[`AgentError::NotImplemented`]` { feature: "handoff" })`.
    ApplyingHandoff {
        /// Name of the target agent.
        target: String,
        /// Conversation transcript to hand off.
        transcript: Vec<Item>,
    },
    /// Compacting session history. **Not driveable in SMA-314.**
    Compacting,
    /// Awaiting approval for a sensitive tool call.
    /// **Not driveable in SMA-314.**
    NeedsApproval {
        /// The tool calls awaiting approval.
        pending: Vec<ToolCallRequest>,
    },
    /// Terminal: run completed successfully.
    Done(FinalOutput),
    /// Terminal: run failed.
    Failed(AgentError),
}

/// One tool call the model has requested. Pure data.
#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    /// The provider-assigned call id (echoed back in `Item::ToolResult`).
    pub call_id: String,
    /// Tool name (matches [`crate::Tool::name`]).
    pub name: String,
    /// JSON-encoded arguments object.
    pub args: serde_json::Value,
}

/// Outcome of one tool execution. Errors are stringified so the
/// outcome implements `Clone` — `ToolError` carries `anyhow::Error`,
/// which is not `Clone`.
#[derive(Debug, Clone)]
pub struct ToolCallOutcome {
    /// The call id this outcome corresponds to.
    pub call_id: String,
    /// Either the tool's content output or a stringified error.
    pub result: Result<Vec<ContentPart>, String>,
}

/// Final assistant content + aggregated token usage at termination.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FinalOutput {
    /// The terminal assistant content.
    pub content: Vec<ContentPart>,
    /// Aggregated token usage across all turns.
    pub usage: TokenUsage,
}

impl FinalOutput {
    /// Concatenate all `ContentPart::Text` parts. This is the
    /// canonical rendering that feeds `RunResult.final_output` when
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

/// Data fed *into* the next [`transition`] call.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TransitionInput {
    /// Seeds the loop with the initial conversation.
    Start {
        /// The user's input messages.
        messages: Vec<Item>,
    },
    /// One model turn aggregated.
    ModelResponse {
        /// Items produced this turn (assistant message + tool calls).
        items: Vec<Item>,
        /// Token usage for this turn.
        usage: TokenUsage,
        /// Why the model stopped emitting tokens.
        finish_reason: FinishReason,
    },
    /// All tool calls for one turn have completed.
    ToolResults {
        /// Per-call outcomes.
        outcomes: Vec<ToolCallOutcome>,
    },
}

/// Side effect the async driver must run before the next transition.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum NextAction {
    /// Call the model with this request; produce a `ModelResponse`.
    CallModel {
        /// The request to send.
        request: ModelRequest,
    },
    /// Execute these tool calls concurrently; produce a `ToolResults`.
    ExecuteTools {
        /// The calls to fan out.
        calls: Vec<ToolCallRequest>,
    },
    /// The current state is terminal; stop driving.
    Terminate,
}

/// What [`transition`] needs to know about the agent and config for
/// one step. Doesn't carry user `Ctx` — that's the driver's concern.
pub struct TransitionCtx<'a> {
    /// Tool definitions available this run.
    pub tools: &'a [ToolDef],
    /// Provider-tuning knobs.
    pub model_settings: &'a ModelSettings,
    /// Maximum number of turns before the loop fails.
    pub max_turns: u32,
    /// The driver's accumulated conversation so far. The driver
    /// appends incoming items before calling [`transition`] and passes
    /// the slice in — [`transition`] reads but never mutates.
    pub conversation: &'a [Item],
}

/// One transition step's result. Not `Clone` (carries `LoopState`).
#[derive(Debug)]
pub struct TransitionOutcome {
    /// The state after this step.
    pub next_state: LoopState,
    /// Events to yield through the driver's event stream.
    pub events: Vec<AgentEvent>,
    /// Side effect the driver must run before the next step.
    pub next_action: NextAction,
}

/// Pure state-machine step. **No async, no tokio, no IO.**
///
/// Given the current state and the most recent input, produce the
/// next state, the events to emit, and the side effect to perform.
/// Resumable by construction: a durable runner can persist
/// [`LoopState`] plus the accumulated conversation and rehydrate the
/// loop at any transition boundary.
pub fn transition(
    state: &LoopState,
    input: TransitionInput,
    ctx: &TransitionCtx<'_>,
) -> TransitionOutcome {
    match (state, input) {
        // Start seeds the loop: emit TurnStarted, request CallModel.
        (LoopState::CallingModel { turn }, TransitionInput::Start { .. })
            if *turn < ctx.max_turns =>
        {
            let request = ModelRequest {
                messages: ctx.conversation.to_vec(),
                tools: ctx.tools.to_vec(),
                model_settings: ctx.model_settings.clone(),
            };
            TransitionOutcome {
                next_state: LoopState::CallingModel { turn: *turn },
                events: vec![AgentEvent::TurnStarted { turn: *turn }],
                next_action: NextAction::CallModel { request },
            }
        }
        // Other cases land in subsequent tasks.
        (s, i) => TransitionOutcome {
            next_state: LoopState::Failed(AgentError::Other(anyhow::anyhow!(
                "invalid transition: {s:?} ← {i:?}"
            ))),
            events: vec![AgentEvent::RunFailed {
                error: format!("invalid transition: {s:?} ← {i:?}"),
            }],
            next_action: NextAction::Terminate,
        },
    }
}
