//! `MessageTranslator` — Anthropic SSE events → `ModelEvent` stream.

use std::collections::HashMap;

use paigasus_helikon_core::{FinishReason, ModelError, ModelEvent};

use crate::error::map_error_type;
use crate::provider_data::{AnthropicData, ThinkingBlock};
use crate::sse::{
    AnthropicEvent, ContentBlockDelta, ContentBlockHead, MessageDeltaUsage, MessageStartUsage,
};
use crate::translate::response_format::SYNTHESIZED_TOOL_NAME;

#[derive(Debug)]
enum BlockState {
    Text {
        bytes: u64,
    },
    Thinking {
        text: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        call_id: String,
        name: String,
        name_emitted: bool,
        /// Args from the block start `input`, used only when no delta follows.
        start_args: String,
        /// Whether this block is the synthesized structured-output tool.
        synthesized: bool,
    },
}

/// The args a zero-delta tool call carries: the start `input` when it is a
/// JSON object with at least one key, else an empty string.
fn start_args(input: &serde_json::Value) -> String {
    match input {
        serde_json::Value::Object(map) if !map.is_empty() => {
            serde_json::to_string(input).unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// The one event that names a tool call that got no input delta.
fn emit_unnamed(call_id: &str, name: &str, start_args: &str) -> ModelEvent {
    ModelEvent::ToolCallDelta {
        call_id: call_id.to_owned(),
        name: Some(name.to_owned()),
        args_delta: start_args.to_owned(),
    }
}

/// State machine for one streaming response.
///
/// `synthesizing_output: true` means a `ResponseFormat::JsonSchema`/`JsonObject`
/// request was sent. When the synthesized tool's content block starts, its
/// `input_json_delta` events are remapped to `TokenDelta`s and the
/// `stop_reason: "tool_use"` is rewritten to `Stop` if it was the only tool fired.
pub(crate) struct MessageTranslator {
    blocks: HashMap<u32, BlockState>,
    last_input_tokens: u32,
    last_cached_input_tokens: Option<u32>,
    stop_reason: Option<String>,
    synthesizing_output: bool,
    synthesized_tool_index: Option<u32>,
    other_tool_fired: bool,
    terminal_emitted: bool,
    /// The stream has emitted a `block` part of this response.
    block_emitted: bool,
    /// Position records that wait for the first `block` part.
    waiting_positions: Vec<ModelEvent>,
}

impl MessageTranslator {
    pub(crate) fn new(synthesizing_output: bool) -> Self {
        Self {
            blocks: HashMap::new(),
            last_input_tokens: 0,
            last_cached_input_tokens: None,
            stop_reason: None,
            synthesizing_output,
            synthesized_tool_index: None,
            other_tool_fired: false,
            terminal_emitted: false,
            block_emitted: false,
            waiting_positions: Vec::new(),
        }
    }

    /// Consume one event. Returns the emitted ModelEvents (most calls
    /// emit zero or one; `message_delta` carrying both stop_reason and
    /// usage emits one Usage followed by Finish on `message_stop`). A
    /// terminal event (`Finish` or an `Err`) is emitted at most once per
    /// stream, and a later `message_delta`'s `Usage` is suppressed once one
    /// has been emitted. Content deltas are deliberately *not* guarded: a
    /// malformed stream that keeps sending `content_block_delta` after
    /// `message_stop` would still yield them. A clean end-of-stream flush of
    /// a buffered stop reason with no terminal event yet is handled
    /// separately by `finish()`.
    pub(crate) fn consume(
        &mut self,
        event: AnthropicEvent,
    ) -> Result<Vec<Result<ModelEvent, ModelError>>, ModelError> {
        let mut out: Vec<Result<ModelEvent, ModelError>> = Vec::new();
        match event {
            AnthropicEvent::MessageStart { message } => {
                let MessageStartUsage {
                    input_tokens,
                    cache_read_input_tokens,
                    ..
                } = message.usage;
                self.last_input_tokens = input_tokens;
                self.last_cached_input_tokens = cache_read_input_tokens;
                out.push(Ok(ModelEvent::Usage {
                    input_tokens,
                    output_tokens: 0,
                    cached_input_tokens: cache_read_input_tokens,
                    reasoning_tokens: None,
                }));
            }
            AnthropicEvent::ContentBlockStart {
                index,
                content_block,
            } => match content_block {
                ContentBlockHead::Text => {
                    self.blocks.insert(index, BlockState::Text { bytes: 0 });
                }
                ContentBlockHead::Thinking {
                    thinking,
                    signature,
                } => {
                    self.blocks.insert(
                        index,
                        BlockState::Thinking {
                            text: thinking,
                            signature,
                        },
                    );
                }
                ContentBlockHead::RedactedThinking { data } => {
                    self.blocks
                        .insert(index, BlockState::RedactedThinking { data });
                }
                ContentBlockHead::ToolUse { id, name, input } => {
                    let synthesized = self.synthesizing_output && name == SYNTHESIZED_TOOL_NAME;
                    if synthesized {
                        self.synthesized_tool_index = Some(index);
                    } else {
                        self.other_tool_fired = true;
                    }
                    self.blocks.insert(
                        index,
                        BlockState::ToolUse {
                            call_id: id,
                            name,
                            name_emitted: false,
                            start_args: start_args(&input),
                            synthesized,
                        },
                    );
                }
            },
            AnthropicEvent::ContentBlockDelta { index, delta } => match delta {
                ContentBlockDelta::TextDelta { text } => {
                    if let Some(BlockState::Text { bytes }) = self.blocks.get_mut(&index) {
                        *bytes = bytes.saturating_add(text.len() as u64);
                    } else {
                        tracing::debug!(
                            target: "paigasus::anthropic::stream",
                            index,
                            "text_delta for a block that is not an open text block",
                        );
                    }
                    out.push(Ok(ModelEvent::TokenDelta { text }));
                }
                ContentBlockDelta::ThinkingDelta { thinking } => {
                    if let Some(BlockState::Thinking { text, .. }) = self.blocks.get_mut(&index) {
                        text.push_str(&thinking);
                    }
                    out.push(Ok(ModelEvent::ReasoningDelta { text: thinking }));
                }
                ContentBlockDelta::SignatureDelta { signature } => {
                    if let Some(BlockState::Thinking {
                        signature: joined, ..
                    }) = self.blocks.get_mut(&index)
                    {
                        joined.push_str(&signature);
                    } else {
                        tracing::debug!(
                            target: "paigasus::anthropic::stream",
                            index,
                            "signature_delta for a block that is not open; dropped",
                        );
                    }
                }
                ContentBlockDelta::InputJsonDelta { partial_json } => {
                    let is_synth = Some(index) == self.synthesized_tool_index;
                    if is_synth {
                        out.push(Ok(ModelEvent::TokenDelta { text: partial_json }));
                    } else if let Some(BlockState::ToolUse {
                        call_id,
                        name,
                        name_emitted,
                        ..
                    }) = self.blocks.get_mut(&index)
                    {
                        let (emit_name, call_id_out) = if *name_emitted {
                            (None, call_id.clone())
                        } else {
                            *name_emitted = true;
                            (Some(name.clone()), call_id.clone())
                        };
                        out.push(Ok(ModelEvent::ToolCallDelta {
                            call_id: call_id_out,
                            name: emit_name,
                            args_delta: partial_json,
                        }));
                    } else {
                        // Protocol violation: input_json_delta only ever
                        // applies to a tool_use content block. Surface it
                        // rather than silently dropping (which would mask
                        // a malformed upstream stream).
                        return Err(ModelError::Transport(format!(
                            "anthropic stream: input_json_delta at index {index} \
                             has no preceding tool_use content_block_start"
                        )));
                    }
                }
            },
            AnthropicEvent::ContentBlockStop { index } => {
                self.stop_block(index, &mut out);
            }
            AnthropicEvent::MessageDelta { delta, usage } => {
                // A `message_delta` after the terminal event is a protocol
                // violation (e.g. a second `message_delta` re-arming
                // `stop_reason` post-`message_stop`, see
                // `second_message_delta_after_message_stop_does_not_double_finish`).
                // Emitting its `Usage` would put an event after `Finish`,
                // which the core contract forbids.
                if !self.terminal_emitted {
                    if let Some(MessageDeltaUsage { output_tokens }) = usage {
                        out.push(Ok(ModelEvent::Usage {
                            input_tokens: self.last_input_tokens,
                            output_tokens,
                            cached_input_tokens: self.last_cached_input_tokens,
                            reasoning_tokens: None,
                        }));
                    }
                }
                if let Some(reason) = delta.stop_reason {
                    self.stop_reason = Some(reason);
                }
            }
            AnthropicEvent::MessageStop => {
                if let Some(reason) = self.stop_reason.take() {
                    if !self.terminal_emitted {
                        out.extend(self.terminal_events(&reason));
                    }
                }
            }
            AnthropicEvent::Ping => {}
            AnthropicEvent::Error { error } => {
                return Err(map_error_type(None, &error.ty, &error.message, None));
            }
        }
        Ok(out)
    }

    /// Close the block at `index`: keep a valid thinking block and record the
    /// positions of each text block and each `tool_use` block.
    fn stop_block(&mut self, index: u32, out: &mut Vec<Result<ModelEvent, ModelError>>) {
        // No event can follow `Finish`, so a block stop after the terminal
        // event gives no provider data.
        if self.terminal_emitted {
            tracing::debug!(
                target: "paigasus::anthropic::stream",
                index,
                "content_block_stop after the terminal event",
            );
            return;
        }
        let Some(state) = self.blocks.remove(&index) else {
            tracing::debug!(
                target: "paigasus::anthropic::stream",
                index,
                "content_block_stop for a block that is not open",
            );
            return;
        };
        match state {
            BlockState::Thinking { text, signature } => self.keep_block(
                index,
                "thinking",
                ThinkingBlock::Thinking {
                    thinking: text,
                    signature,
                },
                out,
            ),
            BlockState::RedactedThinking { data } => self.keep_block(
                index,
                "redacted_thinking",
                ThinkingBlock::RedactedThinking { data },
                out,
            ),
            BlockState::Text { bytes } => self.position(
                AnthropicData::Text {
                    index,
                    bytes: Some(bytes),
                }
                .into_event(None),
                out,
            ),
            BlockState::ToolUse {
                call_id,
                name,
                name_emitted,
                start_args,
                synthesized,
            } => {
                if !name_emitted && !synthesized {
                    tracing::debug!(
                        target: "paigasus::anthropic::stream",
                        index,
                        %call_id,
                        "tool_use block closed with no input delta; emitting its name",
                    );
                    out.push(Ok(emit_unnamed(&call_id, &name, &start_args)));
                }
                if !synthesized {
                    self.position(
                        AnthropicData::ToolUse { index }.into_event(Some(call_id)),
                        out,
                    );
                }
            }
        }
    }

    /// Emit a valid thinking block, then every position that waits for it.
    fn keep_block(
        &mut self,
        index: u32,
        kind: &str,
        block: ThinkingBlock,
        out: &mut Vec<Result<ModelEvent, ModelError>>,
    ) {
        if !block.is_valid() {
            tracing::debug!(
                target: "paigasus::anthropic::stream",
                index,
                kind,
                "thinking block without signature or data is not kept",
            );
            return;
        }
        out.push(Ok(AnthropicData::Block { index, block }.into_event(None)));
        if !self.block_emitted {
            self.block_emitted = true;
            out.extend(self.waiting_positions.drain(..).map(Ok));
        }
    }

    /// Emit a position record now, or hold it until the first `block` part.
    fn position(&mut self, event: ModelEvent, out: &mut Vec<Result<ModelEvent, ModelError>>) {
        if self.block_emitted {
            out.push(Ok(event));
        } else {
            self.waiting_positions.push(event);
        }
    }

    /// Flush a stop reason buffered from `message_delta` when the response
    /// body ends cleanly before `message_stop` arrived.
    ///
    /// Returns an empty `Vec` when a terminal event was already emitted — the
    /// well-formed path, where `message_stop` drained the buffer — or when no
    /// stop reason was ever observed. A stream that ended *before*
    /// `message_delta` is never reported as a clean `Stop`; one that ended
    /// *after* it is, because `message_delta`'s `stop_reason` is the model's
    /// own authoritative decision and `message_stop` is only a frame
    /// terminator.
    ///
    /// `terminal_emitted` — not `stop_reason` being `Some` — is the guard.
    /// A second `message_delta` can re-arm the buffer after `message_stop`
    /// already emitted, and that must not produce a second terminal event.
    ///
    /// It can also return safety-net `ToolCallDelta`s, ahead of the terminal
    /// event, that name a tool block left open when the stream ended.
    ///
    /// **Clean-EOF path only.** Never call this on the cancellation or
    /// transport-error paths — see `model.rs`.
    pub(crate) fn finish(&mut self) -> Vec<Result<ModelEvent, ModelError>> {
        if self.terminal_emitted {
            return Vec::new();
        }
        let Some(reason) = self.stop_reason.take() else {
            return Vec::new();
        };
        tracing::warn!(
            target: "paigasus::anthropic::stream",
            stop_reason = %reason,
            "stream body ended without message_stop; flushing buffered stop reason",
        );
        self.terminal_events(&reason)
    }

    /// Mark the terminal event as emitted and return the events that end the stream.
    fn terminal_events(&mut self, reason: &str) -> Vec<Result<ModelEvent, ModelError>> {
        self.terminal_emitted = true;
        let result = self.finish_or_error(reason);
        let mut out = Vec::new();
        if matches!(
            result,
            Ok(ModelEvent::Finish {
                reason: FinishReason::ToolCalls
            })
        ) {
            // Safety net: a tool block with no block stop and no input delta
            // never got its name. Name it before the terminal event.
            let mut unnamed: Vec<u32> = self
                .blocks
                .iter()
                .filter_map(|(index, state)| match state {
                    BlockState::ToolUse {
                        name_emitted: false,
                        synthesized: false,
                        ..
                    } => Some(*index),
                    _ => None,
                })
                .collect();
            unnamed.sort_unstable();
            for index in unnamed {
                if let Some(BlockState::ToolUse {
                    call_id,
                    name,
                    name_emitted,
                    start_args,
                    ..
                }) = self.blocks.get_mut(&index)
                {
                    tracing::warn!(
                        target: "paigasus::anthropic::stream",
                        index,
                        %call_id,
                        "terminal tool_use with an unclosed block that sent no name; emitting its name",
                    );
                    *name_emitted = true;
                    out.push(Ok(emit_unnamed(call_id, name, start_args)));
                }
            }
        }
        out.push(result);
        out
    }

    fn finish_or_error(&self, reason: &str) -> Result<ModelEvent, ModelError> {
        match reason {
            "end_turn" | "stop_sequence" => Ok(ModelEvent::Finish {
                reason: FinishReason::Stop,
            }),
            "max_tokens" => Ok(ModelEvent::Finish {
                reason: FinishReason::Length,
            }),
            "tool_use" => {
                if self.synthesizing_output && !self.other_tool_fired {
                    Ok(ModelEvent::Finish {
                        reason: FinishReason::Stop,
                    })
                } else if self.synthesizing_output && self.other_tool_fired {
                    Err(ModelError::Other(anyhow::anyhow!(
                        "structured output: model fired both a real tool and the synthesized output tool"
                    )))
                } else {
                    Ok(ModelEvent::Finish {
                        reason: FinishReason::ToolCalls,
                    })
                }
            }
            "refusal" => Err(ModelError::Refused {
                reason: "model refused".to_owned(),
            }),
            other => Ok(ModelEvent::Finish {
                reason: FinishReason::Other(other.to_owned()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::{
        AnthropicErrorPayload, ContentBlockHead, MessageDeltaPayload, MessageStartPayload,
    };

    /// Take the one event a `finish()` call must return.
    fn single(v: Vec<Result<ModelEvent, ModelError>>) -> Result<ModelEvent, ModelError> {
        assert_eq!(v.len(), 1, "a buffered reason must flush");
        v.into_iter().next().unwrap()
    }

    fn message_start(input: u32, cached: Option<u32>) -> AnthropicEvent {
        AnthropicEvent::MessageStart {
            message: MessageStartPayload {
                usage: MessageStartUsage {
                    input_tokens: input,
                    cache_read_input_tokens: cached,
                    cache_creation_input_tokens: None,
                },
            },
        }
    }

    #[test]
    fn message_start_emits_initial_usage_with_cached_count() {
        let mut t = MessageTranslator::new(false);
        let out = t.consume(message_start(100, Some(80))).unwrap();
        assert_eq!(out.len(), 1);
        match out.into_iter().next().unwrap().unwrap() {
            ModelEvent::Usage {
                input_tokens,
                cached_input_tokens,
                output_tokens,
                ..
            } => {
                assert_eq!(input_tokens, 100);
                assert_eq!(cached_input_tokens, Some(80));
                assert_eq!(output_tokens, 0);
            }
            _ => panic!("expected Usage"),
        }
    }

    #[test]
    fn text_delta_emits_token_delta() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::Text,
        });
        let out = t
            .consume(AnthropicEvent::ContentBlockDelta {
                index: 0,
                delta: ContentBlockDelta::TextDelta {
                    text: "Hi".to_owned(),
                },
            })
            .unwrap();
        match out.into_iter().next().unwrap().unwrap() {
            ModelEvent::TokenDelta { text } => assert_eq!(text, "Hi"),
            _ => panic!("expected TokenDelta"),
        }
    }

    #[test]
    fn thinking_delta_emits_reasoning_delta() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::Thinking {
                thinking: String::new(),
                signature: String::new(),
            },
        });
        let out = t
            .consume(AnthropicEvent::ContentBlockDelta {
                index: 0,
                delta: ContentBlockDelta::ThinkingDelta {
                    thinking: "think".to_owned(),
                },
            })
            .unwrap();
        match out.into_iter().next().unwrap().unwrap() {
            ModelEvent::ReasoningDelta { text } => assert_eq!(text, "think"),
            _ => panic!("expected ReasoningDelta"),
        }
    }

    #[test]
    fn tool_use_emits_call_delta_with_name_only_once() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 1,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_1".to_owned(),
                name: "search".to_owned(),
                input: serde_json::json!({}),
            },
        });
        let first = t
            .consume(AnthropicEvent::ContentBlockDelta {
                index: 1,
                delta: ContentBlockDelta::InputJsonDelta {
                    partial_json: "{".to_owned(),
                },
            })
            .unwrap();
        match first.into_iter().next().unwrap().unwrap() {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "tu_1");
                assert_eq!(name.as_deref(), Some("search"));
                assert_eq!(args_delta, "{");
            }
            _ => panic!("expected ToolCallDelta"),
        }

        let second = t
            .consume(AnthropicEvent::ContentBlockDelta {
                index: 1,
                delta: ContentBlockDelta::InputJsonDelta {
                    partial_json: "\"q\":1}".to_owned(),
                },
            })
            .unwrap();
        match second.into_iter().next().unwrap().unwrap() {
            ModelEvent::ToolCallDelta { name, .. } => assert!(name.is_none(), "name not repeated"),
            _ => panic!("expected ToolCallDelta"),
        }
    }

    #[test]
    fn synthesized_tool_remaps_input_json_to_token_delta() {
        let mut t = MessageTranslator::new(true);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_synth".to_owned(),
                name: SYNTHESIZED_TOOL_NAME.to_owned(),
                input: serde_json::json!({}),
            },
        });
        let out = t
            .consume(AnthropicEvent::ContentBlockDelta {
                index: 0,
                delta: ContentBlockDelta::InputJsonDelta {
                    partial_json: "{\"x\":1}".to_owned(),
                },
            })
            .unwrap();
        match out.into_iter().next().unwrap().unwrap() {
            ModelEvent::TokenDelta { text } => assert_eq!(text, "{\"x\":1}"),
            other => panic!("expected TokenDelta, got {other:?}"),
        }
    }

    #[test]
    fn message_delta_then_stop_emits_usage_then_finish() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, Some(2))).unwrap();
        let usage_out = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("end_turn".to_owned()),
                },
                usage: Some(MessageDeltaUsage { output_tokens: 5 }),
            })
            .unwrap();
        assert_eq!(usage_out.len(), 1);
        match usage_out.into_iter().next().unwrap().unwrap() {
            ModelEvent::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                ..
            } => {
                assert_eq!(input_tokens, 10);
                assert_eq!(output_tokens, 5);
                assert_eq!(cached_input_tokens, Some(2));
            }
            _ => panic!("expected Usage"),
        }
        let stop_out = t.consume(AnthropicEvent::MessageStop).unwrap();
        match stop_out.into_iter().next().unwrap().unwrap() {
            ModelEvent::Finish { reason } => assert_eq!(reason, FinishReason::Stop),
            _ => panic!("expected Finish"),
        }
    }

    #[test]
    fn tool_use_stop_reason_emits_tool_calls_finish_without_synthesis() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::MessageDelta {
            delta: MessageDeltaPayload {
                stop_reason: Some("tool_use".to_owned()),
            },
            usage: None,
        });
        let out = t.consume(AnthropicEvent::MessageStop).unwrap();
        match out.into_iter().next().unwrap().unwrap() {
            ModelEvent::Finish { reason } => assert_eq!(reason, FinishReason::ToolCalls),
            _ => panic!("expected Finish"),
        }
    }

    #[test]
    fn synthesized_only_rewrites_tool_use_to_stop() {
        let mut t = MessageTranslator::new(true);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_s".to_owned(),
                name: SYNTHESIZED_TOOL_NAME.to_owned(),
                input: serde_json::json!({}),
            },
        });
        let _ = t.consume(AnthropicEvent::MessageDelta {
            delta: MessageDeltaPayload {
                stop_reason: Some("tool_use".to_owned()),
            },
            usage: None,
        });
        let out = t.consume(AnthropicEvent::MessageStop).unwrap();
        match out.into_iter().next().unwrap().unwrap() {
            ModelEvent::Finish { reason } => assert_eq!(reason, FinishReason::Stop),
            _ => panic!("expected Finish::Stop"),
        }
    }

    #[test]
    fn synthesized_plus_real_tool_errors() {
        let mut t = MessageTranslator::new(true);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_s".to_owned(),
                name: SYNTHESIZED_TOOL_NAME.to_owned(),
                input: serde_json::json!({}),
            },
        });
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 1,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_r".to_owned(),
                name: "search".to_owned(),
                input: serde_json::json!({}),
            },
        });
        let _ = t.consume(AnthropicEvent::MessageDelta {
            delta: MessageDeltaPayload {
                stop_reason: Some("tool_use".to_owned()),
            },
            usage: None,
        });
        let out = t.consume(AnthropicEvent::MessageStop).unwrap();
        match out.into_iter().next().unwrap() {
            Err(ModelError::Other(_)) => {}
            other => panic!("expected Err(Other), got {other:?}"),
        }
    }

    #[test]
    fn in_stream_overloaded_error_terminates_with_unavailable() {
        let mut t = MessageTranslator::new(false);
        let err = t
            .consume(AnthropicEvent::Error {
                error: AnthropicErrorPayload {
                    ty: "overloaded_error".to_owned(),
                    message: "busy".to_owned(),
                },
            })
            .unwrap_err();
        assert!(matches!(err, ModelError::Unavailable));
    }

    /// A second `message_delta` re-arms `stop_reason` after `message_stop`
    /// already emitted the terminal event. The EOF flush must not turn that
    /// into a second `Finish` — `core::Model::invoke` guarantees nothing
    /// follows `Finish`.
    #[test]
    fn second_message_delta_after_message_stop_does_not_double_finish() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("end_turn".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        let stop_out = t.consume(AnthropicEvent::MessageStop).unwrap();
        assert_eq!(stop_out.len(), 1, "message_stop emits the terminal Finish");

        // Protocol violation: a second stop reason after the terminal event.
        // A real `message_delta` always carries `usage`, so exercise that
        // shape rather than sidestepping it with `usage: None`.
        let second_delta_out = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("max_tokens".to_owned()),
                },
                usage: Some(MessageDeltaUsage { output_tokens: 99 }),
            })
            .unwrap();
        assert!(
            second_delta_out.is_empty(),
            "nothing may follow the terminal event, got {second_delta_out:?}"
        );
        assert!(
            t.finish().is_empty(),
            "a second stop reason must not yield a second terminal event"
        );
    }

    /// The same guard on the inline path. This case is a pre-existing defect,
    /// independent of the EOF flush: today the second `message_stop` emits a
    /// second `Finish`.
    #[test]
    fn repeated_message_stop_emits_one_finish() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("end_turn".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        assert_eq!(t.consume(AnthropicEvent::MessageStop).unwrap().len(), 1);

        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("end_turn".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        let second_stop = t.consume(AnthropicEvent::MessageStop).unwrap();
        assert!(
            second_stop.is_empty(),
            "a repeated message_stop must not emit a second Finish, got {second_stop:?}"
        );
    }

    /// The core flush: a stop reason buffered with no `message_stop` following.
    #[test]
    fn finish_flushes_pending_stop_reason() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("end_turn".to_owned()),
                },
                usage: Some(MessageDeltaUsage { output_tokens: 5 }),
            })
            .unwrap();
        match single(t.finish()) {
            Ok(ModelEvent::Finish { reason }) => assert_eq!(reason, FinishReason::Stop),
            other => panic!("expected Ok(Finish::Stop), got {other:?}"),
        }
    }

    /// The highest-consequence `Ok` variant: the agent loop will execute tool
    /// calls assembled from a stream that was cut short.
    #[test]
    fn finish_flushes_tool_use_as_tool_calls() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("tool_use".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        match single(t.finish()) {
            Ok(ModelEvent::Finish { reason }) => assert_eq!(reason, FinishReason::ToolCalls),
            other => panic!("expected Ok(Finish::ToolCalls), got {other:?}"),
        }
    }

    /// Truncation before any stop reason: never reported as a clean `Stop`.
    #[test]
    fn finish_is_empty_when_no_stop_reason_observed() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::Text,
        });
        assert!(
            t.finish().is_empty(),
            "no stop reason was observed; nothing to flush"
        );
    }

    /// The well-formed path: `message_stop` already emitted, so the EOF flush
    /// is a no-op — and stays one on a repeated call.
    #[test]
    fn finish_is_empty_after_message_stop_drained_it() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("end_turn".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        assert_eq!(t.consume(AnthropicEvent::MessageStop).unwrap().len(), 1);
        assert!(
            t.finish().is_empty(),
            "message_stop already emitted the terminal event"
        );
        assert!(t.finish().is_empty(), "finish() must be idempotent");
    }

    /// A refusal observed before truncation surfaces as an error, not silence.
    #[test]
    fn finish_surfaces_refusal_as_error() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("refusal".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        match single(t.finish()) {
            Err(ModelError::Refused { .. }) => {}
            other => panic!("expected Err(Refused), got {other:?}"),
        }
    }

    /// The second `Err` outcome: synthesis mode with both a real and the
    /// synthesized tool fired. Mirrors bedrock's
    /// `finish_surfaces_both_tools_error_without_metadata`.
    #[test]
    fn finish_surfaces_both_tools_error() {
        let mut t = MessageTranslator::new(true);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_s".to_owned(),
                name: SYNTHESIZED_TOOL_NAME.to_owned(),
                input: serde_json::json!({}),
            },
        });
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 1,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_r".to_owned(),
                name: "search".to_owned(),
                input: serde_json::json!({}),
            },
        });
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("tool_use".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        match single(t.finish()) {
            Err(ModelError::Other(_)) => {}
            other => panic!("expected Err(Other), got {other:?}"),
        }
    }

    // --- SMA-840: thinking blocks and position records ---

    use crate::provider_data::{AnthropicData, ThinkingBlock};

    fn translate_fixture(raw: &str, synthesizing: bool) -> Vec<ModelEvent> {
        let mut t = MessageTranslator::new(synthesizing);
        let mut out = Vec::new();
        for line in raw.lines() {
            let Some(json) = line.strip_prefix("data: ") else {
                continue;
            };
            let event: AnthropicEvent = serde_json::from_str(json).unwrap();
            for item in t.consume(event).unwrap() {
                out.push(item.unwrap());
            }
        }
        for last in t.finish() {
            out.push(last.unwrap());
        }
        out
    }

    fn provider_data(events: &[ModelEvent]) -> Vec<(AnthropicData, Option<String>)> {
        events
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ProviderData {
                    provider,
                    call_id,
                    data,
                } => {
                    assert_eq!(provider, crate::PROVIDER_DATA_KEY);
                    Some((
                        serde_json::from_value(data.clone()).unwrap(),
                        call_id.clone(),
                    ))
                }
                _ => None,
            })
            .collect()
    }

    fn reasoning_text(events: &[ModelEvent]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ReasoningDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn block(index: u32, thinking: &str, signature: &str) -> AnthropicData {
        AnthropicData::Block {
            index,
            block: ThinkingBlock::Thinking {
                thinking: thinking.to_owned(),
                signature: signature.to_owned(),
            },
        }
    }

    fn redacted(index: u32, data: &str) -> AnthropicData {
        AnthropicData::Block {
            index,
            block: ThinkingBlock::RedactedThinking {
                data: data.to_owned(),
            },
        }
    }

    fn start(index: u32, content_block: ContentBlockHead) -> AnthropicEvent {
        AnthropicEvent::ContentBlockStart {
            index,
            content_block,
        }
    }

    fn thinking_head() -> ContentBlockHead {
        ContentBlockHead::Thinking {
            thinking: String::new(),
            signature: String::new(),
        }
    }

    fn tool_head(id: &str, name: &str) -> ContentBlockHead {
        ContentBlockHead::ToolUse {
            id: id.to_owned(),
            name: name.to_owned(),
            input: serde_json::json!({}),
        }
    }

    fn thinking_delta(index: u32, text: &str) -> AnthropicEvent {
        AnthropicEvent::ContentBlockDelta {
            index,
            delta: ContentBlockDelta::ThinkingDelta {
                thinking: text.to_owned(),
            },
        }
    }

    fn signature_delta(index: u32, signature: &str) -> AnthropicEvent {
        AnthropicEvent::ContentBlockDelta {
            index,
            delta: ContentBlockDelta::SignatureDelta {
                signature: signature.to_owned(),
            },
        }
    }

    fn json_delta(index: u32, partial: &str) -> AnthropicEvent {
        AnthropicEvent::ContentBlockDelta {
            index,
            delta: ContentBlockDelta::InputJsonDelta {
                partial_json: partial.to_owned(),
            },
        }
    }

    fn text_delta(index: u32, text: &str) -> AnthropicEvent {
        AnthropicEvent::ContentBlockDelta {
            index,
            delta: ContentBlockDelta::TextDelta {
                text: text.to_owned(),
            },
        }
    }

    fn text_pos(index: u32, bytes: u64) -> (AnthropicData, Option<String>) {
        (
            AnthropicData::Text {
                index,
                bytes: Some(bytes),
            },
            None,
        )
    }

    fn stop(index: u32) -> AnthropicEvent {
        AnthropicEvent::ContentBlockStop { index }
    }

    /// Feed events to a translator and collect every emitted event.
    fn run(t: &mut MessageTranslator, events: Vec<AnthropicEvent>) -> Vec<ModelEvent> {
        let mut out = Vec::new();
        for e in events {
            for item in t.consume(e).unwrap() {
                out.push(item.unwrap());
            }
        }
        out
    }

    /// Join the `thinking_delta` texts of a fixture in stream order.
    fn fixture_thinking_text(raw: &str) -> String {
        let mut text = String::new();
        for line in raw.lines() {
            let Some(json) = line.strip_prefix("data: ") else {
                continue;
            };
            if let AnthropicEvent::ContentBlockDelta {
                delta: ContentBlockDelta::ThinkingDelta { thinking },
                ..
            } = serde_json::from_str(json).unwrap()
            {
                text.push_str(&thinking);
            }
        }
        text
    }

    #[test]
    fn thinking_text_tool_use_emits_block_and_positions() {
        let raw = include_str!("../tests/fixtures/thinking_text_tool_use.txt");
        let events = translate_fixture(raw, false);
        let joined = fixture_thinking_text(raw);
        assert!(!joined.is_empty());
        assert_eq!(reasoning_text(&events), joined);
        assert_eq!(
            provider_data(&events),
            vec![
                (block(0, &joined, "SIG_FAKE_1"), None),
                (
                    AnthropicData::Text {
                        index: 1,
                        bytes: Some(210)
                    },
                    None
                ),
                (
                    AnthropicData::ToolUse { index: 2 },
                    Some("toolu_017EB2o3zrXW8R73aqBPNiZi".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn redacted_fixture_emits_two_blocks() {
        let events = translate_fixture(
            include_str!("../tests/fixtures/redacted_thinking_tool_use.txt"),
            false,
        );
        assert_eq!(
            provider_data(&events),
            vec![
                (redacted(0, "REDACTED_FAKE_1"), None),
                (redacted(1, "REDACTED_FAKE_2"), None),
                (
                    AnthropicData::Text {
                        index: 2,
                        bytes: Some(92)
                    },
                    None
                ),
                (
                    AnthropicData::ToolUse { index: 3 },
                    Some("toolu_01TUKF2PUduQbRPsN2DaUV1u".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn empty_thinking_fixture_emits_block_with_empty_text() {
        let events = translate_fixture(
            include_str!("../tests/fixtures/empty_thinking_tool_use.txt"),
            false,
        );
        assert_eq!(provider_data(&events)[0].0, block(0, "", "SIG_FAKE_1"));
    }

    #[test]
    fn interleaved_fixture_emits_each_block() {
        let events = translate_fixture(
            include_str!("../tests/fixtures/interleaved_thinking.txt"),
            false,
        );
        let data = provider_data(&events);
        let expected = vec![
            (block(0, "Need the code.", "SIG_FAKE_1"), None),
            (
                AnthropicData::ToolUse { index: 1 },
                Some("toolu_fake_b".to_owned()),
            ),
            (block(2, "Now the weather.", "SIG_FAKE_2"), None),
            (
                AnthropicData::ToolUse { index: 3 },
                Some("toolu_fake_a".to_owned()),
            ),
        ];
        assert_eq!(data.len(), expected.len(), "got {data:?}");
        for entry in &expected {
            assert!(data.contains(entry), "missing {entry:?} in {data:?}");
        }
    }

    #[test]
    fn unsigned_thinking_emits_no_provider_data() {
        let events = translate_fixture(
            include_str!("../tests/fixtures/unsigned_thinking_tool_use.txt"),
            false,
        );
        assert!(provider_data(&events).is_empty());
        assert!(!reasoning_text(&events).is_empty());
    }

    #[test]
    fn fixtures_without_thinking_emit_no_provider_data() {
        let turns = include_str!("../tests/fixtures/tool_use_then_continuation.txt");
        let mut raws = vec![
            include_str!("../tests/fixtures/text_only.txt"),
            include_str!("../tests/fixtures/parallel_tool_use.txt"),
        ];
        raws.extend(turns.split("# --- turn 2 ---\n"));
        assert_eq!(raws.len(), 4);
        for raw in raws {
            let events = translate_fixture(raw, false);
            assert!(provider_data(&events).is_empty(), "got {events:?}");
        }
    }

    #[test]
    fn thinking_then_text_emits_block_and_text_position() {
        let events = translate_fixture(
            include_str!("../tests/fixtures/thinking_then_text.txt"),
            false,
        );
        assert_eq!(
            provider_data(&events),
            vec![
                (block(0, "Let me think...", "abc123"), None),
                (
                    AnthropicData::Text {
                        index: 1,
                        bytes: Some(2)
                    },
                    None
                ),
            ]
        );
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Built from a thinking
    /// block followed by a tool block with no input delta.
    #[test]
    fn tool_use_without_input_json_delta_gets_a_position() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                start(0, thinking_head()),
                signature_delta(0, "SIG_FAKE_1"),
                stop(0),
                start(1, tool_head("tu_1", "search")),
                stop(1),
            ],
        );
        assert_eq!(
            provider_data(&events),
            vec![
                (block(0, "", "SIG_FAKE_1"), None),
                (AnthropicData::ToolUse { index: 1 }, Some("tu_1".to_owned())),
            ]
        );
    }

    fn tool_head_with_input(id: &str, name: &str, input: serde_json::Value) -> ContentBlockHead {
        ContentBlockHead::ToolUse {
            id: id.to_owned(),
            name: name.to_owned(),
            input,
        }
    }

    fn message_delta(reason: &str) -> AnthropicEvent {
        AnthropicEvent::MessageDelta {
            delta: MessageDeltaPayload {
                stop_reason: Some(reason.to_owned()),
            },
            usage: None,
        }
    }

    /// Every `ToolCallDelta` as `(call_id, name, args_delta)`, in order.
    fn call_deltas(events: &[ModelEvent]) -> Vec<(String, Option<String>, String)> {
        events
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    name,
                    args_delta,
                } => Some((call_id.clone(), name.clone(), args_delta.clone())),
                _ => None,
            })
            .collect()
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/anthropic/zero_arg_tool_call.raw
    /// (SMA-860) with the one empty input delta removed.
    #[test]
    fn tool_use_without_input_json_delta_emits_name_at_block_stop() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                message_start(5, None),
                start(0, tool_head("tu_1", "get_current_time")),
                stop(0),
                message_delta("tool_use"),
                AnthropicEvent::MessageStop,
            ],
        );
        assert_eq!(
            call_deltas(&events),
            vec![(
                "tu_1".to_owned(),
                Some("get_current_time".to_owned()),
                String::new()
            )]
        );
        let delta_pos = events
            .iter()
            .position(|e| matches!(e, ModelEvent::ToolCallDelta { .. }))
            .unwrap();
        let finish_pos = events
            .iter()
            .position(|e| matches!(e, ModelEvent::Finish { .. }))
            .unwrap();
        assert!(delta_pos < finish_pos);
        assert_eq!(finish_pos, events.len() - 1);
        assert!(matches!(
            events.last(),
            Some(ModelEvent::Finish {
                reason: FinishReason::ToolCalls
            })
        ));
    }

    /// Transcribed by hand from the captured stream in
    /// tests/provider-stream-conformance/fixtures/anthropic/zero_arg_tool_call.raw
    /// (SMA-860), including its one empty input delta.
    #[test]
    fn captured_empty_delta_still_gives_one_name() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                message_start(5, None),
                start(0, tool_head("tu_1", "get_current_time")),
                json_delta(0, ""),
                stop(0),
                message_delta("tool_use"),
                AnthropicEvent::MessageStop,
            ],
        );
        let deltas = call_deltas(&events);
        assert_eq!(deltas.iter().filter(|d| d.1.is_some()).count(), 1);
        assert_eq!(deltas.len(), 1);
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/anthropic/zero_arg_tool_call.raw
    /// (SMA-860) with the one empty input delta removed and the stop reason
    /// changed to `max_tokens`.
    #[test]
    fn block_stop_with_max_tokens_gives_named_call_then_length() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                message_start(5, None),
                start(0, tool_head("tu_1", "get_current_time")),
                stop(0),
                message_delta("max_tokens"),
                AnthropicEvent::MessageStop,
            ],
        );
        assert_eq!(call_deltas(&events).len(), 1);
        assert!(call_deltas(&events)[0].1.is_some());
        assert!(matches!(
            events.last(),
            Some(ModelEvent::Finish {
                reason: FinishReason::Length
            })
        ));
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/anthropic/zero_arg_tool_call.raw
    /// (SMA-860) with the one empty input delta removed.
    #[test]
    fn synthesized_tool_without_delta_emits_no_tool_call_delta() {
        let mut t = MessageTranslator::new(true);
        let events = run(
            &mut t,
            vec![
                message_start(5, None),
                start(0, tool_head("tu_s", SYNTHESIZED_TOOL_NAME)),
                stop(0),
                message_delta("tool_use"),
                AnthropicEvent::MessageStop,
            ],
        );
        assert!(call_deltas(&events).is_empty());
        assert!(!events
            .iter()
            .any(|e| matches!(e, ModelEvent::TokenDelta { .. })));
        assert!(matches!(
            events.last(),
            Some(ModelEvent::Finish {
                reason: FinishReason::Stop
            })
        ));
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Built from the
    /// zero-delta stream with a non-empty `input` object in the block start.
    #[test]
    fn start_input_with_keys_is_sent_as_args() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                start(
                    0,
                    tool_head_with_input("tu_1", "search", serde_json::json!({"q": "x"})),
                ),
                stop(0),
            ],
        );
        let deltas = call_deltas(&events);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].2, r#"{"q":"x"}"#);
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Built from the
    /// zero-delta stream with a non-empty `input` object in the block start and
    /// an input delta after it.
    #[test]
    fn start_input_is_ignored_when_deltas_follow() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                start(
                    0,
                    tool_head_with_input("tu_1", "search", serde_json::json!({"q": "x"})),
                ),
                json_delta(0, r#"{"q":"y"}"#),
                stop(0),
            ],
        );
        let deltas = call_deltas(&events);
        let joined: String = deltas.iter().map(|d| d.2.as_str()).collect();
        assert_eq!(joined, r#"{"q":"y"}"#);
        assert_eq!(deltas.iter().filter(|d| d.1.is_some()).count(), 1);
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Built from the
    /// zero-delta stream with two tool blocks, one with an input delta and one
    /// without.
    #[test]
    fn parallel_calls_one_without_delta() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                start(0, tool_head("tu_a", "a")),
                json_delta(0, "{}"),
                start(1, tool_head("tu_b", "b")),
                stop(0),
                stop(1),
            ],
        );
        let named: Vec<(String, String)> = call_deltas(&events)
            .into_iter()
            .filter_map(|d| d.1.map(|n| (d.0, n)))
            .collect();
        assert_eq!(
            named,
            vec![
                ("tu_a".to_owned(), "a".to_owned()),
                ("tu_b".to_owned(), "b".to_owned())
            ]
        );
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Built from the
    /// zero-delta stream with a `null` or string `input` in the block start.
    #[test]
    fn non_object_start_input_gives_blank_args() {
        for input in [serde_json::json!(null), serde_json::json!("x")] {
            let mut t = MessageTranslator::new(false);
            let events = run(
                &mut t,
                vec![
                    start(0, tool_head_with_input("tu_1", "search", input)),
                    stop(0),
                ],
            );
            let deltas = call_deltas(&events);
            assert_eq!(deltas.len(), 1);
            assert_eq!(deltas[0].2, "");
        }
    }

    #[test]
    fn repeated_content_block_stop_emits_nothing_new() {
        let mut t = MessageTranslator::new(false);
        let first = run(
            &mut t,
            vec![
                start(0, thinking_head()),
                signature_delta(0, "SIG_FAKE_1"),
                stop(0),
            ],
        );
        assert_eq!(provider_data(&first).len(), 1);
        assert_eq!(t.consume(stop(0)).unwrap().len(), 0);
    }

    #[test]
    fn synthesized_tool_gets_no_position() {
        let mut t = MessageTranslator::new(true);
        let events = run(
            &mut t,
            vec![
                start(0, thinking_head()),
                signature_delta(0, "SIG_FAKE_1"),
                stop(0),
                start(1, tool_head("tu_synth", SYNTHESIZED_TOOL_NAME)),
                json_delta(1, "{\"x\":1}"),
                stop(1),
            ],
        );
        assert_eq!(
            provider_data(&events),
            vec![(block(0, "", "SIG_FAKE_1"), None)]
        );
    }

    #[test]
    fn positions_wait_for_first_block() {
        let mut t = MessageTranslator::new(false);
        let before = run(
            &mut t,
            vec![
                start(0, ContentBlockHead::Text),
                AnthropicEvent::ContentBlockDelta {
                    index: 0,
                    delta: ContentBlockDelta::TextDelta {
                        text: "hi".to_owned(),
                    },
                },
                stop(0),
                start(1, thinking_head()),
                signature_delta(1, "SIG_FAKE_1"),
            ],
        );
        assert!(provider_data(&before).is_empty());
        let at_stop = t.consume(stop(1)).unwrap();
        let events: Vec<ModelEvent> = at_stop.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(
            provider_data(&events),
            vec![
                (block(1, "", "SIG_FAKE_1"), None),
                (
                    AnthropicData::Text {
                        index: 0,
                        bytes: Some(2)
                    },
                    None
                ),
            ]
        );
    }

    #[test]
    fn each_text_block_records_its_position_and_bytes() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                start(0, thinking_head()),
                signature_delta(0, "SIG_FAKE_1"),
                stop(0),
                start(1, ContentBlockHead::Text),
                text_delta(1, "A"),
                stop(1),
                start(2, thinking_head()),
                signature_delta(2, "SIG_FAKE_2"),
                stop(2),
                start(3, ContentBlockHead::Text),
                text_delta(3, "é ü"),
                stop(3),
                start(4, tool_head("tu_1", "search")),
                json_delta(4, "{\"q\":1}"),
                stop(4),
            ],
        );
        assert_eq!(
            provider_data(&events),
            vec![
                (block(0, "", "SIG_FAKE_1"), None),
                text_pos(1, 1),
                (block(2, "", "SIG_FAKE_2"), None),
                text_pos(3, 5),
                (AnthropicData::ToolUse { index: 4 }, Some("tu_1".to_owned())),
            ]
        );
    }

    #[test]
    fn text_before_first_kept_block_waits_then_flushes_in_order() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                start(0, ContentBlockHead::Text),
                text_delta(0, "A"),
                stop(0),
                start(1, thinking_head()),
                signature_delta(1, "SIG_FAKE_1"),
                stop(1),
                start(2, ContentBlockHead::Text),
                text_delta(2, "B"),
                stop(2),
            ],
        );
        assert_eq!(
            provider_data(&events),
            vec![
                (block(1, "", "SIG_FAKE_1"), None),
                text_pos(0, 1),
                text_pos(2, 1),
            ]
        );
    }

    #[test]
    fn text_delta_for_a_block_that_is_not_open_counts_nowhere() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                start(0, thinking_head()),
                signature_delta(0, "SIG_FAKE_1"),
                stop(0),
                start(1, ContentBlockHead::Text),
                text_delta(9, "XX"),
                text_delta(1, "A"),
                stop(1),
            ],
        );
        let data = provider_data(&events);
        assert_eq!(data.last().unwrap(), &text_pos(1, 1));
        let tokens: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                ModelEvent::TokenDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tokens, vec!["XX", "A"]);
    }

    #[test]
    fn signature_from_split_deltas_is_joined() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                start(0, thinking_head()),
                thinking_delta(0, "a\n"),
                thinking_delta(0, "\"b\""),
                signature_delta(0, "SIG_"),
                signature_delta(0, "FAKE_9"),
                stop(0),
            ],
        );
        assert_eq!(
            provider_data(&events),
            vec![(block(0, "a\n\"b\"", "SIG_FAKE_9"), None)]
        );
    }

    #[test]
    fn block_stop_after_message_stop_emits_nothing() {
        let mut t = MessageTranslator::new(false);
        let _ = run(
            &mut t,
            vec![
                start(0, thinking_head()),
                signature_delta(0, "SIG_FAKE_1"),
                AnthropicEvent::MessageDelta {
                    delta: MessageDeltaPayload {
                        stop_reason: Some("end_turn".to_owned()),
                    },
                    usage: None,
                },
                AnthropicEvent::MessageStop,
            ],
        );
        let after = t.consume(stop(0)).unwrap();
        assert!(after.is_empty(), "no event after Finish, got {after:?}");
    }

    #[test]
    fn eof_after_thinking_block_keeps_only_the_block() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                start(0, thinking_head()),
                signature_delta(0, "SIG_FAKE_1"),
                stop(0),
                start(1, tool_head("tu_1", "search")),
                json_delta(1, "{\"q\":1}"),
            ],
        );
        assert!(t.finish().is_empty());
        assert_eq!(
            provider_data(&events),
            vec![(block(0, "", "SIG_FAKE_1"), None)]
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, ModelEvent::ToolCallDelta { .. })));
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/anthropic/zero_arg_tool_call.raw
    /// (SMA-860) with the one empty input delta removed and the block stop removed.
    #[test]
    fn net_names_unclosed_tool_block_on_message_stop() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                message_start(5, None),
                start(0, tool_head("tu_1", "t")),
                message_delta("tool_use"),
                AnthropicEvent::MessageStop,
            ],
        );
        assert_eq!(
            call_deltas(&events),
            vec![("tu_1".to_owned(), Some("t".to_owned()), String::new())]
        );
        assert!(matches!(
            events.last(),
            Some(ModelEvent::Finish {
                reason: FinishReason::ToolCalls
            })
        ));
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/anthropic/zero_arg_tool_call.raw
    /// (SMA-860) with the one empty input delta removed and the block stop removed.
    #[test]
    fn net_names_unclosed_tool_block_on_finish() {
        let mut t = MessageTranslator::new(false);
        run(
            &mut t,
            vec![
                message_start(5, None),
                start(0, tool_head("tu_1", "t")),
                message_delta("tool_use"),
            ],
        );
        let out: Vec<ModelEvent> = t.finish().into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(out.len(), 2);
        assert_eq!(
            call_deltas(&out),
            vec![("tu_1".to_owned(), Some("t".to_owned()), String::new())]
        );
        assert!(matches!(
            out[1],
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls
            }
        ));
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/anthropic/zero_arg_tool_call.raw
    /// (SMA-860) with the one empty input delta removed and the block stop removed.
    #[test]
    fn net_does_not_fire_for_other_stop_reasons() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                message_start(5, None),
                start(0, tool_head("tu_1", "t")),
                message_delta("max_tokens"),
                AnthropicEvent::MessageStop,
            ],
        );
        assert!(call_deltas(&events).is_empty());
        assert!(matches!(
            events.last(),
            Some(ModelEvent::Finish {
                reason: FinishReason::Length
            })
        ));
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Two tool blocks at
    /// indices 2 and 1, built from the single-block zero-argument capture.
    #[test]
    fn net_names_two_unclosed_blocks_in_index_order() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                message_start(5, None),
                start(2, tool_head("tu_b", "b")),
                start(1, tool_head("tu_a", "a")),
                message_delta("tool_use"),
                AnthropicEvent::MessageStop,
            ],
        );
        let ids: Vec<String> = call_deltas(&events).into_iter().map(|d| d.0).collect();
        assert_eq!(ids, vec!["tu_a".to_owned(), "tu_b".to_owned()]);
        assert!(matches!(
            events.last(),
            Some(ModelEvent::Finish {
                reason: FinishReason::ToolCalls
            })
        ));
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Block 0 gets its name
    /// from an input delta, block 1 gets none; neither has a block stop.
    #[test]
    fn net_skips_a_block_that_already_has_its_name() {
        let mut t = MessageTranslator::new(false);
        let events = run(
            &mut t,
            vec![
                message_start(5, None),
                start(0, tool_head("tu_0", "a")),
                json_delta(0, "{}"),
                start(1, tool_head("tu_1", "b")),
                message_delta("tool_use"),
                AnthropicEvent::MessageStop,
            ],
        );
        let named: Vec<String> = call_deltas(&events)
            .into_iter()
            .filter(|d| d.1.is_some())
            .map(|d| d.0)
            .collect();
        assert_eq!(named, vec!["tu_0".to_owned(), "tu_1".to_owned()]);
        let finish_pos = events.len() - 1;
        let last_named = events
            .iter()
            .rposition(|e| matches!(e, ModelEvent::ToolCallDelta { name: Some(_), .. }))
            .unwrap();
        assert!(last_named < finish_pos);
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/anthropic/zero_arg_tool_call.raw
    /// (SMA-860) with the one empty input delta removed and the block stop removed.
    #[test]
    fn finish_twice_after_net_flush_is_empty() {
        let mut t = MessageTranslator::new(false);
        run(
            &mut t,
            vec![
                message_start(5, None),
                start(0, tool_head("tu_1", "t")),
                message_delta("tool_use"),
            ],
        );
        assert_eq!(t.finish().len(), 2);
        assert!(t.finish().is_empty());
    }

    /// HAND-AUTHORED: no capture shows this wire shape. Derived from
    /// tests/provider-stream-conformance/fixtures/anthropic/zero_arg_tool_call.raw
    /// (SMA-860) with the one empty input delta removed and the block stop removed.
    #[test]
    fn late_block_stop_after_net_flush_emits_nothing() {
        let mut t = MessageTranslator::new(false);
        run(
            &mut t,
            vec![
                message_start(5, None),
                start(0, tool_head("tu_1", "t")),
                message_delta("tool_use"),
                AnthropicEvent::MessageStop,
            ],
        );
        assert!(t.consume(stop(0)).unwrap().is_empty());
    }
}
