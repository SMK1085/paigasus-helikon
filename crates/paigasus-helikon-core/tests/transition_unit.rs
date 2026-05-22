//! Pure-function unit tests on `transition(...)`. No tokio, no async,
//! no IO. Locks SMA-314's state-machine determinism guarantees.

use paigasus_helikon_core::{
    AgentEvent, ContentPart, Item, LoopState, ModelSettings,
    NextAction, TransitionCtx, TransitionInput, transition,
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
