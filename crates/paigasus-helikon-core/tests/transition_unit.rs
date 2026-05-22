//! Pure-function unit tests on `transition(...)`. No tokio, no async,
//! no IO. Locks SMA-314's state-machine determinism guarantees.

use paigasus_helikon_core::{
    AgentEvent, ContentPart, FinishReason, Item, LoopState, ModelSettings,
    NextAction, TokenUsage, TransitionCtx, TransitionInput, transition,
};

macro_rules! assert_matches {
    ($expr:expr, $pat:pat $(,)?) => {
        let val = $expr;
        match val {
            $pat => {}
            other => panic!("assertion failed: {other:?} does not match {}", stringify!($pat)),
        }
    };
}

fn ctx_with<'a>(max_turns: u32, conversation: &'a [Item], settings: &'a ModelSettings) -> TransitionCtx<'a> {
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
    let input = TransitionInput::Start { messages: vec![user_msg] };
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
        content: vec![ContentPart::Text { text: "hello".into() }],
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
        content: vec![ContentPart::Text { text: "calling tools".into() }],
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
