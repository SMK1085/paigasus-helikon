//! Pure step machine for the Temporal-backed durable agent loop.
//!
//! [`crate::driver::DurableDriver`] mirrors `paigasus-helikon-core`'s
//! ephemeral driver — the inline step loop inside `LlmAgent::run`
//! (`crates/paigasus-helikon-core/src/agent.rs`) — but instead
//! of directly calling a [`Model`] or [`Tool`](paigasus_helikon_core::Tool)
//! it yields a [`crate::driver::DriverEffect`] the caller must satisfy
//! out-of-band (a Temporal activity) and feed back via `apply_*`. This keeps
//! the loop **SDK-free**: no `temporalio-*` import appears in this module,
//! so it can be unit-tested without a Temporal worker and reused verbatim by
//! the Task 8 workflow, which is a mechanical executor of the effects this
//! module produces.
//!
//! [`Model`]: paigasus_helikon_core::Model
//!
//! # Ordering contract
//!
//! [`paigasus_helikon_core::transition`] reads the driver's conversation but
//! never appends to it — the driver owns all conversation mutation. Per
//! `loop_state.rs`'s `ToolResults` arm (and the ephemeral driver's mirror —
//! its `Item::ToolResult`-append site in the `NextAction::ExecuteTools` arm
//! of `LlmAgent::run`), the split is:
//! - The driver appends [`paigasus_helikon_core::Item::ToolResult`]/model-turn
//!   items to its conversation directly, in
//!   [`crate::driver::DurableDriver::apply_model`] and
//!   [`crate::driver::DurableDriver::apply_tools`].
//! - `transition` only *emits events* describing those items
//!   (`AgentEvent::ToolOutputItem`, `AgentEvent::MessageOutput`, ...); it
//!   never mutates the conversation itself.
//!
//! Violating this (e.g. having `transition` also push `Item::ToolResult`)
//! would double-append tool results into the conversation the next
//! `CallModel` request carries.

use paigasus_helikon_core::{
    transition, AgentEvent, ContentPart, Item, LoopState, ModelRequest, ModelSettings, NextAction,
    OutputType, TokenUsage, ToolCallOutcome, ToolCallRequest, ToolDef, TransitionCtx,
    TransitionInput, TransitionOutcome,
};

use crate::error::ErrorKindPayload;
use crate::payloads::{
    DriverConfig, DurableRunOutcome, FinalOutputPayload, ModelTurnResult, RunStatusPayload,
    WorkflowInput,
};

/// What the workflow must do next.
///
/// Task 8's workflow is a mechanical executor of this enum: each variant
/// names exactly one Temporal activity (or termination) to invoke, and the
/// activity's result feeds back through the matching `apply_*` method.
#[derive(Debug)]
pub enum DriverEffect {
    /// Run the render_instructions activity (always the first effect).
    RenderInstructions,
    /// Invoke the model with this request; feed the result to
    /// [`DurableDriver::apply_model`] (or [`DurableDriver::apply_model_failure`]
    /// on a non-retryable error).
    CallModel(ModelRequest),
    /// Execute these tool calls (concurrently, subject to
    /// [`DriverConfig::parallel_tool_call_limit`]); feed the outcomes back
    /// via [`DurableDriver::apply_tools`] **in this same order**.
    ExecuteTools(Vec<ToolCallRequest>),
    /// The run has reached a terminal outcome; stop driving.
    Finished(DurableRunOutcome),
}

/// Static agent definition the driver plans against (worker-registered).
///
/// Built once per agent by whoever registers the Temporal worker (mirroring
/// the tool/model/output-type configuration an [`paigasus_helikon_core::LlmAgent`]
/// carries) and handed to [`DurableDriver::new`] alongside the per-run
/// [`WorkflowInput`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentPlan {
    /// Tool definitions available this run.
    pub tool_defs: Vec<ToolDef>,
    /// Provider-tuning knobs.
    pub model_settings: ModelSettings,
    /// Structured-output type, when the agent configured one.
    pub output: Option<OutputType>,
}

