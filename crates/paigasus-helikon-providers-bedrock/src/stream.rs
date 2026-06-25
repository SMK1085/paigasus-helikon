//! `StreamTranslator` — Bedrock `ConverseStreamOutput` events → `ModelEvent`.
//!
//! ## Usage / Finish ordering
//!
//! The Bedrock Converse API emits events in this order:
//!
//! ```text
//! MessageStart
//! ContentBlockStart | ContentBlockDelta | ContentBlockStop  (interleaved)
//! MessageStop         ← stop_reason lives here
//! Metadata            ← usage lives here (AFTER MessageStop)
//! ```
//!
//! Because `ModelEvent::Usage` must precede `ModelEvent::Finish` (per the
//! `paigasus_helikon_core::Model::invoke` ordering contract), and Bedrock
//! emits `Metadata` **after** `MessageStop`, this translator buffers the
//! stop reason when `MessageStop` arrives and emits nothing until `Metadata`
//! arrives — at that point it emits `Usage` immediately followed by `Finish`.
//!
//! If (contrary to the typical wire order) `Metadata` arrives **before**
//! `MessageStop`, `Usage` is emitted immediately on `Metadata` and `Finish`
//! is emitted immediately on `MessageStop`, which also satisfies the contract.

use std::collections::HashMap;

use aws_sdk_bedrockruntime::types::{
    ContentBlockDelta, ContentBlockStart, ConverseStreamOutput, ReasoningContentBlockDelta,
    StopReason,
};
use paigasus_helikon_core::{FinishReason, ModelError, ModelEvent};

use crate::translate::tools::SYNTHESIZED_TOOL_NAME;

// ── internal block state ──────────────────────────────────────────────────────

/// Per-content-block state keyed by `content_block_index`.
///
/// Reasoning content blocks are identified purely by the
/// `ContentBlockDelta::ReasoningContent` delta variant; no separate block-state
/// entry is needed for them.
#[derive(Debug)]
enum BlockState {
    Text,
    ToolUse {
        call_id: String,
        name: String,
        /// `true` after the first `ToolCallDelta` has been emitted (name
        /// appears only in that first delta; subsequent deltas set it to
        /// `None`).
        name_emitted: bool,
    },
}

// ── public translator ─────────────────────────────────────────────────────────

/// State machine that maps Bedrock `ConverseStreamOutput` events to
/// `paigasus_helikon_core::ModelEvent`s.
///
/// Call [`StreamTranslator::consume`] once per SDK event.  The translator is
/// pure (no I/O) and is intended to be driven from an `async_stream!` wrapper
/// that feeds it chunks from `converse_stream`.
///
/// ## Synthesizing mode
///
/// When `synthesizing = true` the model was asked to fill a JSON schema via a
/// forced-tool call (`__paigasus_structured_output__`).  In that mode the
/// synthesized tool's input deltas are re-emitted as `TokenDelta`s and the
/// terminal `tool_use` stop reason is rewritten to `Stop`.  If a *real* tool
/// fires alongside the synthesized one, `ModelError::Other` is returned
/// (protocol violation).
#[derive(Debug, Default)]
pub struct StreamTranslator {
    /// `content_block_index` → per-block state.
    blocks: HashMap<i32, BlockState>,
    /// Whether we are in structured-output synthesis mode.
    synthesizing: bool,
    /// The content-block index of the synthesized tool's block (if any).
    synthesized_block_index: Option<i32>,
    /// `true` if at least one *real* (non-synthesized) tool block started.
    real_tool_fired: bool,
    /// Buffered stop reason from `MessageStop`; held until `Metadata` arrives
    /// so that `Usage` can be emitted first.
    pending_stop_reason: Option<StopReason>,
    /// `true` once a `Metadata` event has been processed.
    metadata_seen: bool,
}

impl StreamTranslator {
    /// Create a new translator.
    ///
    /// Set `synthesizing` to `true` when the request was built with
    /// structured-output synthesis (forced `__paigasus_structured_output__`
    /// tool call).
    pub fn new(synthesizing: bool) -> Self {
        Self {
            synthesizing,
            ..Default::default()
        }
    }

