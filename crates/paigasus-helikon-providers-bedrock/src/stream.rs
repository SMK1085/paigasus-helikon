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
//! Per the `paigasus_helikon_core::Model::invoke` ordering contract,
//! `ModelEvent::Finish` is terminal and `ModelEvent::Usage` may appear
//! anywhere in the stream — most providers emit one immediately before
//! `Finish`. Because Bedrock emits `Metadata` (where usage lives) **after**
//! `MessageStop` (where the stop reason lives), this translator buffers the
//! stop reason when `MessageStop` arrives and emits nothing until `Metadata`
//! arrives. At that point it emits `Usage`, then any safety-net
//! `ToolCallDelta`s for tool blocks that never got a name, then `Finish`.
//! The core contract allows `Usage` anywhere and only requires `Finish` to be
//! terminal, so the deltas between `Usage` and `Finish` are valid.
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
        /// `true` for the structured-output anchor block. It never emits a
        /// `ToolCallDelta`; its input is remapped to `TokenDelta`s.
        synthesized: bool,
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
    /// Whether the request used native structured output (`outputConfig`).
    native: bool,
    /// Whether the request set `tool_choice` to `None`. A tool call is then an error.
    forbid_tool_calls: bool,
    /// `true` once a refused tool call produced its error.
    refused_tool_call: bool,
    /// The content-block index of the synthesized tool's block (if any).
    synthesized_block_index: Option<i32>,
    /// `true` if at least one *real* (non-synthesized) tool block started.
    real_tool_fired: bool,
    /// Buffered stop reason from `MessageStop`; held until `Metadata` arrives
    /// so that `Usage` can be emitted first.
    pending_stop_reason: Option<StopReason>,
    /// `true` once a `Metadata` event has been processed.
    metadata_seen: bool,
    /// `true` once a terminal event (`Finish` or an `Err`) was produced.
    terminal_emitted: bool,
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

    /// Tell the translator that the request used native structured output.
    /// An anchor call then logs one warning, because `outputConfig` did not
    /// constrain the text that the anchor carries.
    pub(crate) fn native(mut self, native: bool) -> Self {
        self.native = native;
        self
    }

    /// Tell the translator that the request set `tool_choice` to `None`.
    /// A tool call from the model then becomes an error.
    pub(crate) fn forbid_tool_calls(mut self, forbid: bool) -> Self {
        self.forbid_tool_calls = forbid;
        self
    }

    /// Consume one `ConverseStreamOutput` event and return zero or more
    /// `ModelEvent`s (or `ModelError`s) produced by it.
    pub fn consume(&mut self, ev: ConverseStreamOutput) -> Vec<Result<ModelEvent, ModelError>> {
        let mut out: Vec<Result<ModelEvent, ModelError>> = Vec::new();

        match ev {
            // ── events that produce no output ─────────────────────────────────
            ConverseStreamOutput::MessageStart(_) => {}

            // ── content block stop ────────────────────────────────────────────
            ConverseStreamOutput::ContentBlockStop(e) => {
                // After a terminal event nothing may follow it.
                if self.terminal_emitted {
                    return out;
                }
                let idx = e.content_block_index();
                if let Some(event) = self.emit_unnamed(idx) {
                    if let ModelEvent::ToolCallDelta { call_id, .. } = &event {
                        tracing::debug!(
                            target: "paigasus::bedrock::stream",
                            index = idx,
                            %call_id,
                            "toolUse block closed with no input delta; emitting its name",
                        );
                    }
                    out.push(Ok(event));
                }
            }

            // ── content block start ───────────────────────────────────────────
            ConverseStreamOutput::ContentBlockStart(e) => {
                let idx = e.content_block_index();
                match e.start() {
                    Some(ContentBlockStart::ToolUse(tu)) => {
                        let call_id = tu.tool_use_id().to_owned();
                        let name = tu.name().to_owned();

                        let synthesized = self.synthesizing && name == SYNTHESIZED_TOOL_NAME;
                        if synthesized {
                            if self.native && self.synthesized_block_index.is_none() {
                                tracing::warn!(
                                    target: "paigasus::bedrock::stream",
                                    "the model called the structured-output anchor on the native path; outputConfig did not constrain this text"
                                );
                            }
                            self.synthesized_block_index = Some(idx);
                        } else if self.forbid_tool_calls {
                            // Record no block: its deltas then emit nothing.
                            // A second refused block at another index is
                            // silent: the error is emitted once per stream.
                            if !self.refused_tool_call {
                                self.refused_tool_call = true;
                                self.terminal_emitted = true;
                                out.push(Err(ModelError::Other(anyhow::anyhow!(
                                    "the model called a tool, but tool_choice is None"
                                ))));
                            }
                            return out;
                        } else {
                            self.real_tool_fired = true;
                            // The name goes out with the first input delta,
                            // or at the block stop when no delta came
                            // (SMA-863). Nothing is emitted at the start, so
                            // the call_id→state map stays consistent.
                        }
                        self.blocks.insert(
                            idx,
                            BlockState::ToolUse {
                                call_id,
                                name,
                                name_emitted: false,
                                synthesized,
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
                            ..
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
    /// both-tools-fired error condition) if a stop reason is still pending.
    /// Returns an empty `Vec` if the stream already emitted its terminal event
    /// through the normal `Metadata`→`Finish` path.
    ///
    /// It can also return safety-net `ToolCallDelta`s, ahead of the terminal
    /// event, that name a tool block left open when the stream ended. It
    /// returns an empty `Vec` after a refused tool call.
    ///
    /// **Call this only on the EOF path, not on the cancellation path.**
    /// Cancellation must end the stream without emitting `Finish`.
    pub(crate) fn finish(&mut self) -> Vec<Result<ModelEvent, ModelError>> {
        match self.pending_stop_reason.take() {
            Some(reason) => self.finish_events(reason),
            None => Vec::new(),
        }
    }

    // ── private helpers ───────────────────────────────────────────────────────

    /// Build the zero-delta `ToolCallDelta` for the tool block at `idx`.
    ///
    /// Returns `Some` only when the block is a non-synthesized `ToolUse` whose
    /// name has not gone out yet, and then sets `name_emitted`. The block stays
    /// in the map so a late delta still finds it.
    fn emit_unnamed(&mut self, idx: i32) -> Option<ModelEvent> {
        match self.blocks.get_mut(&idx) {
            Some(BlockState::ToolUse {
                call_id,
                name,
                name_emitted,
                synthesized,
            }) if !*name_emitted && !*synthesized => {
                *name_emitted = true;
                Some(ModelEvent::ToolCallDelta {
                    call_id: call_id.clone(),
                    name: Some(name.clone()),
                    args_delta: String::new(),
                })
            }
            _ => None,
        }
    }

    /// Translate a `StopReason` into the terminal event(s).
    ///
    /// Returns a single `Ok(Finish{..})` in normal cases, or a single
    /// `Err(ModelError::Other)` when both a real and the synthesized tool
    /// fired in synthesis mode. When the terminal is `Finish(ToolCalls)`, it
    /// first returns one zero-delta `ToolCallDelta` for each tool block that
    /// has no name sent (the safety net), in index order. It sets
    /// `terminal_emitted` when it returns a non-empty `Vec`.
    fn finish_events(&mut self, reason: StopReason) -> Vec<Result<ModelEvent, ModelError>> {
        // A refused tool call already ended the stream with an error. As on the
        // both-tools-fired path, the error replaces the terminal `Finish`.
        if self.refused_tool_call {
            return Vec::new();
        }
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
            // The context window limit cuts the output like `max_tokens`
            // (SMA-864).
            StopReason::MaxTokens | StopReason::ModelContextWindowExceeded => {
                Ok(ModelEvent::Finish {
                    reason: FinishReason::Length,
                })
            }
            StopReason::GuardrailIntervened | StopReason::ContentFiltered => {
                Ok(ModelEvent::Finish {
                    reason: FinishReason::ContentFilter,
                })
            }
            other => Ok(ModelEvent::Finish {
                reason: FinishReason::Other(other.as_str().to_owned()),
            }),
        };
        let mut out = Vec::new();
        // Safety net: a tool block that never got its block stop still has no
        // name on the wire. Name it before the terminal `Finish(ToolCalls)`.
        if matches!(
            &finish,
            Ok(ModelEvent::Finish {
                reason: FinishReason::ToolCalls
            })
        ) {
            let mut unnamed: Vec<i32> = self
                .blocks
                .iter()
                .filter_map(|(idx, block)| match block {
                    BlockState::ToolUse {
                        name_emitted: false,
                        synthesized: false,
                        ..
                    } => Some(*idx),
                    _ => None,
                })
                .collect();
            unnamed.sort_unstable();
            for idx in unnamed {
                if let Some(delta) = self.emit_unnamed(idx) {
                    if let ModelEvent::ToolCallDelta { call_id, .. } = &delta {
                        tracing::warn!(
                            target: "paigasus::bedrock::stream",
                            index = idx,
                            %call_id,
                            "terminal tool_use with an unclosed block that sent no name; emitting its name",
                        );
                    }
                    out.push(Ok(delta));
                }
            }
        }
        out.push(finish);
        self.terminal_emitted = true;
        out
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
    /// when Metadata arrives — Finish stays terminal even though Bedrock's own
    /// wire order puts usage after the stop reason.
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

    /// ModelContextWindowExceeded → Finish(Length): the output is cut at the
    /// context window limit (SMA-864).
    #[test]
    fn model_context_window_exceeded_maps_to_length() {
        let events = vec![
            message_stop(StopReason::ModelContextWindowExceeded),
            metadata(5, 0, None),
        ];
        let out = run(false, events);
        assert!(matches!(
            out.last().unwrap().as_ref().unwrap(),
            ModelEvent::Finish { reason } if *reason == FinishReason::Length
        ));
    }

    /// The malformed stop reasons are not a cut or a filter, so they stay in
    /// `Other` (SMA-864 sweep).
    #[test]
    fn malformed_stop_reasons_stay_other() {
        for (wire, raw) in [
            (StopReason::MalformedModelOutput, "malformed_model_output"),
            (StopReason::MalformedToolUse, "malformed_tool_use"),
        ] {
            let out = run(false, vec![message_stop(wire), metadata(5, 0, None)]);
            assert!(matches!(
                out.last().unwrap().as_ref().unwrap(),
                ModelEvent::Finish {
                    reason: FinishReason::Other(s)
                } if s == raw
            ));
        }
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

    /// A writer that collects the formatted tracing output of a test.
    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Run the anchor events through a translator and return the text deltas
    /// and the captured log output.
    fn run_anchor_captured(native: bool) -> (Vec<String>, String) {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_ansi(false)
            .finish();
        let out = tracing::subscriber::with_default(subscriber, || {
            let mut t = StreamTranslator::new(true);
            if native {
                t = t.native(true);
            }
            let mut out = Vec::new();
            for ev in [
                tool_use_start(0, "tu_synth", SYNTHESIZED_TOOL_NAME),
                tool_delta(0, "{\"answer\":"),
                tool_delta(0, "42}"),
                content_block_stop(0),
                message_stop(StopReason::ToolUse),
                metadata(10, 5, None),
            ] {
                out.extend(t.consume(ev));
            }
            out
        });
        let texts = out
            .into_iter()
            .filter_map(|r| match r {
                Ok(ModelEvent::TokenDelta { text }) => Some(text),
                _ => None,
            })
            .collect();
        let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        (texts, log)
    }

    /// Native path: the anchor call logs one warning and still emits the text.
    #[test]
    fn native_anchor_call_warns_once_and_still_emits_text() {
        let (texts, log) = run_anchor_captured(true);
        assert_eq!(texts, vec!["{\"answer\":", "42}"]);
        assert_eq!(log.matches("WARN").count(), 1, "log: {log}");
        assert!(log.contains("paigasus::bedrock::stream"), "log: {log}");
        assert!(log.contains("structured-output anchor"), "log: {log}");
    }

    /// Forced path: the same events log no warning.
    #[test]
    fn forced_anchor_call_does_not_warn() {
        let (texts, log) = run_anchor_captured(false);
        assert_eq!(texts, vec!["{\"answer\":", "42}"]);
        assert!(!log.contains("WARN"), "log: {log}");
    }

    #[test]
    fn forbid_tool_calls_turns_a_tool_use_into_an_error() {
        let mut t = StreamTranslator::new(false).forbid_tool_calls(true);
        let mut out = Vec::new();
        for ev in [
            tool_use_start(0, "tu_1", "get_balance"),
            tool_delta(0, "{}"),
            message_stop(StopReason::ToolUse),
            metadata(3, 4, None),
        ] {
            out.extend(t.consume(ev));
        }
        assert_eq!(out.len(), 2, "got {out:?}");
        match &out[0] {
            Err(e) => assert!(e.to_string().contains("tool_choice is None")),
            other => panic!("expected Err first, got {other:?}"),
        }
        assert!(matches!(out[1], Ok(ModelEvent::Usage { .. })));
        assert!(!out.iter().any(|r| matches!(
            r,
            Ok(ModelEvent::Finish { .. } | ModelEvent::ToolCallDelta { .. })
        )));
    }

    /// Pins the `Model::invoke` contract: an `Err` is terminal for every event
    /// except `Usage`. A refused tool call yields `[Err, Usage]` and nothing else,
    /// and `finish()` adds no item.
    #[test]
    fn a_refused_tool_call_yields_err_then_usage_and_nothing_else() {
        let mut t = StreamTranslator::new(false).forbid_tool_calls(true);
        let mut out = Vec::new();
        for ev in [
            tool_use_start(0, "tu_1", "get_balance"),
            tool_delta(0, "{}"),
            message_stop(StopReason::ToolUse),
            metadata(3, 4, None),
        ] {
            out.extend(t.consume(ev));
        }
        assert!(t.finish().is_empty(), "finish() must add nothing");
        assert_eq!(out.len(), 2, "got {out:?}");
        assert!(out[0].is_err(), "expected Err first, got {:?}", out[0]);
        assert!(
            matches!(out[1], Ok(ModelEvent::Usage { .. })),
            "expected Usage second, got {:?}",
            out[1]
        );
    }

    #[test]
    fn forbid_tool_calls_finish_at_eof_emits_no_finish() {
        let mut t = StreamTranslator::new(false).forbid_tool_calls(true);
        let mut out = Vec::new();
        for ev in [
            tool_use_start(0, "tu_1", "get_balance"),
            message_stop(StopReason::ToolUse),
        ] {
            out.extend(t.consume(ev));
        }
        assert_eq!(out.len(), 1, "got {out:?}");
        assert!(t.finish().is_empty());
    }

    #[test]
    fn tool_use_without_forbid_is_unchanged() {
        let out = run(
            false,
            vec![
                tool_use_start(0, "tu_1", "get_balance"),
                tool_delta(0, "{}"),
                message_stop(StopReason::ToolUse),
            ],
        );
        assert!(out.iter().all(|r| r.is_ok()));
        assert!(out
            .iter()
            .any(|r| matches!(r, Ok(ModelEvent::ToolCallDelta { .. }))));
    }

    #[test]
    fn forbid_tool_calls_allows_the_synthesized_anchor() {
        let mut t = StreamTranslator::new(true).forbid_tool_calls(true);
        let mut out = Vec::new();
        for ev in [
            tool_use_start(0, "tu_synth", SYNTHESIZED_TOOL_NAME),
            tool_delta(0, "{\"a\":1}"),
            message_stop(StopReason::ToolUse),
        ] {
            out.extend(t.consume(ev));
        }
        assert!(out.iter().all(|r| r.is_ok()), "got {out:?}");
        assert!(out
            .iter()
            .any(|r| matches!(r, Ok(ModelEvent::TokenDelta { .. }))));
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

    /// ContentBlockStop of a block that never started → no events emitted.
    #[test]
    fn content_block_stop_of_unknown_block_produces_no_events() {
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
        let mut fin = t.finish();
        assert_eq!(fin.len(), 1, "got {fin:?}");
        let terminal = fin.remove(0);
        assert!(
            matches!(terminal, Ok(ModelEvent::Finish { ref reason }) if *reason == FinishReason::Stop),
            "expected Finish(Stop), got {terminal:?}"
        );

        // A second call must return empty (idempotent drain).
        assert!(
            t.finish().is_empty(),
            "finish() must return empty after the pending reason was drained"
        );
    }

    /// When the normal `Metadata`→`Finish` path has already fired,
    /// `finish()` must return empty (nothing left to flush).
    #[test]
    fn finish_returns_empty_after_normal_metadata_path() {
        let mut t = StreamTranslator::new(false);
        t.consume(message_stop(StopReason::EndTurn));
        // Metadata arrives — translator drains pending_stop_reason internally.
        let out = t.consume(metadata(10, 5, None));
        // out should contain Usage + Finish.
        assert_eq!(out.len(), 2);

        // finish() must now return empty — nothing is pending.
        assert!(
            t.finish().is_empty(),
            "finish() must return empty when Metadata already drained the pending reason"
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

        let mut fin = t.finish();
        assert_eq!(fin.len(), 1, "got {fin:?}");
        let terminal = fin.remove(0);
        assert!(
            matches!(terminal, Err(ModelError::Other(_))),
            "expected Err(Other) for both-tools-fired; got {terminal:?}"
        );
    }

    /// A text turn through `MessageStop` and `Metadata` sets `terminal_emitted`.
    #[test]
    fn terminal_emitted_is_set_by_finish() {
        let mut t = StreamTranslator::new(false);
        t.consume(message_stop(StopReason::EndTurn));
        t.consume(metadata(1, 1, None));
        assert!(t.terminal_emitted);
    }

    /// A refused tool call pushes its `Err` and sets `terminal_emitted`.
    #[test]
    fn terminal_emitted_is_set_by_refused_tool_call() {
        let mut t = StreamTranslator::new(false).forbid_tool_calls(true);
        t.consume(tool_use_start(0, "tu", "t"));
        assert!(t.terminal_emitted);
    }

    /// The both-tools `Err` is a terminal event and sets `terminal_emitted`.
    #[test]
    fn terminal_emitted_is_set_by_both_tools_error() {
        let mut t = StreamTranslator::new(true);
        let mut out = Vec::new();
        for ev in [
            tool_use_start(0, "tu_synth", SYNTHESIZED_TOOL_NAME),
            tool_use_start(1, "tu_real", "search"),
            message_stop(StopReason::ToolUse),
            metadata(1, 1, None),
        ] {
            out.extend(t.consume(ev));
        }
        assert!(matches!(out.last(), Some(Err(_))), "got {out:?}");
        assert!(t.terminal_emitted);
    }

    /// Collect the tool-call names and the final event of a run.
    fn tool_call_names(out: &[Result<ModelEvent, ModelError>]) -> Vec<String> {
        out.iter()
            .filter_map(|r| match r {
                Ok(ModelEvent::ToolCallDelta {
                    name: Some(name), ..
                }) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Collect the `ToolCallDelta`s of a run as `(name, args_delta)`.
    fn tool_deltas(out: &[Result<ModelEvent, ModelError>]) -> Vec<(Option<String>, String)> {
        out.iter()
            .filter_map(|r| match r {
                Ok(ModelEvent::ToolCallDelta {
                    name, args_delta, ..
                }) => Some((name.clone(), args_delta.clone())),
                _ => None,
            })
            .collect()
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/bedrock/zero_arg_tool_call.raw (SMA-860)
    /// with the one empty input delta removed.
    #[test]
    fn tool_use_without_delta_emits_name_at_block_stop() {
        let out = run(
            false,
            vec![
                message_start(),
                tool_use_start(0, "tu_1", "get_current_time"),
                content_block_stop(0),
                message_stop(StopReason::ToolUse),
                metadata(10, 5, None),
            ],
        );
        assert!(out.iter().all(Result::is_ok), "{out:?}");
        assert_eq!(out.len(), 3, "{out:?}");
        assert!(matches!(
            out[0].as_ref().unwrap(),
            ModelEvent::ToolCallDelta { call_id, name: Some(n), args_delta }
                if call_id == "tu_1" && n == "get_current_time" && args_delta.is_empty()
        ));
        assert!(matches!(out[1].as_ref().unwrap(), ModelEvent::Usage { .. }));
        assert!(matches!(
            out[2].as_ref().unwrap(),
            ModelEvent::Finish { reason } if *reason == FinishReason::ToolCalls
        ));
    }

    /// The captured shape: one empty input delta, then the stop. One name only.
    /// This is the capture zero_arg_tool_call.raw (SMA-860) as a unit test.
    #[test]
    fn captured_empty_delta_still_gives_one_name() {
        let out = run(
            false,
            vec![
                message_start(),
                tool_use_start(0, "tu_1", "get_current_time"),
                tool_delta(0, ""),
                content_block_stop(0),
                message_stop(StopReason::ToolUse),
                metadata(10, 5, None),
            ],
        );
        assert_eq!(tool_call_names(&out), vec!["get_current_time".to_owned()]);
        assert_eq!(
            tool_deltas(&out),
            vec![(Some("get_current_time".to_owned()), String::new())]
        );
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/bedrock/zero_arg_tool_call.raw (SMA-860)
    /// with the one empty input delta removed and the stop reason set to max_tokens.
    #[test]
    fn block_stop_with_max_tokens_gives_named_call_then_length() {
        let out = run(
            false,
            vec![
                message_start(),
                tool_use_start(0, "tu_1", "get_current_time"),
                content_block_stop(0),
                message_stop(StopReason::MaxTokens),
                metadata(10, 5, None),
            ],
        );
        assert_eq!(
            tool_deltas(&out),
            vec![(Some("get_current_time".to_owned()), String::new())]
        );
        assert!(matches!(
            out.last().unwrap().as_ref().unwrap(),
            ModelEvent::Finish { reason } if *reason == FinishReason::Length
        ));
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/bedrock/zero_arg_tool_call.raw (SMA-860)
    /// with the one empty input delta removed and the tool renamed to the synthesized anchor.
    #[test]
    fn synthesized_tool_without_delta_emits_no_tool_call_delta() {
        let out = run(
            true,
            vec![
                message_start(),
                tool_use_start(0, "tu_s", SYNTHESIZED_TOOL_NAME),
                content_block_stop(0),
                message_stop(StopReason::ToolUse),
                metadata(10, 5, None),
            ],
        );
        assert!(
            !out.iter()
                .any(|r| matches!(r, Ok(ModelEvent::ToolCallDelta { .. }))),
            "{out:?}"
        );
        assert!(matches!(
            out.last().unwrap().as_ref().unwrap(),
            ModelEvent::Finish { reason } if *reason == FinishReason::Stop
        ));
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Built from the
    /// zero-delta stream with the block stop sent twice.
    #[test]
    fn repeated_block_stop_gives_one_name() {
        let out = run(
            false,
            vec![
                tool_use_start(0, "tu_1", "get_current_time"),
                content_block_stop(0),
                content_block_stop(0),
                message_stop(StopReason::ToolUse),
                metadata(10, 5, None),
            ],
        );
        assert_eq!(
            tool_deltas(&out),
            vec![(Some("get_current_time".to_owned()), String::new())]
        );
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Built from the
    /// zero-delta stream with a block stop after the terminal event.
    #[test]
    fn block_stop_after_terminal_emits_nothing() {
        let mut t = StreamTranslator::new(false);
        t.consume(tool_use_start(0, "tu_1", "get_current_time"));
        t.consume(message_stop(StopReason::ToolUse));
        t.consume(metadata(10, 5, None));
        assert!(t.terminal_emitted);
        assert!(t.consume(content_block_stop(0)).is_empty());

        let mut t = StreamTranslator::new(true);
        for ev in [
            tool_use_start(0, "tu_synth", SYNTHESIZED_TOOL_NAME),
            tool_use_start(1, "tu_real", "search"),
            message_stop(StopReason::ToolUse),
            metadata(1, 1, None),
        ] {
            t.consume(ev);
        }
        assert!(t.terminal_emitted);
        assert!(t.consume(content_block_stop(1)).is_empty());
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Built from the
    /// zero-delta stream with a second tool block that has an input delta.
    #[test]
    fn parallel_calls_one_without_delta() {
        let out = run(
            false,
            vec![
                tool_use_start(0, "tu_a", "a"),
                tool_delta(0, "{}"),
                content_block_stop(0),
                tool_use_start(1, "tu_b", "b"),
                content_block_stop(1),
                message_stop(StopReason::ToolUse),
                metadata(10, 5, None),
            ],
        );
        assert_eq!(tool_call_names(&out), vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(
            tool_deltas(&out),
            vec![
                (Some("a".to_owned()), "{}".to_owned()),
                (Some("b".to_owned()), String::new()),
            ]
        );
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Built from the
    /// zero-delta stream with `tool_choice` set to `None`.
    #[test]
    fn forbid_tool_calls_block_stop_after_refusal_emits_nothing() {
        let mut t = StreamTranslator::new(false).forbid_tool_calls(true);
        let first = t.consume(tool_use_start(0, "tu_1", "t"));
        assert!(matches!(first.as_slice(), [Err(_)]), "{first:?}");
        assert!(t.consume(content_block_stop(0)).is_empty());
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Built from the
    /// zero-delta stream with an input delta sent after the block stop.
    #[test]
    fn delta_after_block_stop_carries_no_name() {
        let out = run(
            false,
            vec![
                tool_use_start(0, "tu_1", "get_current_time"),
                content_block_stop(0),
                tool_delta(0, "{}"),
                message_stop(StopReason::ToolUse),
                metadata(10, 5, None),
            ],
        );
        assert_eq!(
            tool_deltas(&out),
            vec![
                (Some("get_current_time".to_owned()), String::new()),
                (None, "{}".to_owned()),
            ]
        );
    }

    /// A typed memory call on a native structured request is a normal tool
    /// call. The stream translator needs no special case for it.
    #[test]
    fn typed_memory_call_with_structured_native_request_finishes_with_tool_calls() {
        let mut t = StreamTranslator::new(false).native(true);
        let mut out = Vec::new();
        for ev in [
            tool_use_start(0, "tu_1", "memory"),
            tool_delta(0, "{\"command\":\"view\",\"path\":\"/memories\"}"),
            content_block_stop(0),
            message_stop(StopReason::ToolUse),
            metadata(10, 5, None),
        ] {
            out.extend(t.consume(ev));
        }
        assert!(out.iter().all(Result::is_ok), "{out:?}");
        assert_eq!(tool_call_names(&out), vec!["memory".to_owned()]);
        assert!(matches!(
            out.last().unwrap().as_ref().unwrap(),
            ModelEvent::Finish { reason } if *reason == FinishReason::ToolCalls
        ));
    }

    /// With `synthesizing = false`, a call to the placeholder name is a
    /// normal tool call. Only `synthesizing = true` remaps it.
    #[test]
    fn placeholder_call_without_synthesis_is_a_normal_tool_call() {
        let out = run(
            false,
            vec![
                tool_use_start(0, "tu_p", SYNTHESIZED_TOOL_NAME),
                tool_delta(0, "{}"),
                content_block_stop(0),
                message_stop(StopReason::ToolUse),
                metadata(10, 5, None),
            ],
        );
        assert!(out.iter().all(Result::is_ok), "{out:?}");
        assert_eq!(
            tool_call_names(&out),
            vec![SYNTHESIZED_TOOL_NAME.to_owned()]
        );
        assert!(matches!(
            out.last().unwrap().as_ref().unwrap(),
            ModelEvent::Finish { reason } if *reason == FinishReason::ToolCalls
        ));
    }

    /// Assert that `ev` is a named, zero-argument `ToolCallDelta`.
    fn assert_named_delta(ev: &Result<ModelEvent, ModelError>, id: &str, tool: &str) {
        assert!(
            matches!(
                ev.as_ref().unwrap(),
                ModelEvent::ToolCallDelta { call_id, name: Some(n), args_delta }
                    if call_id == id && n == tool && args_delta.is_empty()
            ),
            "{ev:?}"
        );
    }

    /// Assert that `ev` is `Finish(ToolCalls)`.
    fn assert_finish_tool_calls(ev: &Result<ModelEvent, ModelError>) {
        assert!(
            matches!(
                ev.as_ref().unwrap(),
                ModelEvent::Finish { reason } if *reason == FinishReason::ToolCalls
            ),
            "{ev:?}"
        );
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/bedrock/zero_arg_tool_call.raw (SMA-860)
    /// with the one empty input delta removed and the block stop removed.
    #[test]
    fn net_names_unclosed_block_metadata_after_message_stop() {
        let out = run(
            false,
            vec![
                message_start(),
                tool_use_start(0, "tu_1", "get_current_time"),
                message_stop(StopReason::ToolUse),
                metadata(10, 5, None),
            ],
        );
        assert_eq!(out.len(), 3, "{out:?}");
        assert!(matches!(out[0].as_ref().unwrap(), ModelEvent::Usage { .. }));
        assert_named_delta(&out[1], "tu_1", "get_current_time");
        assert_finish_tool_calls(&out[2]);
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/bedrock/zero_arg_tool_call.raw (SMA-860)
    /// with the one empty input delta removed and the block stop removed, and with
    /// the metadata event moved before the message stop.
    #[test]
    fn net_names_unclosed_block_message_stop_after_metadata() {
        let out = run(
            false,
            vec![
                message_start(),
                tool_use_start(0, "tu_1", "get_current_time"),
                metadata(10, 5, None),
                message_stop(StopReason::ToolUse),
            ],
        );
        assert_eq!(out.len(), 3, "{out:?}");
        assert!(matches!(out[0].as_ref().unwrap(), ModelEvent::Usage { .. }));
        assert_named_delta(&out[1], "tu_1", "get_current_time");
        assert_finish_tool_calls(&out[2]);
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/bedrock/zero_arg_tool_call.raw (SMA-860)
    /// with the one empty input delta removed and the block stop removed, and with
    /// the metadata event removed so the stream ends at EOF.
    #[test]
    fn net_names_unclosed_block_on_finish() {
        let mut t = StreamTranslator::new(false);
        let mut out = Vec::new();
        for ev in [
            message_start(),
            tool_use_start(0, "tu_1", "get_current_time"),
            message_stop(StopReason::ToolUse),
        ] {
            out.extend(t.consume(ev));
        }
        assert!(out.is_empty(), "{out:?}");
        let fin = t.finish();
        assert_eq!(fin.len(), 2, "{fin:?}");
        assert_named_delta(&fin[0], "tu_1", "get_current_time");
        assert_finish_tool_calls(&fin[1]);
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/bedrock/zero_arg_tool_call.raw (SMA-860)
    /// with the one empty input delta removed and the block stop removed, and with
    /// the stop reason set to end_turn.
    #[test]
    fn net_does_not_fire_for_other_stop_reasons() {
        let out = run(
            false,
            vec![
                message_start(),
                tool_use_start(0, "tu_1", "get_current_time"),
                message_stop(StopReason::EndTurn),
                metadata(10, 5, None),
            ],
        );
        assert!(tool_deltas(&out).is_empty(), "{out:?}");
    }

    /// HAND-AUTHORED: no capture shows this wire shape. No capture has two tool
    /// blocks. Built from the zero-delta stream with two blocks, opened in the
    /// order 2 then 1, and no block stops.
    #[test]
    fn net_names_two_unclosed_blocks_in_index_order() {
        let out = run(
            false,
            vec![
                message_start(),
                tool_use_start(2, "tu_2", "second"),
                tool_use_start(1, "tu_1", "first"),
                message_stop(StopReason::ToolUse),
                metadata(10, 5, None),
            ],
        );
        assert_eq!(
            tool_call_names(&out),
            vec!["first".to_owned(), "second".to_owned()]
        );
        assert_eq!(out.len(), 4, "{out:?}");
        assert_named_delta(&out[1], "tu_1", "first");
        assert_named_delta(&out[2], "tu_2", "second");
        assert_finish_tool_calls(&out[3]);
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Built from the
    /// zero-delta stream with two blocks. Block 0 gets an input delta. Block 1
    /// gets none. Neither block has a stop.
    #[test]
    fn net_skips_a_block_that_already_has_its_name() {
        let out = run(
            false,
            vec![
                message_start(),
                tool_use_start(0, "tu_0", "named"),
                tool_delta(0, "{}"),
                tool_use_start(1, "tu_1", "unnamed"),
                message_stop(StopReason::ToolUse),
                metadata(10, 5, None),
            ],
        );
        let deltas = tool_deltas(&out);
        assert_eq!(
            deltas,
            vec![
                (Some("named".to_owned()), "{}".to_owned()),
                (Some("unnamed".to_owned()), String::new()),
            ]
        );
        assert_eq!(
            tool_call_names(&out),
            vec!["named".to_owned(), "unnamed".to_owned()]
        );
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/bedrock/zero_arg_tool_call.raw (SMA-860)
    /// with the one empty input delta removed and the block stop removed, and a
    /// second `finish` call after the flush.
    #[test]
    fn finish_twice_after_net_flush_is_empty() {
        let mut t = StreamTranslator::new(false);
        for ev in [
            message_start(),
            tool_use_start(0, "tu_1", "get_current_time"),
            message_stop(StopReason::ToolUse),
        ] {
            let _ = t.consume(ev);
        }
        assert_eq!(t.finish().len(), 2);
        assert!(t.finish().is_empty());
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/bedrock/zero_arg_tool_call.raw (SMA-860)
    /// with the one empty input delta removed and the block stop removed, and a
    /// block stop sent after the net flush.
    #[test]
    fn late_block_stop_after_net_flush_emits_nothing() {
        let mut t = StreamTranslator::new(false);
        for ev in [
            message_start(),
            tool_use_start(0, "tu_1", "get_current_time"),
            message_stop(StopReason::ToolUse),
            metadata(10, 5, None),
        ] {
            let _ = t.consume(ev);
        }
        assert!(t.consume(content_block_stop(0)).is_empty());
    }
}
