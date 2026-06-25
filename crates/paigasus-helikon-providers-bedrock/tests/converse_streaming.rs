//! Integration tests for `StreamTranslator` — Bedrock `ConverseStreamOutput`
//! events → `paigasus_helikon_core::ModelEvent`.
//!
//! All events are constructed via the AWS SDK builders so no mock transport is
//! needed; the translator is pure and exercised synchronously.

use aws_sdk_bedrockruntime::types::{
    ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStart, ContentBlockStartEvent,
    ContentBlockStopEvent, ConversationRole, ConverseStreamMetadataEvent, ConverseStreamOutput,
    MessageStartEvent, MessageStopEvent, ReasoningContentBlockDelta, StopReason, TokenUsage,
    ToolUseBlockDelta, ToolUseBlockStart,
};
use paigasus_helikon_core::{FinishReason, ModelError, ModelEvent};
use paigasus_helikon_providers_bedrock::testing::StreamTranslator;

// ── helpers ───────────────────────────────────────────────────────────────────

fn text_delta(idx: i32, text: &str) -> ConverseStreamOutput {
    ConverseStreamOutput::ContentBlockDelta(
        ContentBlockDeltaEvent::builder()
            .delta(ContentBlockDelta::Text(text.to_owned()))
            .content_block_index(idx)
            .build()
            .unwrap(),
    )
}

fn tool_use_start(idx: i32, id: &str, name: &str) -> ConverseStreamOutput {
    let tu_start = ToolUseBlockStart::builder()
        .tool_use_id(id)
        .name(name)
        .build()
        .unwrap();
    ConverseStreamOutput::ContentBlockStart(
        ContentBlockStartEvent::builder()
            .start(ContentBlockStart::ToolUse(tu_start))
            .content_block_index(idx)
            .build()
            .unwrap(),
    )
}

fn tool_delta(idx: i32, partial: &str) -> ConverseStreamOutput {
    ConverseStreamOutput::ContentBlockDelta(
        ContentBlockDeltaEvent::builder()
            .delta(ContentBlockDelta::ToolUse(
                ToolUseBlockDelta::builder().input(partial).build().unwrap(),
            ))
            .content_block_index(idx)
            .build()
            .unwrap(),
    )
}

fn reasoning_delta(idx: i32, text: &str) -> ConverseStreamOutput {
    ConverseStreamOutput::ContentBlockDelta(
        ContentBlockDeltaEvent::builder()
            .delta(ContentBlockDelta::ReasoningContent(
                ReasoningContentBlockDelta::Text(text.to_owned()),
            ))
            .content_block_index(idx)
            .build()
            .unwrap(),
    )
}

fn message_stop(reason: StopReason) -> ConverseStreamOutput {
    ConverseStreamOutput::MessageStop(
        MessageStopEvent::builder()
            .stop_reason(reason)
            .build()
            .unwrap(),
    )
}

fn metadata(input: i32, output: i32, cached: Option<i32>) -> ConverseStreamOutput {
    let mut b = TokenUsage::builder()
        .input_tokens(input)
        .output_tokens(output)
        .total_tokens(input + output);
    if let Some(c) = cached {
        b = b.cache_read_input_tokens(c);
    }
    let usage = b.build().unwrap();
    ConverseStreamOutput::Metadata(ConverseStreamMetadataEvent::builder().usage(usage).build())
}

fn content_block_stop(idx: i32) -> ConverseStreamOutput {
    ConverseStreamOutput::ContentBlockStop(
        ContentBlockStopEvent::builder()
            .content_block_index(idx)
            .build()
            .unwrap(),
    )
}

fn message_start() -> ConverseStreamOutput {
    ConverseStreamOutput::MessageStart(
        MessageStartEvent::builder()
            .role(ConversationRole::Assistant)
            .build()
            .unwrap(),
    )
}