    /// Consume one `ConverseStreamOutput` event and return zero or more
    /// `ModelEvent`s (or `ModelError`s) produced by it.
    pub fn consume(&mut self, ev: ConverseStreamOutput) -> Vec<Result<ModelEvent, ModelError>> {
        let mut out: Vec<Result<ModelEvent, ModelError>> = Vec::new();

        match ev {
            // ── events that produce no output ─────────────────────────────────
            ConverseStreamOutput::MessageStart(_) => {}
            ConverseStreamOutput::ContentBlockStop(_) => {}

            // ── content block start ───────────────────────────────────────────
            ConverseStreamOutput::ContentBlockStart(e) => {
                let idx = e.content_block_index();
                match e.start() {
                    Some(ContentBlockStart::ToolUse(tu)) => {
                        let call_id = tu.tool_use_id().to_owned();
                        let name = tu.name().to_owned();

                        if self.synthesizing && name == SYNTHESIZED_TOOL_NAME {
                            self.synthesized_block_index = Some(idx);
                        } else {
                            self.real_tool_fired = true;
                            // Emit the name-carrying delta immediately on block
                            // start (mirrors the Anthropic provider's behaviour
                            // of emitting name on the first input-json delta).
                            // We *don't* emit here — we wait for the first
                            // tool-input delta so the call_id→state map is
                            // consistent.
                        }
                        self.blocks.insert(
                            idx,
                            BlockState::ToolUse {
                                call_id,
                                name,
                                name_emitted: false,
                            },
                        );
                    }
                    Some(ContentBlockStart::Image(_) | ContentBlockStart::ToolResult(_)) => {
                        // Not a tool we translate; record nothing.
                    }
                    Some(_) | None => {
                        // Text or unknown start: record as text block.
                        self.blocks.insert(idx, BlockState::Text);
                    }
                }
            }

            // ── content block delta ───────────────────────────────────────────
            ConverseStreamOutput::ContentBlockDelta(e) => {
                let idx = e.content_block_index();
                match e.delta() {
                    Some(ContentBlockDelta::Text(text)) => {
                        out.push(Ok(ModelEvent::TokenDelta { text: text.clone() }));
                    }
                    Some(ContentBlockDelta::ToolUse(tu_delta)) => {
                        let input = tu_delta.input().to_owned();
                        let is_synth = Some(idx) == self.synthesized_block_index;

                        if is_synth {
                            // Remap synthesized tool input to TokenDelta.
                            out.push(Ok(ModelEvent::TokenDelta { text: input }));
                        } else if let Some(BlockState::ToolUse {
                            call_id,
                            name,
                            name_emitted,
                        }) = self.blocks.get_mut(&idx)
                        {
                            let emit_name = if *name_emitted {
                                None
                            } else {
                                *name_emitted = true;
                                Some(name.clone())
                            };
                            out.push(Ok(ModelEvent::ToolCallDelta {
                                call_id: call_id.clone(),
                                name: emit_name,
                                args_delta: input,
                            }));
                        }
                        // If no block state (shouldn't happen for well-formed
                        // streams), silently drop rather than panic.
                    }
                    Some(ContentBlockDelta::ReasoningContent(rc)) => match rc {
                        ReasoningContentBlockDelta::Text(text) => {
                            out.push(Ok(ModelEvent::ReasoningDelta { text: text.clone() }));
                        }
                        ReasoningContentBlockDelta::Signature(_) => {
                            // Signature is a round-trip token for multi-turn
                            // extended-thinking; we drop it here (not yet
                            // plumbed through the session layer).
                            tracing::debug!(
                                target: "paigasus::bedrock::stream",
                                "reasoning signature delta dropped (round-trip not yet supported)",
                            );
                        }
                        ReasoningContentBlockDelta::RedactedContent(_) => {
                            // Encrypted reasoning blob — provider-opaque, drop.
                            tracing::debug!(
                                target: "paigasus::bedrock::stream",
                                "reasoning redacted-content delta dropped",
                            );
                        }
                        _ => {
                            // Forward-compat: ignore unknown reasoning variants.
                        }
                    },
                    // Image, ToolResult, Citation, Unknown deltas — drop.
                    Some(_) | None => {}
                }
            }

            // ── message stop (stop_reason) ────────────────────────────────────
            ConverseStreamOutput::MessageStop(e) => {
                let stop_reason = e.stop_reason().clone();
                if self.metadata_seen {
                    // Metadata already arrived; emit Finish now.
                    out.extend(self.finish_events(stop_reason));
                } else {
                    // Buffer; Metadata will trigger Usage + Finish.
                    self.pending_stop_reason = Some(stop_reason);
                }
            }

            // ── metadata (usage) ──────────────────────────────────────────────
            ConverseStreamOutput::Metadata(e) => {
                self.metadata_seen = true;
                if let Some(usage) = e.usage() {
                    out.push(Ok(ModelEvent::Usage {
                        input_tokens: usage.input_tokens().max(0) as u32,
                        output_tokens: usage.output_tokens().max(0) as u32,
                        cached_input_tokens: usage
                            .cache_read_input_tokens()
                            .map(|n| n.max(0) as u32),
                        reasoning_tokens: None,
                    }));
                }
                // If a buffered stop reason is waiting, emit Finish now.
                if let Some(reason) = self.pending_stop_reason.take() {
                    out.extend(self.finish_events(reason));
                }
            }

            // Forward-compat catch-all: Unknown and any future enum variants
            // added by the SDK are silently ignored.
            _ => {}
        }

        out
    }

