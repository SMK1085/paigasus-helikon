//! Pure-function unit tests on `transition(...)`. No tokio, no async,
//! no IO. Locks SMA-314's state-machine determinism guarantees.

use paigasus_helikon_core::{
    transition, AgentEvent, ContentPart, FinishReason, Item, LoopState, ModelSettings, NextAction,
    OutputType, ResponseFormat, TokenUsage, ToolCallOutcome, ToolCallRequest, TransitionCtx,
    TransitionInput,
};

/// Minimal schema struct for structured-output tests.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct Answer {
    #[allow(dead_code)]
    value: u32,
}

macro_rules! assert_matches {
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

fn ctx_with<'a>(
    max_turns: u32,
    conversation: &'a [Item],
    settings: &'a ModelSettings,
) -> TransitionCtx<'a> {
    TransitionCtx {
        tools: &[],
        model_settings: settings,
        max_turns,
        conversation,
        output: None,
        handoffs: &[],
    }
}

#[test]
fn start_seeds_turn_zero_and_emits_call_model() {
    let state = LoopState::CallingModel {
        turn: 0,
        usage: TokenUsage::default(),
    };
    let user_msg = Item::UserMessage {
        content: vec![ContentPart::Text { text: "hi".into() }],
    };
    let conversation = vec![user_msg.clone()];
    let input = TransitionInput::Start {
        messages: vec![user_msg],
    };
    let settings = ModelSettings::new();

    let outcome = transition(&state, input, &ctx_with(16, &conversation, &settings));

    assert_matches!(outcome.next_state, LoopState::CallingModel { turn: 0, .. });
    assert_matches!(outcome.next_action, NextAction::CallModel { .. });
    assert_eq!(outcome.events.len(), 1);
    assert_matches!(&outcome.events[0], AgentEvent::TurnStarted { turn: 0 });
}

#[test]
fn model_response_without_tool_calls_terminates() {
    let state = LoopState::CallingModel {
        turn: 0,
        usage: TokenUsage::default(),
    };
    let assistant = Item::AssistantMessage {
        content: vec![ContentPart::Text {
            text: "hello".into(),
        }],
        agent: Some("test".into()),
    };
    let conversation = vec![];
    let settings = ModelSettings::new();
    let input = TransitionInput::ModelResponse {
        items: vec![assistant.clone()],
        usage: TokenUsage::default(),
        finish_reason: FinishReason::Stop,
    };

    let outcome = transition(&state, input, &ctx_with(16, &conversation, &settings));

    assert_matches!(outcome.next_state, LoopState::Done(_));
    assert_matches!(outcome.next_action, NextAction::Terminate);
    // Expected events: MessageOutput { item: AssistantMessage }, RunCompleted { .. }.
    assert_eq!(outcome.events.len(), 2);
    assert_matches!(&outcome.events[0], AgentEvent::MessageOutput { .. });
    assert_matches!(&outcome.events[1], AgentEvent::RunCompleted { .. });
}

#[test]
fn model_response_with_tool_calls_fans_out() {
    let state = LoopState::CallingModel {
        turn: 0,
        usage: TokenUsage::default(),
    };
    let assistant = Item::AssistantMessage {
        content: vec![ContentPart::Text {
            text: "calling tools".into(),
        }],
        agent: Some("test".into()),
    };
    let call_a = Item::ToolCall {
        call_id: "1".into(),
        name: "a".into(),
        args: serde_json::json!({}),
    };
    let call_b = Item::ToolCall {
        call_id: "2".into(),
        name: "b".into(),
        args: serde_json::json!({}),
    };
    let conversation = vec![];
    let settings = ModelSettings::new();
    let input = TransitionInput::ModelResponse {
        items: vec![assistant, call_a, call_b],
        usage: paigasus_helikon_core::TokenUsage::default(),
        finish_reason: paigasus_helikon_core::FinishReason::ToolCalls,
    };

    let outcome = transition(&state, input, &ctx_with(16, &conversation, &settings));

    match outcome.next_state {
        LoopState::ExecutingTools {
            ref calls, turn, ..
        } => {
            assert_eq!(calls.len(), 2);
            assert_eq!(turn, 0);
        }
        other => panic!("expected ExecutingTools, got {other:?}"),
    }
    match outcome.next_action {
        NextAction::ExecuteTools { ref calls } => assert_eq!(calls.len(), 2),
        ref other => panic!("expected ExecuteTools, got {other:?}"),
    }
    // Expected events: MessageOutput, ToolCallItem, ToolCallItem.
    assert_eq!(outcome.events.len(), 3);
    assert_matches!(&outcome.events[0], AgentEvent::MessageOutput { .. });
    assert_matches!(&outcome.events[1], AgentEvent::ToolCallItem { .. });
    assert_matches!(&outcome.events[2], AgentEvent::ToolCallItem { .. });
}

