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
    ResponseFormat, TokenUsage, ToolDef,
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
    /// Constrained finalizing turn: the model is asked to emit the
    /// structured output for the configured `output_type`.
    Finalizing {
        /// The turn index that produced this finalizing request.
        turn: u32,
    },
    /// The one allowed repair turn after a failed finalizing validation.
    RepairingOutput {
        /// The turn index of the finalizing turn being repaired.
        turn: u32,
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
    /// Structured-output type, when the agent configured one. Drives the
    /// constrained finalizing turn and output validation.
    pub output: Option<&'a crate::OutputType>,
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
    /// Items the driver must append to its owned conversation before the
    /// next step (e.g. a synthesized repair message). Empty in most arms.
    pub conversation_appends: Vec<Item>,
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
        // Max turns reached at the CallingModel boundary → fail fast.
        (LoopState::CallingModel { turn }, _) if *turn >= ctx.max_turns => TransitionOutcome {
            next_state: LoopState::Failed(AgentError::MaxTurnsExceeded(ctx.max_turns)),
            events: vec![AgentEvent::RunFailed {
                error: format!("max turns ({}) exceeded", ctx.max_turns),
            }],
            next_action: NextAction::Terminate,
            conversation_appends: Vec::new(),
        },
        // Start seeds the loop: emit TurnStarted, request CallModel.
        (LoopState::CallingModel { turn }, TransitionInput::Start { .. })
            if *turn < ctx.max_turns =>
        {
            match ctx.output {
                Some(out) if ctx.tools.is_empty() => {
                    let request = ModelRequest {
                        messages: ctx.conversation.to_vec(),
                        tools: Vec::new(),
                        model_settings: constrained_settings(ctx.model_settings, out),
                    };
                    TransitionOutcome {
                        next_state: LoopState::Finalizing { turn: *turn },
                        events: vec![AgentEvent::TurnStarted { turn: *turn }],
                        next_action: NextAction::CallModel { request },
                        conversation_appends: Vec::new(),
                    }
                }
                _ => {
                    let request = ModelRequest {
                        messages: ctx.conversation.to_vec(),
                        tools: ctx.tools.to_vec(),
                        model_settings: ctx.model_settings.clone(),
                    };
                    TransitionOutcome {
                        next_state: LoopState::CallingModel { turn: *turn },
                        events: vec![AgentEvent::TurnStarted { turn: *turn }],
                        next_action: NextAction::CallModel { request },
                        conversation_appends: Vec::new(),
                    }
                }
            }
        }
        // Model produced tool calls → fan out to ExecutingTools.
        (LoopState::CallingModel { turn }, TransitionInput::ModelResponse { items, .. })
            if items.iter().any(|i| matches!(i, Item::ToolCall { .. })) =>
        {
            let mut events: Vec<AgentEvent> = Vec::new();
            let mut calls: Vec<ToolCallRequest> = Vec::new();
            for item in &items {
                match item {
                    Item::AssistantMessage { .. } => {
                        events.push(AgentEvent::MessageOutput { item: item.clone() });
                    }
                    Item::ToolCall {
                        call_id,
                        name,
                        args,
                    } => {
                        events.push(AgentEvent::ToolCallItem { item: item.clone() });
                        calls.push(ToolCallRequest {
                            call_id: call_id.clone(),
                            name: name.clone(),
                            args: args.clone(),
                        });
                    }
                    _ => {}
                }
            }
            TransitionOutcome {
                next_state: LoopState::ExecutingTools {
                    calls: calls.clone(),
                    turn: *turn,
                },
                events,
                next_action: NextAction::ExecuteTools { calls },
                conversation_appends: Vec::new(),
            }
        }
        // Model produced a response with no tool calls → either issue a
        // constrained finalizing turn (output set) or terminate (no output).
        (LoopState::CallingModel { turn }, TransitionInput::ModelResponse { items, usage, .. })
            if !items.iter().any(|i| matches!(i, Item::ToolCall { .. })) =>
        {
            let mut events: Vec<AgentEvent> = items
                .iter()
                .filter(|i| matches!(i, Item::AssistantMessage { .. }))
                .cloned()
                .map(|item| AgentEvent::MessageOutput { item })
                .collect();

            match ctx.output {
                Some(out) => {
                    // Phase 2: issue one constrained finalizing turn (real tools
                    // withdrawn; the prior unconstrained answer stays in context).
                    // The finalizing call is a distinct model invocation, so it
                    // gets its own turn number (the unconstrained turn already
                    // emitted `TurnStarted { turn }`).
                    let finalizing_turn = *turn + 1;
                    let request = ModelRequest {
                        messages: ctx.conversation.to_vec(),
                        tools: Vec::new(),
                        model_settings: constrained_settings(ctx.model_settings, out),
                    };
                    events.push(AgentEvent::TurnStarted {
                        turn: finalizing_turn,
                    });
                    TransitionOutcome {
                        next_state: LoopState::Finalizing {
                            turn: finalizing_turn,
                        },
                        events,
                        next_action: NextAction::CallModel { request },
                        conversation_appends: Vec::new(),
                    }
                }
                None => {
                    let content = last_assistant_content(&items);
                    events.push(AgentEvent::RunCompleted { usage });
                    TransitionOutcome {
                        next_state: LoopState::Done(FinalOutput { content, usage }),
                        events,
                        next_action: NextAction::Terminate,
                        conversation_appends: Vec::new(),
                    }
                }
            }
        }
        // Tool results complete → bump turn and ask the model again.
        (LoopState::ExecutingTools { turn, .. }, TransitionInput::ToolResults { outcomes }) => {
            let next_turn = turn + 1;
            let mut events: Vec<AgentEvent> = outcomes
                .into_iter()
                .map(|o| AgentEvent::ToolOutputItem {
                    item: Item::ToolResult {
                        call_id: o.call_id,
                        content: o
                            .result
                            .unwrap_or_else(|e| vec![ContentPart::Text { text: e }]),
                    },
                })
                .collect();
            if next_turn >= ctx.max_turns {
                events.push(AgentEvent::RunFailed {
                    error: format!("max turns ({}) exceeded", ctx.max_turns),
                });
                return TransitionOutcome {
                    next_state: LoopState::Failed(AgentError::MaxTurnsExceeded(ctx.max_turns)),
                    events,
                    next_action: NextAction::Terminate,
                    conversation_appends: Vec::new(),
                };
            }
            events.push(AgentEvent::TurnStarted { turn: next_turn });
            let request = ModelRequest {
                messages: ctx.conversation.to_vec(),
                tools: ctx.tools.to_vec(),
                model_settings: ctx.model_settings.clone(),
            };
            TransitionOutcome {
                next_state: LoopState::CallingModel { turn: next_turn },
                events,
                next_action: NextAction::CallModel { request },
                conversation_appends: Vec::new(),
            }
        }
        // Finalizing: validate the model's structured output.
        (LoopState::Finalizing { turn }, TransitionInput::ModelResponse { items, usage, .. }) => {
            let Some(out) = ctx.output else {
                return TransitionOutcome {
                    next_state: LoopState::Failed(AgentError::Other(anyhow::anyhow!(
                        "Finalizing state without output type"
                    ))),
                    events: vec![AgentEvent::RunFailed {
                        error: "internal: Finalizing without output type".to_owned(),
                    }],
                    next_action: NextAction::Terminate,
                    conversation_appends: Vec::new(),
                };
            };
            let mut events: Vec<AgentEvent> = items
                .iter()
                .filter(|i| matches!(i, Item::AssistantMessage { .. }))
                .cloned()
                .map(|item| AgentEvent::MessageOutput { item })
                .collect();
            let content = last_assistant_content(&items);
            let has_tool_call = items.iter().any(|i| matches!(i, Item::ToolCall { .. }));

            let validation = if has_tool_call {
                Err(vec![
                    "model called a tool on the constrained finalizing turn".to_owned(),
                ])
            } else {
                validate_terminal(out, &content)
            };

            match validation {
                Ok(()) => {
                    events.push(AgentEvent::RunCompleted { usage });
                    TransitionOutcome {
                        next_state: LoopState::Done(FinalOutput { content, usage }),
                        events,
                        next_action: NextAction::Terminate,
                        conversation_appends: Vec::new(),
                    }
                }
                Err(schema_errors) => {
                    // Task 8 replaces this branch with the one-shot repair transition.
                    let final_text = flatten_text(&content);
                    events.push(AgentEvent::StructuredOutputFailed {
                        schema_errors: schema_errors.clone(),
                        final_text: final_text.clone(),
                    });
                    events.push(AgentEvent::RunFailed {
                        error: "invalid structured output".to_owned(),
                    });
                    let _ = turn;
                    TransitionOutcome {
                        next_state: LoopState::Failed(AgentError::InvalidStructuredOutput {
                            schema_errors,
                            final_text,
                        }),
                        events,
                        next_action: NextAction::Terminate,
                        conversation_appends: Vec::new(),
                    }
                }
            }
        }
        // Unreachable-in-SMA-314 variants surface NotImplemented and Terminate.
        (LoopState::ApplyingHandoff { .. }, _) => not_implemented("handoff"),
        (LoopState::Compacting, _) => not_implemented("compaction"),
        (LoopState::NeedsApproval { .. }, _) => not_implemented("approval"),
        // Other cases land in subsequent tasks.
        (s, i) => TransitionOutcome {
            next_state: LoopState::Failed(AgentError::Other(anyhow::anyhow!(
                "invalid transition: {s:?} ← {i:?}"
            ))),
            events: vec![AgentEvent::RunFailed {
                error: format!("invalid transition: {s:?} ← {i:?}"),
            }],
            next_action: NextAction::Terminate,
            conversation_appends: Vec::new(),
        },
    }
}

