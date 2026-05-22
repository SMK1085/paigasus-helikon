//! Pure-function unit tests on `transition(...)`. No tokio, no async,
//! no IO. Locks SMA-314's state-machine determinism guarantees.

use paigasus_helikon_core::{
    transition, AgentEvent, ContentPart, FinishReason, Item, LoopState, ModelSettings, NextAction,
    TokenUsage, ToolCallOutcome, ToolCallRequest, TransitionCtx, TransitionInput,
};

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
    }
}

#[test]
fn start_seeds_turn_zero_and_emits_call_model() {
    let state = LoopState::CallingModel { turn: 0 };
    let user_msg = Item::UserMessage {
        content: vec![ContentPart::Text { text: "hi".into() }],
    };
    let conversation = vec![user_msg.clone()];
    let input = TransitionInput::Start {
        messages: vec![user_msg],
    };
    let settings = ModelSettings::new();

    let outcome = transition(&state, input, &ctx_with(16, &conversation, &settings));

    assert_matches!(outcome.next_state, LoopState::CallingModel { turn: 0 });
    assert_matches!(outcome.next_action, NextAction::CallModel { .. });
    assert_eq!(outcome.events.len(), 1);
    assert_matches!(&outcome.events[0], AgentEvent::TurnStarted { turn: 0 });
}

#[test]
fn model_response_without_tool_calls_terminates() {
    let state = LoopState::CallingModel { turn: 0 };
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
    let state = LoopState::CallingModel { turn: 0 };
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
        LoopState::ExecutingTools { ref calls, turn } => {
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

    assert_matches!(outcome.next_state, LoopState::CallingModel { turn: 1 });
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
    let state = LoopState::CallingModel { turn: max_turns };
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
fn applying_handoff_surfaces_not_implemented() {
    let state = LoopState::ApplyingHandoff {
        target: "other".into(),
        transcript: vec![],
    };
    let conversation = vec![];
    let settings = ModelSettings::new();
    let outcome = transition(
        &state,
        TransitionInput::Start { messages: vec![] },
        &ctx_with(16, &conversation, &settings),
    );

    match outcome.next_state {
        LoopState::Failed(paigasus_helikon_core::AgentError::NotImplemented { feature }) => {
            assert_eq!(feature, "handoff");
        }
        other => panic!("expected Failed(NotImplemented), got {other:?}"),
    }
    assert_matches!(outcome.next_action, NextAction::Terminate);
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
}