    /// Flush any buffered stop reason when the Bedrock stream ends normally
    /// (EOF) without having emitted a `Metadata` event.
    ///
    /// Returns the terminal `Finish` event (or a `ModelError::Other` for the
    /// both-tools-fired error condition) if a stop reason is still pending,
    /// or `None` if the stream already emitted its terminal event through the
    /// normal `Metadata`→`Finish` path.
    ///
    /// **Call this only on the EOF path, not on the cancellation path.**
    /// Cancellation must end the stream without emitting `Finish`.
    pub(crate) fn finish(&mut self) -> Option<Result<ModelEvent, ModelError>> {
        self.pending_stop_reason
            .take()
            .map(|reason| self.finish_events(reason).remove(0))
    }

    // ── private helpers ───────────────────────────────────────────────────────

    /// Translate a `StopReason` into the terminal event(s).
    ///
    /// Returns a single `Ok(Finish{..})` in normal cases, or a single
    /// `Err(ModelError::Other)` when both a real and the synthesized tool
    /// fired in synthesis mode.
    fn finish_events(&self, reason: StopReason) -> Vec<Result<ModelEvent, ModelError>> {
        let finish = match reason {
            StopReason::EndTurn | StopReason::StopSequence => Ok(ModelEvent::Finish {
                reason: FinishReason::Stop,
            }),
            StopReason::ToolUse => {
                if self.synthesizing {
                    if self.real_tool_fired {
                        // Both tools fired — protocol violation.
                        Err(ModelError::Other(anyhow::anyhow!(
                            "bedrock stream: structured-output synthesis: \
                             model fired both a real tool and the synthesized \
                             output tool '__paigasus_structured_output__'"
                        )))
                    } else {
                        // Only synthesized tool fired — rewrite to Stop.
                        Ok(ModelEvent::Finish {
                            reason: FinishReason::Stop,
                        })
                    }
                } else {
                    Ok(ModelEvent::Finish {
                        reason: FinishReason::ToolCalls,
                    })
                }
            }
            StopReason::MaxTokens => Ok(ModelEvent::Finish {
                reason: FinishReason::Length,
            }),
            StopReason::GuardrailIntervened | StopReason::ContentFiltered => {
                Ok(ModelEvent::Finish {
                    reason: FinishReason::ContentFilter,
                })
            }
            other => Ok(ModelEvent::Finish {
                reason: FinishReason::Other(other.as_str().to_owned()),
            }),
        };
        vec![finish]
    }
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use aws_sdk_bedrockruntime::types::{
        ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStart, ContentBlockStartEvent,
        ContentBlockStopEvent, ConversationRole, ConverseStreamMetadataEvent, ConverseStreamOutput,
        MessageStartEvent, MessageStopEvent, ReasoningContentBlockDelta, StopReason, TokenUsage,
        ToolUseBlockDelta, ToolUseBlockStart,
    };
    use paigasus_helikon_core::{FinishReason, ModelError, ModelEvent};