/// Helper to surface `NotImplemented` for deferred-variant transitions.
fn not_implemented(feature: &'static str) -> TransitionOutcome {
    TransitionOutcome {
        next_state: LoopState::Failed(AgentError::NotImplemented { feature }),
        events: vec![AgentEvent::RunFailed {
            error: format!("not yet implemented: {feature}"),
        }],
        next_action: NextAction::Terminate,
        conversation_appends: Vec::new(),
    }
}

/// Build constrained model settings for a finalizing/repair turn: inject the
/// `output_type`-derived `response_format` (raw schema, strict mode) and clear
/// any caller tool_choice (Anthropic forces its own synthesized tool).
fn constrained_settings(base: &ModelSettings, output: &crate::OutputType) -> ModelSettings {
    let mut s = base.clone();
    s.response_format = Some(ResponseFormat::JsonSchema {
        name: output.name.clone(),
        schema: output.schema.as_value().clone(),
        strict: true,
    });
    s.tool_choice = None;
    s
}

/// Concatenate `ContentPart::Text` parts (the structured output arrives as text
/// on both providers).
fn flatten_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Parse + validate terminal text against the output type.
/// `Ok(())` on success; `Err(schema_errors)` otherwise (non-JSON included).
fn validate_terminal(
    output: &crate::OutputType,
    content: &[ContentPart],
) -> Result<(), Vec<String>> {
    let text = flatten_text(content);
    let value: serde_json::Value = match serde_json::from_str(text.trim()) {
        Ok(v) => v,
        Err(e) => return Err(vec![format!("response was not valid JSON: {e}")]),
    };
    output.validate(&value)
}

/// The last `AssistantMessage` content in a list of items.
fn last_assistant_content(items: &[Item]) -> Vec<ContentPart> {
    items
        .iter()
        .rev()
        .find_map(|i| match i {
            Item::AssistantMessage { content, .. } => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_default()
}
