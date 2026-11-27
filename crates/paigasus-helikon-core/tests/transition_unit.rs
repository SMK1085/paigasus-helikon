//! Pure-function unit tests on `transition(...)`. No tokio, no async,
//! no IO. Locks SMA-314's state-machine determinism guarantees.

use paigasus_helikon_core::{
    transition, AgentError, AgentEvent, ContentPart, FinishReason, HandoffDef, Item, LoopState,
    ModelSettings, NextAction, OutputType, PendingToolCall, ResponseFormat, TokenUsage,
    ToolCallOutcome, ToolCallRequest, TransitionCtx, TransitionInput,
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
fn tool_results_batch_ends_with_turn_started() {
    // The turn-boundary checkpoint in the driver depends on this order:
    // every ToolOutputItem first, TurnStarted last.
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
        calls,
        turn: 0,
        usage: TokenUsage::default(),
    };
    let outcomes = ["1", "2"]
        .into_iter()
        .map(|id| ToolCallOutcome {
            call_id: id.into(),
            result: Ok(vec![ContentPart::Text { text: "ok".into() }]),
        })
        .collect();
    let conversation = vec![];
    let settings = ModelSettings::new();
    let outcome = transition(
        &state,
        TransitionInput::ToolResults { outcomes },
        &ctx_with(5, &conversation, &settings),
    );
    assert_matches!(outcome.next_action, NextAction::CallModel { .. });
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
fn needs_approval_is_terminal() {
    let state = LoopState::NeedsApproval { pending: vec![] };
    let conversation = vec![];
    let settings = ModelSettings::new();
    let outcome = transition(
        &state,
        TransitionInput::Start { messages: vec![] },
        &ctx_with(16, &conversation, &settings),
    );

    assert_matches!(
        outcome.next_state,
        LoopState::Failed(paigasus_helikon_core::AgentError::Other(_))
    );
    assert_matches!(outcome.next_action, NextAction::Terminate);
}

#[test]
fn tool_results_with_deferrals_emits_approval_then_run_deferred() {
    let calls = vec![
        ToolCallRequest {
            call_id: "a".into(),
            name: "tool_a".into(),
            args: serde_json::json!({}),
        },
        ToolCallRequest {
            call_id: "b".into(),
            name: "tool_b".into(),
            args: serde_json::json!({"x": 1}),
        },
    ];
    let mut usage = TokenUsage::default();
    usage.input_tokens = 5;
    // turn 15 of 16: a plain ToolResults would trip MaxTurnsExceeded here.
    let state = LoopState::ExecutingTools {
        calls,
        turn: 15,
        usage,
    };
    let outcomes = vec![ToolCallOutcome {
        call_id: "a".into(),
        result: Ok(vec![ContentPart::Text {
            text: "ok-a".into(),
        }]),
    }];
    let deferred = vec![PendingToolCall::new(
        "d-1",
        "b",
        "tool_b",
        serde_json::json!({"x": 1}),
        "wait",
        vec!["root".to_owned()],
    )];
    let conversation = vec![];
    let settings = ModelSettings::new();
    let outcome = transition(
        &state,
        TransitionInput::ToolResultsWithDeferrals {
            outcomes,
            deferred: deferred.clone(),
        },
        &ctx_with(16, &conversation, &settings),
    );

    assert_eq!(outcome.events.len(), 3, "{:?}", outcome.events);
    assert!(
        matches!(
            &outcome.events[0],
            AgentEvent::ToolOutputItem { item: Item::ToolResult { call_id, .. } } if call_id == "a"
        ),
        "{:?}",
        outcome.events[0]
    );
    assert!(
        matches!(
            &outcome.events[1],
            AgentEvent::ApprovalRequested { call_id, tool, args }
                if call_id == "b" && tool == "tool_b" && *args == serde_json::json!({"x": 1})
        ),
        "{:?}",
        outcome.events[1]
    );
    assert!(
        matches!(
            &outcome.events[2],
            AgentEvent::RunDeferred { calls, usage }
                if *calls == deferred && usage.input_tokens == 5
        ),
        "{:?}",
        outcome.events[2]
    );
    match outcome.next_state {
        LoopState::NeedsApproval { pending } => {
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].call_id, "b");
        }
        other => panic!("expected NeedsApproval, got {other:?}"),
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

// --- SMA-864: a cut or filtered turn must not run its tool calls. ---

fn cut_turn_text() -> Item {
    Item::AssistantMessage {
        content: vec![ContentPart::Text {
            text: "partial".into(),
        }],
        agent: Some("test".into()),
    }
}

fn call_item(call_id: &str, name: &str) -> Item {
    Item::ToolCall {
        call_id: call_id.into(),
        name: name.into(),
        args: serde_json::json!({}),
    }
}

fn respond(
    items: Vec<Item>,
    finish_reason: FinishReason,
    handoffs: &[HandoffDef],
) -> paigasus_helikon_core::TransitionOutcome {
    let state = LoopState::CallingModel {
        turn: 0,
        usage: TokenUsage::default(),
    };
    let conversation = vec![];
    let settings = ModelSettings::new();
    let ctx = TransitionCtx {
        tools: &[],
        model_settings: &settings,
        max_turns: 16,
        conversation: &conversation,
        output: None,
        handoffs,
    };
    let input = TransitionInput::ModelResponse {
        items,
        usage: TokenUsage::default(),
        finish_reason,
    };
    transition(&state, input, &ctx)
}

fn assert_not_run(
    outcome: &paigasus_helikon_core::TransitionOutcome,
    reason: FinishReason,
    names: &[&str],
) {
    match &outcome.next_state {
        LoopState::Failed(AgentError::ToolCallsNotRun {
            finish_reason,
            tool_names,
            ..
        }) => {
            assert_eq!(finish_reason.as_ref(), Some(&reason));
            assert_eq!(tool_names, names);
        }
        other => panic!("expected Failed(ToolCallsNotRun), got {other:?}"),
    }
    assert_matches!(&outcome.next_action, NextAction::Terminate);
    assert!(outcome.conversation_appends.is_empty());
    assert_matches!(outcome.events.last().unwrap(), AgentEvent::RunFailed { .. });
    assert!(!outcome
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallItem { .. })));
}