    use super::StreamTranslator;
    use crate::translate::tools::SYNTHESIZED_TOOL_NAME;

    // ── helpers ───────────────────────────────────────────────────────────────

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

    // ── tests ─────────────────────────────────────────────────────────────────

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

    // NOTE: ConverseStreamOutput::Unknown is #[non_exhaustive] and cannot be
    // constructed outside the AWS SDK crate, so it cannot be exercised by a test
    // here; the translator handles it via the match's catch-all arm (no output).

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

    // ── finish() flush tests (Fix 3) ──────────────────────────────────────────

    /// `MessageStop(EndTurn)` with NO subsequent `Metadata` event:
    /// `finish()` must yield `Finish(Stop)` — the consumer must see the
    /// terminal event even when Bedrock closes the stream without metadata.
    #[test]
    fn finish_flushes_pending_stop_reason_without_metadata() {
        let mut t = StreamTranslator::new(false);

        // Feed only MessageStop — no Metadata.
        let out = t.consume(message_stop(StopReason::EndTurn));
        assert!(
            out.is_empty(),
            "MessageStop alone must buffer, not emit immediately"
        );

        // EOF path: call finish() and expect the terminal Finish(Stop).
        let terminal = t
            .finish()
            .expect("finish() must return Some when stop reason is buffered");
        assert!(
            matches!(terminal, Ok(ModelEvent::Finish { ref reason }) if *reason == FinishReason::Stop),
            "expected Finish(Stop), got {terminal:?}"
        );

        // A second call must return None (idempotent drain).
        assert!(
            t.finish().is_none(),
            "finish() must return None after the pending reason was drained"
        );
    }

    /// When the normal `Metadata`→`Finish` path has already fired,
    /// `finish()` must return `None` (nothing left to flush).
    #[test]
    fn finish_returns_none_after_normal_metadata_path() {
        let mut t = StreamTranslator::new(false);
        t.consume(message_stop(StopReason::EndTurn));
        // Metadata arrives — translator drains pending_stop_reason internally.
        let out = t.consume(metadata(10, 5, None));
        // out should contain Usage + Finish.
        assert_eq!(out.len(), 2);

        // finish() must now return None — nothing is pending.
        assert!(
            t.finish().is_none(),
            "finish() must return None when Metadata already drained the pending reason"
        );
    }

    /// `finish()` on the both-tools-fired error path (synthesizing mode):
    /// must surface `ModelError::Other` rather than `Finish`.
    #[test]
    fn finish_surfaces_both_tools_error_without_metadata() {
        let mut t = StreamTranslator::new(true);
        // Both synthesized and real tool fire.
        t.consume(tool_use_start(0, "tu_synth", SYNTHESIZED_TOOL_NAME));
        t.consume(tool_use_start(1, "tu_real", "search"));
        // MessageStop with ToolUse — but no Metadata follows.
        let out = t.consume(message_stop(StopReason::ToolUse));
        assert!(out.is_empty(), "MessageStop alone must buffer");

        let terminal = t.finish().expect("finish() must return Some");
        assert!(
            matches!(terminal, Err(ModelError::Other(_))),
            "expected Err(Other) for both-tools-fired; got {terminal:?}"
        );
    }
}