/// Drive the translator through a sequence, collect all output events in order.
fn run(
    synthesizing: bool,
    events: Vec<ConverseStreamOutput>,
) -> Vec<Result<ModelEvent, ModelError>> {
    let mut t = StreamTranslator::new(synthesizing);
    let mut out = Vec::new();
    for ev in events {
        out.extend(t.consume(ev));
    }
    out
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Text-only turn: Bedrock order is ...deltas... MessageStop Metadata.
/// Translator buffers the stop reason on MessageStop, then emits Usage+Finish
/// when Metadata arrives — ensuring Usage always precedes Finish.
#[test]
fn text_only_emits_token_deltas_then_usage_then_finish() {
    let events = vec![
        message_start(),
        text_delta(0, "Hel"),
        text_delta(0, "lo"),
        content_block_stop(0),
        message_stop(StopReason::EndTurn),
        metadata(10, 5, None),
    ];
    let out = run(false, events);

    // Expect: TokenDelta "Hel", TokenDelta "lo", Usage, Finish(Stop)
    assert_eq!(out.len(), 4, "expected 4 events, got {:?}", out);

    match out[0].as_ref().unwrap() {
        ModelEvent::TokenDelta { text } => assert_eq!(text, "Hel"),
        other => panic!("event[0]: expected TokenDelta, got {other:?}"),
    }
    match out[1].as_ref().unwrap() {
        ModelEvent::TokenDelta { text } => assert_eq!(text, "lo"),
        other => panic!("event[1]: expected TokenDelta, got {other:?}"),
    }
    match out[2].as_ref().unwrap() {
        ModelEvent::Usage {
            input_tokens,
            output_tokens,
            cached_input_tokens,
            ..
        } => {
            assert_eq!(*input_tokens, 10);
            assert_eq!(*output_tokens, 5);
            assert_eq!(*cached_input_tokens, None);
        }
        other => panic!("event[2]: expected Usage, got {other:?}"),
    }
    match out[3].as_ref().unwrap() {
        ModelEvent::Finish { reason } => assert_eq!(*reason, FinishReason::Stop),
        other => panic!("event[3]: expected Finish(Stop), got {other:?}"),
    }
}

/// Cached tokens round-trip through Usage.
#[test]
fn metadata_with_cache_sets_cached_input_tokens() {
    let events = vec![message_stop(StopReason::EndTurn), metadata(20, 8, Some(15))];
    let out = run(false, events);
    assert_eq!(out.len(), 2);
    match out[0].as_ref().unwrap() {
        ModelEvent::Usage {
            input_tokens,
            output_tokens,
            cached_input_tokens,
            ..
        } => {
            assert_eq!(*input_tokens, 20);
            assert_eq!(*output_tokens, 8);
            assert_eq!(*cached_input_tokens, Some(15));
        }
        other => panic!("expected Usage, got {other:?}"),
    }
    match out[1].as_ref().unwrap() {
        ModelEvent::Finish { reason } => assert_eq!(*reason, FinishReason::Stop),
        other => panic!("expected Finish, got {other:?}"),
    }
}

/// Metadata before MessageStop: emit Usage immediately, Finish on MessageStop.
#[test]
fn metadata_before_message_stop_emits_usage_immediately_then_finish() {
    let events = vec![
        text_delta(0, "hi"),
        metadata(5, 2, None),
        message_stop(StopReason::EndTurn),
    ];
    let out = run(false, events);
    assert_eq!(out.len(), 3);
    assert!(matches!(
        out[0].as_ref().unwrap(),
        ModelEvent::TokenDelta { .. }
    ));
    assert!(matches!(out[1].as_ref().unwrap(), ModelEvent::Usage { .. }));
    assert!(
        matches!(out[2].as_ref().unwrap(), ModelEvent::Finish { reason } if *reason == FinishReason::Stop)
    );
}

/// StopSequence maps to Stop.
#[test]
fn stop_sequence_maps_to_finish_stop() {
    let events = vec![message_stop(StopReason::StopSequence), metadata(1, 1, None)];
    let out = run(false, events);
    assert!(matches!(
        out.last().unwrap().as_ref().unwrap(),
        ModelEvent::Finish { reason } if *reason == FinishReason::Stop
    ));
}

/// Parallel tool calls: two ToolUse starts at different content-block indices,
/// followed by interleaved input deltas.  Correct call_ids must be emitted.
#[test]
fn parallel_tool_calls_emit_correct_call_ids() {
    let events = vec![
        tool_use_start(0, "call_a", "search"),
        tool_use_start(1, "call_b", "calc"),
        tool_delta(0, "{\"q\":"),
        tool_delta(1, "{\"n\":"),
        tool_delta(0, "\"rust\"}"),
        tool_delta(1, "42}"),
        content_block_stop(0),
        content_block_stop(1),
        message_stop(StopReason::ToolUse),
        metadata(10, 20, None),
    ];
    let out = run(false, events);

    // First delta per block carries name; subsequent ones do not.
    let tool_events: Vec<_> = out
        .iter()
        .filter_map(|r| {
            if let Ok(ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            }) = r
            {
                Some((call_id.clone(), name.clone(), args_delta.clone()))
            } else {
                None
            }
        })
        .collect();

    // 4 ToolCallDelta events (two per tool)
    assert_eq!(tool_events.len(), 4);

    // Index 0 → first two
    assert_eq!(tool_events[0].0, "call_a");
    assert_eq!(tool_events[0].1.as_deref(), Some("search"));
    assert_eq!(tool_events[0].2, "{\"q\":");

    assert_eq!(tool_events[1].0, "call_b");
    assert_eq!(tool_events[1].1.as_deref(), Some("calc"));
    assert_eq!(tool_events[1].2, "{\"n\":");

    assert_eq!(tool_events[2].0, "call_a");
    assert!(tool_events[2].1.is_none(), "name should not repeat");
    assert_eq!(tool_events[2].2, "\"rust\"}");

    assert_eq!(tool_events[3].0, "call_b");
    assert!(tool_events[3].1.is_none(), "name should not repeat");
    assert_eq!(tool_events[3].2, "42}");

    // Finish reason = ToolCalls
    assert!(matches!(
        out.last().unwrap().as_ref().unwrap(),
        ModelEvent::Finish { reason } if *reason == FinishReason::ToolCalls
    ));
}