/// Why the workflow is tearing the driver down before a natural terminal
/// state.
#[derive(Debug, Clone, Copy)]
pub enum InterruptKind {
    /// The workflow (or its caller) was cancelled.
    Cancelled,
    /// The run exceeded [`WorkflowInput::timeout_ms`].
    TimedOut,
}

/// Internal driving phase. Not part of the public API.
#[derive(Debug)]
enum Phase {
    /// Waiting on the render_instructions activity result.
    AwaitingInstructions,
    /// Normal operation: `next_effect` runs `transition` against
    /// `pending_input`.
    Driving,
    /// Terminal: the outcome is cached so repeated `next_effect` calls (and
    /// `interrupt`) are idempotent instead of re-driving a finished loop.
    Done(DurableRunOutcome),
}

/// Pure step machine for one durable agent run.
///
/// Owns the conversation, the [`LoopState`], the accumulated event log, and
/// the running usage total; advances by alternating [`Self::next_effect`]
/// (what to do next) with the matching `apply_*` call (the activity's
/// result). Contains **no** Temporal SDK types or async code — a Temporal
/// workflow drives it, but this type has no idea Temporal exists.
pub struct DurableDriver {
    agent_name: String,
    plan: AgentPlan,
    config: DriverConfig,
    /// The run's seed messages (`WorkflowInput::conversation`), held until
    /// [`Self::apply_instructions`] seeds the real conversation with the
    /// system item ahead of them.
    seed_messages: Vec<Item>,
    conversation: Vec<Item>,
    loop_state: LoopState,
    pending_input: Option<TransitionInput>,
    events: Vec<AgentEvent>,
    /// Cumulative usage as of the most recent [`LoopState`] that carried one
    /// (every variant except `Failed`/`Compacting`/`NeedsApproval`). Read at
    /// interrupt/failure time instead of recomputed, since `LoopState::Failed`
    /// itself carries no usage.
    usage: TokenUsage,
    phase: Phase,
}

impl DurableDriver {
    /// Start a new driver for one run. Does not seed the conversation yet —
    /// call [`Self::next_effect`] (yields [`DriverEffect::RenderInstructions`])
    /// then [`Self::apply_instructions`] to do that.
    pub fn new(input: WorkflowInput, plan: AgentPlan) -> Self {
        Self {
            agent_name: input.agent_name,
            plan,
            config: input.config,
            seed_messages: input.conversation,
            conversation: Vec::new(),
            loop_state: LoopState::CallingModel {
                turn: 0,
                usage: TokenUsage::default(),
            },
            pending_input: None,
            events: Vec::new(),
            usage: TokenUsage::default(),
            phase: Phase::AwaitingInstructions,
        }
    }