#[test]
fn tool_results_advance_to_next_call_model() {
    let calls = vec![
        ToolCallRequest {
            call_id: "1".into(),
            name: "a".into(),
            args: serde_json::json!({}),
        },
        ToolCallRequest {
            call_id: "2".into(),
            name: "b".into(),
            args: serde_json::json!({}),
        },
    ];
    let state = LoopState::ExecutingTools {
        calls: calls.clone(),
        turn: 0,
        usage: TokenUsage::default(),
    };
    let outcomes = vec![
        ToolCallOutcome {
            call_id: "1".into(),
            result: Ok(vec![ContentPart::Text {
                text: "ok-a".into(),
            }]),
        },
        ToolCallOutcome {
            call_id: "2".into(),
            result: Ok(vec![ContentPart::Text {
                text: "ok-b".into(),
            }]),
        },
    ];
    let conversation = vec![];
    let settings = ModelSettings::new();
    let input = TransitionInput::ToolResults { outcomes };

    let outcome = transition(&state, input, &ctx_with(16, &conversation, &settings));

    assert_matches!(outcome.next_state, LoopState::CallingModel { turn: 1, .. });
    assert_matches!(outcome.next_action, NextAction::CallModel { .. });
    // Expected: ToolOutputItem × 2 + TurnStarted { turn: 1 }
    assert_eq!(outcome.events.len(), 3);
    assert_matches!(&outcome.events[0], AgentEvent::ToolOutputItem { .. });
    assert_matches!(&outcome.events[1], AgentEvent::ToolOutputItem { .. });
    assert_matches!(&outcome.events[2], AgentEvent::TurnStarted { turn: 1 });
}

#[test]
fn calling_model_at_max_turns_fails() {
    let max_turns = 4;
    let state = LoopState::CallingModel {
        turn: max_turns,
        usage: TokenUsage::default(),
    };
    let conversation = vec![];
    let settings = ModelSettings::new();
    let input = TransitionInput::Start { messages: vec![] };

    let outcome = transition(
        &state,
        input,
        &ctx_with(max_turns, &conversation, &settings),
    );

    match outcome.next_state {
        LoopState::Failed(paigasus_helikon_core::AgentError::MaxTurnsExceeded(n)) => {
            assert_eq!(n, max_turns);
        }
        other => panic!("expected Failed(MaxTurnsExceeded), got {other:?}"),
    }
    assert_matches!(outcome.next_action, NextAction::Terminate);
    assert_eq!(outcome.events.len(), 1);
    assert_matches!(&outcome.events[0], AgentEvent::RunFailed { .. });
}

#[test]
fn compacting_surfaces_not_implemented() {
    let state = LoopState::Compacting;
    let conversation = vec![];
    let settings = ModelSettings::new();
    let outcome = transition(
        &state,
        TransitionInput::Start { messages: vec![] },
        &ctx_with(16, &conversation, &settings),
    );

    match outcome.next_state {
        LoopState::Failed(paigasus_helikon_core::AgentError::NotImplemented { feature }) => {
            assert_eq!(feature, "compaction");
        }
        other => panic!("expected Failed(NotImplemented), got {other:?}"),
    }
    assert_matches!(outcome.next_action, NextAction::Terminate);
}

#[test]
fn needs_approval_surfaces_not_implemented() {
    let state = LoopState::NeedsApproval { pending: vec![] };
    let conversation = vec![];
    let settings = ModelSettings::new();
    let outcome = transition(
        &state,
        TransitionInput::Start { messages: vec![] },
        &ctx_with(16, &conversation, &settings),
    );

    match outcome.next_state {
        LoopState::Failed(paigasus_helikon_core::AgentError::NotImplemented { feature }) => {
            assert_eq!(feature, "approval");
        }
        other => panic!("expected Failed(NotImplemented), got {other:?}"),
    }
    assert_matches!(outcome.next_action, NextAction::Terminate);
}