/// Reasoning content delta → ReasoningDelta.
#[test]
fn reasoning_delta_emits_reasoning_event() {
    let events = vec![
        reasoning_delta(0, "let me think"),
        message_stop(StopReason::EndTurn),
        metadata(5, 3, None),
    ];
    let out = run(false, events);
    match out[0].as_ref().unwrap() {
        ModelEvent::ReasoningDelta { text } => assert_eq!(text, "let me think"),
        other => panic!("expected ReasoningDelta, got {other:?}"),
    }
}

/// Reasoning Signature delta is silently dropped — no event emitted.
#[test]
fn reasoning_signature_delta_is_dropped() {
    let sig_ev = ConverseStreamOutput::ContentBlockDelta(
        ContentBlockDeltaEvent::builder()
            .delta(ContentBlockDelta::ReasoningContent(
                ReasoningContentBlockDelta::Signature("sig-abc".to_owned()),
            ))
            .content_block_index(0)
            .build()
            .unwrap(),
    );
    let out = run(false, vec![sig_ev]);
    assert!(out.is_empty(), "signature delta should produce no event");
}

/// MaxTokens → Finish(Length).
#[test]
fn max_tokens_maps_to_finish_length() {
    let events = vec![message_stop(StopReason::MaxTokens), metadata(10, 100, None)];
    let out = run(false, events);
    assert!(matches!(
        out.last().unwrap().as_ref().unwrap(),
        ModelEvent::Finish { reason } if *reason == FinishReason::Length
    ));
}

/// GuardrailIntervened → Finish(ContentFilter).
#[test]
fn guardrail_intervened_maps_to_content_filter() {
    let events = vec![
        message_stop(StopReason::GuardrailIntervened),
        metadata(5, 0, None),
    ];
    let out = run(false, events);
    assert!(matches!(
        out.last().unwrap().as_ref().unwrap(),
        ModelEvent::Finish { reason } if *reason == FinishReason::ContentFilter
    ));
}