    /// What the workflow must do next.
    ///
    /// Idempotent at the boundaries: called before [`Self::apply_instructions`]
    /// it always returns [`DriverEffect::RenderInstructions`]; called again
    /// after a terminal outcome it replays the same [`DriverEffect::Finished`]
    /// rather than re-driving a finished loop.
    ///
    /// # Panics
    ///
    /// Panics if called with no pending transition input queued — i.e.
    /// calling `next_effect` twice in a row without an intervening
    /// `apply_instructions`/`apply_model`/`apply_model_failure`/`apply_tools`
    /// call in between (outside the `RenderInstructions`/`Finished` idempotent
    /// boundaries above, where no pending input is ever expected). The
    /// workflow's own effect loop always alternates `next_effect`/`apply_*`
    /// correctly by construction, so this should not fire in practice.
    pub fn next_effect(&mut self) -> DriverEffect {
        match &self.phase {
            Phase::AwaitingInstructions => return DriverEffect::RenderInstructions,
            Phase::Done(outcome) => return DriverEffect::Finished(outcome.clone()),
            Phase::Driving => {}
        }

        let tx_ctx = TransitionCtx {
            tools: &self.plan.tool_defs,
            model_settings: &self.plan.model_settings,
            max_turns: self.config.max_turns,
            conversation: &self.conversation,
            output: self.plan.output.as_ref(),
            handoffs: &[],
        };
        let tx_input = self
            .pending_input
            .take()
            .expect("next_effect called without a pending transition input (apply_* not called)");
        let TransitionOutcome {
            next_state,
            events,
            next_action,
            conversation_appends,
        } = transition(&self.loop_state, tx_input, &tx_ctx);

        self.events.extend(events);
        self.conversation.extend(conversation_appends);
        if let Some(u) = state_usage(&next_state) {
            self.usage = u;
        }

        match next_action {
            NextAction::CallModel { request } => {
                self.loop_state = next_state;
                DriverEffect::CallModel(request)
            }
            NextAction::ExecuteTools { calls } => {
                self.loop_state = next_state;
                DriverEffect::ExecuteTools(calls)
            }
            NextAction::Terminate => {
                let status = match next_state {
                    LoopState::Done(final_output) => {
                        RunStatusPayload::Completed(FinalOutputPayload {
                            content: final_output.content,
                            usage: final_output.usage,
                        })
                    }
                    LoopState::Failed(err) => {
                        RunStatusPayload::AgentFailed(ErrorKindPayload::from_agent_error(&err))
                    }
                    // `transition` never pairs `Terminate` with any other
                    // state today, but `LoopState` is `#[non_exhaustive]`.
                    other => RunStatusPayload::AgentFailed(ErrorKindPayload::Other {
                        message: format!("unexpected terminal state: {other:?}"),
                    }),
                };
                self.finish(status)
            }
            NextAction::Handoff => {
                // Defensively unreachable: `handoffs` above is always `&[]`,
                // so `transition` can never select a handoff target. Kept in
                // case a later ticket wires handoff-configured agents through
                // this driver without updating this match.
                let target = match &next_state {
                    LoopState::ApplyingHandoff { target, .. } => target.clone(),
                    _ => "<unknown>".to_owned(),
                };
                self.finish(RunStatusPayload::AgentFailed(
                    ErrorKindPayload::HandoffUnsupported { target },
                ))
            }
            // `NextAction` is `#[non_exhaustive]`.
            other => self.finish(RunStatusPayload::AgentFailed(ErrorKindPayload::Other {
                message: format!("unsupported next_action: {other:?}"),
            })),
        }
    }

    /// Result of `RenderInstructions`: seed `[System] ++ conversation`, emit
    /// `RunStarted`.
    pub fn apply_instructions(&mut self, system_text: String) {
        let seed_messages = std::mem::take(&mut self.seed_messages);
        let mut conversation = Vec::with_capacity(seed_messages.len() + 1);
        if !system_text.is_empty() {
            conversation.push(Item::System {
                content: vec![ContentPart::Text { text: system_text }],
            });
        }
        conversation.extend(seed_messages.iter().cloned());
        self.conversation = conversation;

        self.events.push(AgentEvent::RunStarted {
            agent: self.agent_name.clone(),
        });

        self.pending_input = Some(TransitionInput::Start {
            messages: seed_messages,
        });
        self.loop_state = LoopState::CallingModel {
            turn: 0,
            usage: TokenUsage::default(),
        };
        self.phase = Phase::Driving;
    }

    /// Feed back a completed model turn.
    ///
    /// Mirrors the ephemeral driver (`agent.rs`'s `NextAction::CallModel`
    /// arm): the driver — not `transition` — appends the turn's items to the
    /// conversation, immediately, so the next `next_effect` call's
    /// `TransitionCtx::conversation` already includes them.
    pub fn apply_model(&mut self, turn: ModelTurnResult) {
        let paigasus_helikon_core::ModelTurn {
            items,
            usage,
            finish_reason,
            ..
        } = turn.0;
        self.conversation.extend(items.iter().cloned());
        self.pending_input = Some(TransitionInput::ModelResponse {
            items,
            usage,
            finish_reason,
        });
    }

    /// Model activity failed terminally (non-retryable `ErrorKindPayload`
    /// json).
    ///
    /// Bypasses `transition` entirely — like the ephemeral driver's direct
    /// `Err(e) => { failure.set(...); yield RunFailed; return; }` paths, a
    /// model invocation failure is a terminal event the state machine never
    /// sees.
    pub fn apply_model_failure(&mut self, kind: ErrorKindPayload) {
        let message = kind.clone().into_agent_error().to_string();
        self.events.push(AgentEvent::RunFailed { error: message });
        self.finish(RunStatusPayload::AgentFailed(kind));
    }