#[test]
fn tool_results_at_max_turns_preserves_outputs_before_failing() {
    let max_turns = 4u32;
    let calls = vec![
        ToolCallRequest {
            call_id: "1".into(),
            name: "a".into(),
            args: serde_json::json!({}),
        },
        ToolCallRequest {
            call_id: "2".into(),
            name: "b".into(),
            args: serde_json::json!({}),
        },
    ];
    // turn = max_turns - 1, so next_turn = max_turns trips the budget.
    let state = LoopState::ExecutingTools {
        calls: calls.clone(),
        turn: max_turns - 1,
        usage: TokenUsage::default(),
    };
    let outcomes = vec![
        ToolCallOutcome {
            call_id: "1".into(),
            result: Ok(vec![ContentPart::Text {
                text: "ok-a".into(),
            }]),
        },
        ToolCallOutcome {
            call_id: "2".into(),
            result: Ok(vec![ContentPart::Text {
                text: "ok-b".into(),
            }]),
        },
    ];
    let conversation = vec![];
    let settings = ModelSettings::new();
    let input = TransitionInput::ToolResults { outcomes };

    let outcome = transition(
        &state,
        input,
        &ctx_with(max_turns, &conversation, &settings),
    );

    match outcome.next_state {
        LoopState::Failed(paigasus_helikon_core::AgentError::MaxTurnsExceeded(n)) => {
            assert_eq!(n, max_turns);
        }
        other => panic!("expected Failed(MaxTurnsExceeded), got {other:?}"),
    }
    assert_matches!(outcome.next_action, NextAction::Terminate);
    // Expected events: ToolOutputItem × 2, then RunFailed.
    assert_eq!(outcome.events.len(), 3);
    assert_matches!(&outcome.events[0], AgentEvent::ToolOutputItem { .. });
    assert_matches!(&outcome.events[1], AgentEvent::ToolOutputItem { .. });
    assert_matches!(&outcome.events[2], AgentEvent::RunFailed { .. });
}

// --- SMA-320 structured-output finalizing request shape tests ---

/// AC#1: when `output` is `Some` and `tools` is empty, the Start arm emits a
/// `Finalizing` next-state whose `CallModel` request carries `response_format:
/// JsonSchema { strict: true, name: "Answer", .. }` and an empty tools list.
#[test]
fn finalizing_request_carries_response_format_and_no_tools() {
    let output_type = OutputType::from_schema::<Answer>();
    let settings = ModelSettings::new();
    let conversation: Vec<Item> = vec![];
    let ctx = TransitionCtx {
        tools: &[],
        model_settings: &settings,
        max_turns: 16,
        conversation: &conversation,
        output: Some(&output_type),
        handoffs: &[],
    };

    let state = LoopState::CallingModel {
        turn: 0,
        usage: TokenUsage::default(),
    };
    let input = TransitionInput::Start { messages: vec![] };
    let outcome = transition(&state, input, &ctx);

    // State must become Finalizing.
    assert_matches!(outcome.next_state, LoopState::Finalizing { .. });

    // Action must be CallModel with an empty tools list and JsonSchema response_format.
    match outcome.next_action {
        NextAction::CallModel { request } => {
            assert!(
                request.tools.is_empty(),
                "finalizing request must have no tools, got {:?}",
                request.tools
            );
            match request.model_settings.response_format {
                Some(ResponseFormat::JsonSchema { name, strict, .. }) => {
                    assert_eq!(name, "Answer");
                    assert!(strict, "strict must be true on the finalizing request");
                }
                other => panic!("expected ResponseFormat::JsonSchema, got {:?}", other),
            }
        }
        other => panic!("expected NextAction::CallModel, got {:?}", other),
    }
}