/// ContentFiltered → Finish(ContentFilter).
#[test]
fn content_filtered_maps_to_content_filter() {
    let events = vec![
        message_stop(StopReason::ContentFiltered),
        metadata(5, 0, None),
    ];
    let out = run(false, events);
    assert!(matches!(
        out.last().unwrap().as_ref().unwrap(),
        ModelEvent::Finish { reason } if *reason == FinishReason::ContentFilter
    ));
}

/// ModelContextWindowExceeded → Finish(Other("model_context_window_exceeded")).
#[test]
fn model_context_window_exceeded_maps_to_other() {
    let events = vec![
        message_stop(StopReason::ModelContextWindowExceeded),
        metadata(5, 0, None),
    ];
    let out = run(false, events);
    assert!(matches!(
        out.last().unwrap().as_ref().unwrap(),
        ModelEvent::Finish {
            reason: FinishReason::Other(s)
        } if s == "model_context_window_exceeded"
    ));
}

/// synthesizing=true: the synthesized tool's input delta is remapped to TokenDelta.
#[test]
fn synthesizing_mode_remaps_synthesized_tool_input_to_token_delta() {
    use paigasus_helikon_providers_bedrock::testing::SYNTHESIZED_TOOL_NAME;

    let events = vec![
        tool_use_start(0, "tu_synth", SYNTHESIZED_TOOL_NAME),
        tool_delta(0, "{\"answer\":"),
        tool_delta(0, "42}"),
        content_block_stop(0),
        message_stop(StopReason::ToolUse),
        metadata(10, 5, None),
    ];
    let out = run(true, events);

    let token_deltas: Vec<_> = out
        .iter()
        .filter_map(|r| {
            if let Ok(ModelEvent::TokenDelta { text }) = r {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(token_deltas, vec!["{\"answer\":", "42}"]);

    // No ToolCallDelta should be emitted.
    assert!(
        !out.iter()
            .any(|r| matches!(r, Ok(ModelEvent::ToolCallDelta { .. }))),
        "no ToolCallDelta expected in synthesizing mode"
    );

    // Finish should be Stop (only synthesized tool fired).
    assert!(matches!(
        out.last().unwrap().as_ref().unwrap(),
        ModelEvent::Finish { reason } if *reason == FinishReason::Stop
    ));
}

/// synthesizing=true: synthesized tool + a real tool → ModelError::Other.
#[test]
fn both_tools_fired_produces_error() {
    use paigasus_helikon_providers_bedrock::testing::SYNTHESIZED_TOOL_NAME;

    let events = vec![
        tool_use_start(0, "tu_synth", SYNTHESIZED_TOOL_NAME),
        tool_use_start(1, "tu_real", "search"),
        message_stop(StopReason::ToolUse),
        metadata(10, 5, None),
    ];
    let out = run(true, events);

    // Should contain a ModelError::Other somewhere.
    let has_error = out.iter().any(|r| matches!(r, Err(ModelError::Other(_))));
    assert!(
        has_error,
        "expected Err(Other) for both-tools-fired; got {out:?}"
    );
}

// NOTE: `ConverseStreamOutput::Unknown` is `#[non_exhaustive]` and cannot be
// constructed outside the AWS SDK crate.  The Unknown arm is exercised by the
// unit test `stream::tests::unknown_event_no_output` inside `src/stream.rs`,
// which has access to the private constructor via the `#[cfg(test)]` helper.

/// MessageStart → no events emitted.
#[test]
fn message_start_produces_no_events() {
    let out = run(false, vec![message_start()]);
    assert!(out.is_empty());
}

/// ContentBlockStop → no events emitted.
#[test]
fn content_block_stop_produces_no_events() {
    let out = run(false, vec![content_block_stop(0)]);
    assert!(out.is_empty());
}