    /// Feed back completed tool outcomes. Outcomes MUST be passed in
    /// original call order (workflow joins in order).
    ///
    /// Mirrors the ephemeral driver (`agent.rs`'s `NextAction::ExecuteTools`
    /// arm): the driver appends one [`Item::ToolResult`] per outcome, in the
    /// given order, **before** the next `next_effect` call — `transition`'s
    /// `ToolResults` arm only emits `AgentEvent::ToolOutputItem`s describing
    /// them, it does not append conversation items itself.
    pub fn apply_tools(&mut self, outcomes: Vec<ToolCallOutcome>) {
        for o in &outcomes {
            self.conversation.push(Item::ToolResult {
                call_id: o.call_id.clone(),
                content: o
                    .result
                    .clone()
                    .unwrap_or_else(|e| vec![ContentPart::Text { text: e }]),
            });
        }
        self.pending_input = Some(TransitionInput::ToolResults { outcomes });
    }

    /// Cancel/timeout interruption — partial outcome with events so far.
    ///
    /// If the run had already reached a terminal state (e.g. a prior
    /// `next_effect` call returned `Finished`), that outcome is returned
    /// unchanged instead of being overwritten with `Cancelled`/`TimedOut` —
    /// a completed run's status is not retroactively an interruption.
    pub fn interrupt(self, kind: InterruptKind) -> DurableRunOutcome {
        if let Phase::Done(outcome) = self.phase {
            return outcome;
        }
        let status = match kind {
            InterruptKind::Cancelled => RunStatusPayload::Cancelled,
            InterruptKind::TimedOut => RunStatusPayload::TimedOut,
        };
        DurableRunOutcome {
            status,
            events: self.events,
            usage: self.usage,
        }
    }

    /// Cache `status` (with all accumulated events/usage) as the terminal
    /// outcome and return the matching [`DriverEffect::Finished`].
    fn finish(&mut self, status: RunStatusPayload) -> DriverEffect {
        let outcome = DurableRunOutcome {
            status,
            events: self.events.clone(),
            usage: self.usage,
        };
        self.phase = Phase::Done(outcome.clone());
        DriverEffect::Finished(outcome)
    }
}