/// SMA-402: the running usage total is carried forward and summed across
/// turns by `transition`, surfacing the cumulative total on `Done` /
/// `RunCompleted` — not the last turn only.
#[test]
fn usage_accumulates_across_turns() {
    // TokenUsage is #[non_exhaustive]: build via default + field assignment.
    let mut u0 = TokenUsage::default();
    u0.input_tokens = 100;
    u0.output_tokens = 20;
    u0.total_tokens = 120;

    let mut u1 = TokenUsage::default();
    u1.input_tokens = 200;
    u1.output_tokens = 8;
    u1.total_tokens = 208;

    let settings = ModelSettings::new();
    let conversation: Vec<Item> = vec![];

    // Turn 0: model emits a tool call (usage u0) → ExecutingTools carries u0.
    let assistant = Item::AssistantMessage {
        content: vec![ContentPart::Text {
            text: "calling".into(),
        }],
        agent: Some("test".into()),
    };
    let call = Item::ToolCall {
        call_id: "1".into(),
        name: "a".into(),
        args: serde_json::json!({}),
    };
    let state0 = LoopState::CallingModel {
        turn: 0,
        usage: TokenUsage::default(),
    };
    let out0 = transition(
        &state0,
        TransitionInput::ModelResponse {
            items: vec![assistant, call],
            usage: u0,
            finish_reason: FinishReason::ToolCalls,
        },
        &ctx_with(16, &conversation, &settings),
    );
    let exec_usage = match &out0.next_state {
        LoopState::ExecutingTools { usage, .. } => *usage,
        other => panic!("expected ExecutingTools, got {other:?}"),
    };
    assert_eq!(exec_usage.input_tokens, 100);
    assert_eq!(exec_usage.total_tokens, 120);

    // Tool results → CallingModel { turn: 1 } carries u0 forward unchanged.
    let out1 = transition(
        &out0.next_state,
        TransitionInput::ToolResults {
            outcomes: vec![ToolCallOutcome {
                call_id: "1".into(),
                result: Ok(vec![ContentPart::Text { text: "ok".into() }]),
            }],
        },
        &ctx_with(16, &conversation, &settings),
    );
    let call1_usage = match &out1.next_state {
        LoopState::CallingModel { turn: 1, usage } => *usage,
        other => panic!("expected CallingModel turn 1, got {other:?}"),
    };
    assert_eq!(call1_usage.input_tokens, 100, "tools add no tokens");

    // Turn 1: final text (usage u1) → Done with cumulative u0 + u1.
    let final_assistant = Item::AssistantMessage {
        content: vec![ContentPart::Text {
            text: "done".into(),
        }],
        agent: Some("test".into()),
    };
    let out2 = transition(
        &out1.next_state,
        TransitionInput::ModelResponse {
            items: vec![final_assistant],
            usage: u1,
            finish_reason: FinishReason::Stop,
        },
        &ctx_with(16, &conversation, &settings),
    );
    let final_usage = match &out2.next_state {
        LoopState::Done(fo) => fo.usage,
        other => panic!("expected Done, got {other:?}"),
    };
    assert_eq!(final_usage.input_tokens, 300);
    assert_eq!(final_usage.output_tokens, 28);
    assert_eq!(final_usage.total_tokens, 328);

    // The RunCompleted event carries the same cumulative total.
    match out2
        .events
        .iter()
        .find(|e| matches!(e, AgentEvent::RunCompleted { .. }))
    {
        Some(AgentEvent::RunCompleted { usage }) => {
            assert_eq!(usage.input_tokens, 300);
            assert_eq!(usage.total_tokens, 328);
        }
        _ => panic!("expected a RunCompleted event"),
    }
}

/// D6 precedence: when the caller pre-sets `model_settings.response_format`
/// to `Text`, the finalizing request must still carry `JsonSchema` derived
/// from `output_type` — the output_type wins.
#[test]
fn output_type_overrides_caller_response_format_on_finalizing_turn() {
    let output_type = OutputType::from_schema::<Answer>();
    // Caller explicitly sets Text — should be overridden.
    let mut settings = ModelSettings::new();
    settings.response_format = Some(ResponseFormat::Text);
    let conversation: Vec<Item> = vec![];
    let ctx = TransitionCtx {
        tools: &[],
        model_settings: &settings,
        max_turns: 16,
        conversation: &conversation,
        output: Some(&output_type),
        handoffs: &[],
    };

    let state = LoopState::CallingModel {
        turn: 0,
        usage: TokenUsage::default(),
    };
    let input = TransitionInput::Start { messages: vec![] };
    let outcome = transition(&state, input, &ctx);

    assert_matches!(outcome.next_state, LoopState::Finalizing { .. });

    match outcome.next_action {
        NextAction::CallModel { request } => {
            match request.model_settings.response_format {
                Some(ResponseFormat::JsonSchema { name, strict, .. }) => {
                    assert_eq!(name, "Answer", "output_type name must prevail over caller Text");
                    assert!(strict);
                }
                other => panic!(
                    "expected ResponseFormat::JsonSchema (output_type must win over caller Text), got {:?}",
                    other
                ),
            }
        }
        other => panic!("expected NextAction::CallModel, got {:?}", other),
    }
}