#[test]
fn length_with_one_tool_call_fails_without_running_it() {
    let outcome = respond(
        vec![cut_turn_text(), call_item("1", "search")],
        FinishReason::Length,
        &[],
    );
    assert_not_run(&outcome, FinishReason::Length, &["search"]);
}

#[test]
fn content_filter_with_one_tool_call_fails_without_running_it() {
    let outcome = respond(
        vec![call_item("1", "search")],
        FinishReason::ContentFilter,
        &[],
    );
    assert_not_run(&outcome, FinishReason::ContentFilter, &["search"]);
}

#[test]
fn length_with_three_tool_calls_runs_none() {
    let outcome = respond(
        vec![
            call_item("a", "x"),
            call_item("b", "y"),
            call_item("c", "z"),
        ],
        FinishReason::Length,
        &[],
    );
    assert_not_run(&outcome, FinishReason::Length, &["x", "y", "z"]);
}

#[test]
fn length_with_text_and_handoff_fails_without_handoff() {
    let defs = vec![HandoffDef {
        tool_name: "transfer_to_budgeting_specialist".to_owned(),
        target: "budgeting specialist".to_owned(),
        description: "Handles budgeting.".to_owned(),
    }];
    let outcome = respond(
        vec![
            cut_turn_text(),
            call_item("c1", "transfer_to_budgeting_specialist"),
        ],
        FinishReason::Length,
        &defs,
    );
    assert_not_run(
        &outcome,
        FinishReason::Length,
        &["transfer_to_budgeting_specialist"],
    );
    assert_eq!(outcome.events.len(), 2);
    assert_matches!(&outcome.events[0], AgentEvent::MessageOutput { .. });
    assert_matches!(&outcome.events[1], AgentEvent::RunFailed { .. });
}

#[test]
fn length_without_tool_calls_still_completes() {
    let outcome = respond(vec![cut_turn_text()], FinishReason::Length, &[]);
    assert_matches!(outcome.next_state, LoopState::Done(_));
}

#[test]
fn stop_with_tool_call_still_executes() {
    let outcome = respond(vec![call_item("1", "search")], FinishReason::Stop, &[]);
    assert_matches!(outcome.next_state, LoopState::ExecutingTools { .. });
}

#[test]
fn other_function_call_with_tool_call_still_executes() {
    let outcome = respond(
        vec![call_item("1", "search")],
        FinishReason::Other("function_call".into()),
        &[],
    );
    assert_matches!(outcome.next_state, LoopState::ExecutingTools { .. });
}