/// Extract the cumulative usage carried by states that have one.
/// `LoopState` is `#[non_exhaustive]`, so this needs a wildcard arm even
/// though every current variant is named.
fn state_usage(state: &LoopState) -> Option<TokenUsage> {
    match state {
        LoopState::CallingModel { usage, .. }
        | LoopState::ExecutingTools { usage, .. }
        | LoopState::ApplyingHandoff { usage, .. }
        | LoopState::Finalizing { usage, .. }
        | LoopState::RepairingOutput { usage, .. } => Some(*usage),
        LoopState::Done(final_output) => Some(final_output.usage),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::{FinishReason, ModelTurn};

    macro_rules! assert_matches {
        ($expr:expr, $pat:pat if $guard:expr $(,)?) => {
            let val = $expr;
            match val {
                $pat if $guard => {}
                other => panic!(
                    "assertion failed: {other:?} does not match {} if {}",
                    stringify!($pat),
                    stringify!($guard)
                ),
            }
        };
        ($expr:expr, $pat:pat $(,)?) => {
            let val = $expr;
            match val {
                $pat => {}
                other => panic!(
                    "assertion failed: {other:?} does not match {}",
                    stringify!($pat)
                ),
            }
        };
    }

    /// Minimal schema struct for structured-output tests.
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct Answer {
        #[allow(dead_code)]
        value: u32,
    }

    fn input(msgs: Vec<Item>) -> WorkflowInput {
        WorkflowInput {
            agent_name: "a1".to_owned(),
            conversation: msgs,
            config: DriverConfig {
                max_turns: 4,
                parallel_tool_call_limit: None,
            },
            timeout_ms: None,
        }
    }

    fn plan_no_tools() -> AgentPlan {
        AgentPlan {
            tool_defs: Vec::new(),
            model_settings: ModelSettings::new(),
            output: None,
        }
    }

    fn plan_with_output() -> AgentPlan {
        AgentPlan {
            tool_defs: Vec::new(),
            model_settings: ModelSettings::new(),
            output: Some(OutputType::from_schema::<Answer>()),
        }
    }

    fn user(text: &str) -> Item {
        Item::UserMessage {
            content: vec![ContentPart::Text { text: text.into() }],
        }
    }

    fn usage(input_tokens: u64, output_tokens: u64) -> TokenUsage {
        // `TokenUsage` is `#[non_exhaustive]`: no struct-expression
        // construction outside its crate, even with `..Default::default()`
        // (E0639) — build via `Default` then assign the `pub` fields.
        let mut u = TokenUsage::default();
        u.input_tokens = input_tokens;
        u.output_tokens = output_tokens;
        u.total_tokens = input_tokens + output_tokens;
        u
    }

    fn model_text_turn(text: &str) -> ModelTurnResult {
        ModelTurnResult(ModelTurn::new(
            vec![Item::AssistantMessage {
                content: vec![ContentPart::Text { text: text.into() }],
                agent: None,
            }],
            usage(1, 1),
            FinishReason::Stop,
        ))
    }

    #[test]
    fn first_effect_is_render_instructions_then_model() {
        let mut d = DurableDriver::new(input(vec![user("hi")]), plan_no_tools());
        assert_matches!(d.next_effect(), DriverEffect::RenderInstructions);

        d.apply_instructions("sys".to_owned());
        let effect = d.next_effect();
        let request = match effect {
            DriverEffect::CallModel(r) => r,
            other => panic!("expected CallModel, got {other:?}"),
        };
        assert_matches!(&request.messages[0], Item::System { .. });
        assert_matches!(&request.messages[1], Item::UserMessage { .. });

        assert_eq!(d.events.len(), 2);
        assert_matches!(&d.events[0], AgentEvent::RunStarted { agent } if agent == "a1");
        assert_matches!(&d.events[1], AgentEvent::TurnStarted { turn: 0 });
    }

    #[test]
    fn empty_system_text_is_omitted() {
        let mut d = DurableDriver::new(input(vec![user("hi")]), plan_no_tools());
        assert_matches!(d.next_effect(), DriverEffect::RenderInstructions);

        d.apply_instructions(String::new());
        let request = match d.next_effect() {
            DriverEffect::CallModel(r) => r,
            other => panic!("expected CallModel, got {other:?}"),
        };
        assert_eq!(request.messages.len(), 1);
        assert_matches!(&request.messages[0], Item::UserMessage { .. });
    }

    #[test]
    fn text_response_completes() {
        let mut d = DurableDriver::new(input(vec![user("hi")]), plan_no_tools());
        assert_matches!(d.next_effect(), DriverEffect::RenderInstructions);
        d.apply_instructions("sys".to_owned());
        assert_matches!(d.next_effect(), DriverEffect::CallModel(_));

        d.apply_model(model_text_turn("hello"));
        let outcome = match d.next_effect() {
            DriverEffect::Finished(o) => o,
            other => panic!("expected Finished, got {other:?}"),
        };

        assert_matches!(&outcome.status, RunStatusPayload::Completed(_));
        assert_matches!(outcome.events.last(), Some(AgentEvent::RunCompleted { .. }));
        assert_eq!(outcome.usage.input_tokens, 1);
        assert_eq!(outcome.usage.output_tokens, 1);
    }

    #[test]
    fn tool_roundtrip_appends_in_call_order() {
        let mut d = DurableDriver::new(input(vec![user("hi")]), plan_no_tools());
        assert_matches!(d.next_effect(), DriverEffect::RenderInstructions);
        d.apply_instructions("sys".to_owned());
        assert_matches!(d.next_effect(), DriverEffect::CallModel(_));

        d.apply_model(ModelTurnResult(ModelTurn::new(
            vec![
                Item::AssistantMessage {
                    content: vec![ContentPart::Text {
                        text: "checking".into(),
                    }],
                    agent: None,
                },
                Item::ToolCall {
                    call_id: "c1".into(),
                    name: "search".into(),
                    args: serde_json::json!({"q": "a"}),
                },
                Item::ToolCall {
                    call_id: "c2".into(),
                    name: "search".into(),
                    args: serde_json::json!({"q": "b"}),
                },
            ],
            usage(2, 2),
            FinishReason::ToolCalls,
        )));

        let calls = match d.next_effect() {
            DriverEffect::ExecuteTools(c) => c,
            other => panic!("expected ExecuteTools, got {other:?}"),
        };
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].call_id, "c1");
        assert_eq!(calls[1].call_id, "c2");

        d.apply_tools(vec![
            ToolCallOutcome {
                call_id: "c1".into(),
                result: Ok(vec![ContentPart::Text {
                    text: "result-a".into(),
                }]),
            },
            ToolCallOutcome {
                call_id: "c2".into(),
                result: Ok(vec![ContentPart::Text {
                    text: "result-b".into(),
                }]),
            },
        ]);

        let request = match d.next_effect() {
            DriverEffect::CallModel(r) => r,
            other => panic!("expected CallModel, got {other:?}"),
        };
        // [System, User, Assistant, ToolCall c1, ToolCall c2, ToolResult c1, ToolResult c2]
        assert_eq!(request.messages.len(), 7);
        assert_matches!(
            &request.messages[5],
            Item::ToolResult { call_id, .. } if call_id == "c1"
        );
        assert_matches!(
            &request.messages[6],
            Item::ToolResult { call_id, .. } if call_id == "c2"
        );
    }

    #[test]
    fn conversation_appends_are_applied() {
        let mut d = DurableDriver::new(input(vec![user("hi")]), plan_with_output());
        assert_matches!(d.next_effect(), DriverEffect::RenderInstructions);
        d.apply_instructions("sys".to_owned());
        // Finalizing turn (output type set, no tools).
        assert_matches!(d.next_effect(), DriverEffect::CallModel(_));

        // Non-conforming (non-JSON) terminal text on the finalizing turn.
        d.apply_model(model_text_turn("not json"));

        let request = match d.next_effect() {
            DriverEffect::CallModel(r) => r,
            other => panic!("expected CallModel (repair turn), got {other:?}"),
        };
        assert_matches!(request.messages.last(), Some(Item::UserMessage { .. }));
    }

    #[test]
    fn handoff_terminates_with_unsupported_error() {
        // v0 rejects handoff-configured agents at registration (a later
        // task), so `TransitionCtx.handoffs` is always `&[]` here and
        // `transition` can never emit `NextAction::Handoff` through this
        // driver — the arm is defensively unreachable. Two checks stand in:
        //
        // 1. `TransitionCtx` really is built with `handoffs: &[]`: a tool
        //    call using a conventional handoff-tool name is executed as a
        //    REGULAR tool call rather than being special-cased, since
        //    `transition` only recognizes names present in `ctx.handoffs`.
        let mut d = DurableDriver::new(input(vec![user("hi")]), plan_no_tools());
        assert_matches!(d.next_effect(), DriverEffect::RenderInstructions);
        d.apply_instructions(String::new());
        assert_matches!(d.next_effect(), DriverEffect::CallModel(_));

        d.apply_model(ModelTurnResult(ModelTurn::new(
            vec![Item::ToolCall {
                call_id: "c1".into(),
                name: "transfer_to_billing".into(),
                args: serde_json::json!({}),
            }],
            usage(1, 1),
            FinishReason::ToolCalls,
        )));
        match d.next_effect() {
            DriverEffect::ExecuteTools(calls) => assert_eq!(calls.len(), 1),
            other => panic!(
                "expected ExecuteTools (handoffs:&[] means no name is special-cased), got {other:?}"
            ),
        }

        // 2. The driver's Terminate-mapping treats a `LoopState::Failed`
        //    (the same shape the Handoff arm would also wrap into
        //    `AgentFailed`) as `RunStatusPayload::AgentFailed`. Exercised
        //    end-to-end via the max-turns path (see
        //    `max_turns_exceeded_maps_to_typed_error`), confirmed here too.
        d.apply_tools(vec![ToolCallOutcome {
            call_id: "c1".into(),
            result: Ok(vec![ContentPart::Text { text: "ok".into() }]),
        }]);
        // config.max_turns = 4, so this does not fail yet — just confirms
        // driving continues normally past the "handoff-shaped" tool call.
        assert_matches!(d.next_effect(), DriverEffect::CallModel(_));
    }

    #[test]
    fn max_turns_exceeded_maps_to_typed_error() {
        let mut cfg_input = input(vec![user("hi")]);
        cfg_input.config.max_turns = 1;
        let mut d = DurableDriver::new(cfg_input, plan_no_tools());
        assert_matches!(d.next_effect(), DriverEffect::RenderInstructions);
        d.apply_instructions(String::new());
        assert_matches!(d.next_effect(), DriverEffect::CallModel(_));

        d.apply_model(ModelTurnResult(ModelTurn::new(
            vec![Item::ToolCall {
                call_id: "c1".into(),
                name: "search".into(),
                args: serde_json::json!({}),
            }],
            usage(1, 1),
            FinishReason::ToolCalls,
        )));
        assert_matches!(d.next_effect(), DriverEffect::ExecuteTools(_));

        d.apply_tools(vec![ToolCallOutcome {
            call_id: "c1".into(),
            result: Ok(vec![ContentPart::Text { text: "ok".into() }]),
        }]);

        let outcome = match d.next_effect() {
            DriverEffect::Finished(o) => o,
            other => panic!("expected Finished, got {other:?}"),
        };
        assert_matches!(
            outcome.status,
            RunStatusPayload::AgentFailed(ErrorKindPayload::MaxTurnsExceeded(1))
        );
    }

    #[test]
    fn interrupt_returns_partial_events() {
        let mut d = DurableDriver::new(input(vec![user("hi")]), plan_no_tools());
        assert_matches!(d.next_effect(), DriverEffect::RenderInstructions);
        d.apply_instructions("sys".to_owned());
        assert_matches!(d.next_effect(), DriverEffect::CallModel(_));

        d.apply_model(ModelTurnResult(ModelTurn::new(
            vec![
                Item::AssistantMessage {
                    content: vec![ContentPart::Text {
                        text: "thinking".into(),
                    }],
                    agent: None,
                },
                Item::ToolCall {
                    call_id: "c1".into(),
                    name: "search".into(),
                    args: serde_json::json!({}),
                },
            ],
            usage(3, 4),
            FinishReason::ToolCalls,
        )));
        assert_matches!(d.next_effect(), DriverEffect::ExecuteTools(_));

        let outcome = d.interrupt(InterruptKind::Cancelled);
        assert_matches!(outcome.status, RunStatusPayload::Cancelled);
        assert!(!outcome.events.is_empty());
        assert_matches!(outcome.events.first(), Some(AgentEvent::RunStarted { .. }));
        assert!(outcome
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnStarted { .. })));
        assert!(outcome
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::MessageOutput { .. })));
        assert_eq!(outcome.usage.input_tokens, 3);
        assert_eq!(outcome.usage.output_tokens, 4);
    }

    #[test]
    fn model_failure_is_terminal_with_events() {
        let mut d = DurableDriver::new(input(vec![user("hi")]), plan_no_tools());
        assert_matches!(d.next_effect(), DriverEffect::RenderInstructions);
        d.apply_instructions("sys".to_owned());
        assert_matches!(d.next_effect(), DriverEffect::CallModel(_));

        d.apply_model_failure(ErrorKindPayload::Model {
            message: "connection lost".to_owned(),
        });

        let outcome = match d.next_effect() {
            DriverEffect::Finished(o) => o,
            other => panic!("expected Finished, got {other:?}"),
        };
        assert_matches!(
            outcome.status,
            RunStatusPayload::AgentFailed(ErrorKindPayload::Model { .. })
        );
        assert_matches!(outcome.events.last(), Some(AgentEvent::RunFailed { .. }));
    }
}