#[test]
fn model_response_with_transfer_call_routes_to_applying_handoff() {
    use paigasus_helikon_core::{
        transition, AgentEvent, ContentPart, HandoffDef, Item, LoopState, ModelSettings,
        NextAction, TokenUsage, TransitionCtx, TransitionInput,
    };

    let defs = vec![HandoffDef {
        tool_name: "transfer_to_budgeting_specialist".to_owned(),
        target: "budgeting specialist".to_owned(),
        description: "Handles budgeting.".to_owned(),
    }];
    let conversation = vec![
        Item::System {
            content: vec![ContentPart::Text {
                text: "sys".to_owned(),
            }],
        },
        Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "help me budget".to_owned(),
            }],
        },
        Item::AssistantMessage {
            content: vec![ContentPart::Text {
                text: "routing".to_owned(),
            }],
            agent: Some("triage".to_owned()),
        },
        Item::ToolCall {
            call_id: "c1".to_owned(),
            name: "transfer_to_budgeting_specialist".to_owned(),
            args: serde_json::json!({}),
        },
    ];
    let settings = ModelSettings::default();
    let ctx = TransitionCtx {
        tools: &[],
        model_settings: &settings,
        max_turns: 16,
        conversation: &conversation,
        output: None,
        handoffs: &defs,
    };
    let state = LoopState::CallingModel {
        turn: 0,
        usage: TokenUsage::default(),
    };
    let input = TransitionInput::ModelResponse {
        items: vec![
            Item::AssistantMessage {
                content: vec![ContentPart::Text {
                    text: "routing".to_owned(),
                }],
                agent: Some("triage".to_owned()),
            },
            Item::ToolCall {
                call_id: "c1".to_owned(),
                name: "transfer_to_budgeting_specialist".to_owned(),
                args: serde_json::json!({}),
            },
        ],
        usage: TokenUsage::default(),
        finish_reason: paigasus_helikon_core::FinishReason::ToolCalls,
    };

    let outcome = transition(&state, input, &ctx);
    assert!(matches!(outcome.next_action, NextAction::Handoff));
    match outcome.next_state {
        LoopState::ApplyingHandoff {
            target, transcript, ..
        } => {
            assert_eq!(target, "budgeting specialist");
            assert!(!transcript
                .iter()
                .any(|i| matches!(i, Item::System { .. } | Item::ToolCall { .. })));
            match transcript.last() {
                Some(Item::UserMessage { content }) => {
                    let text = match &content[0] {
                        ContentPart::Text { text } => text.as_str(),
                        _ => "",
                    };
                    assert!(text.to_lowercase().contains("transferred"));
                }
                other => panic!("expected trailing transfer note, got {other:?}"),
            }
        }
        other => panic!("expected ApplyingHandoff, got {other:?}"),
    }
    assert!(outcome
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallItem { .. })));
}

#[test]
fn model_response_prefers_handoff_over_regular_tool_call() {
    use paigasus_helikon_core::{
        transition, ContentPart, HandoffDef, Item, LoopState, ModelSettings, NextAction,
        TokenUsage, TransitionCtx, TransitionInput,
    };

    let defs = vec![HandoffDef {
        tool_name: "transfer_to_budgeting_specialist".to_owned(),
        target: "budgeting specialist".to_owned(),
        description: "Handles budgeting.".to_owned(),
    }];
    let conversation = vec![
        Item::System {
            content: vec![ContentPart::Text {
                text: "sys".to_owned(),
            }],
        },
        Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "help me budget".to_owned(),
            }],
        },
    ];
    let settings = ModelSettings::default();
    let ctx = TransitionCtx {
        tools: &[],
        model_settings: &settings,
        max_turns: 16,
        conversation: &conversation,
        output: None,
        handoffs: &defs,
    };
    let state = LoopState::CallingModel {
        turn: 0,
        usage: TokenUsage::default(),
    };
    // Response contains BOTH a regular tool call AND a handoff tool call.
    // The handoff must win; the regular call is dropped.
    let input = TransitionInput::ModelResponse {
        items: vec![
            Item::AssistantMessage {
                content: vec![ContentPart::Text {
                    text: "let me look that up and route you".to_owned(),
                }],
                agent: Some("triage".to_owned()),
            },
            Item::ToolCall {
                call_id: "c0".to_owned(),
                name: "lookup_spending".to_owned(),
                args: serde_json::json!({"period": "last_month"}),
            },
            Item::ToolCall {
                call_id: "c1".to_owned(),
                name: "transfer_to_budgeting_specialist".to_owned(),
                args: serde_json::json!({}),
            },
        ],
        usage: TokenUsage::default(),
        finish_reason: paigasus_helikon_core::FinishReason::ToolCalls,
    };

    let outcome = transition(&state, input, &ctx);

    // Handoff must win: action is Handoff, not ExecuteTools.
    assert!(
        matches!(outcome.next_action, NextAction::Handoff),
        "expected Handoff, got {:?}",
        outcome.next_action
    );
    match outcome.next_state {
        LoopState::ApplyingHandoff { target, .. } => {
            assert_eq!(
                target, "budgeting specialist",
                "handoff target must be the budgeting specialist"
            );
        }
        other => panic!("expected ApplyingHandoff, got {other:?}"),
    }
}
