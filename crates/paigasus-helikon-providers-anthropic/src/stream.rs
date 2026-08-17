//! `MessageTranslator` — Anthropic SSE events → `ModelEvent` stream.

use std::collections::HashMap;

use paigasus_helikon_core::{FinishReason, ModelError, ModelEvent};

use crate::error::map_error_type;
use crate::sse::{
    AnthropicEvent, ContentBlockDelta, ContentBlockHead, MessageDeltaUsage, MessageStartUsage,
};
use crate::translate::response_format::SYNTHESIZED_TOOL_NAME;

#[derive(Debug)]
enum BlockState {
    Text,
    Thinking,
    ToolUse {
        call_id: String,
        name: String,
        name_emitted: bool,
    },
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
                    self.blocks.insert(index, BlockState::Text);
                }
                ContentBlockHead::Thinking => {
                    self.blocks.insert(index, BlockState::Thinking);
                }
                ContentBlockHead::ToolUse { id, name, .. } => {
                    if self.synthesizing_output && name == SYNTHESIZED_TOOL_NAME {
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
                        },
                    );
                }
            },
            AnthropicEvent::ContentBlockDelta { index, delta } => match delta {
                ContentBlockDelta::TextDelta { text } => {
                    out.push(Ok(ModelEvent::TokenDelta { text }));
                }
                ContentBlockDelta::ThinkingDelta { thinking } => {
                    out.push(Ok(ModelEvent::ReasoningDelta { text: thinking }));
                }
                ContentBlockDelta::SignatureDelta { .. } => {
                    tracing::debug!(
                        target: "paigasus::anthropic::stream",
                        "signature_delta dropped (round-trip not yet supported)",
                    );
                }
                ContentBlockDelta::InputJsonDelta { partial_json } => {
                    let is_synth = Some(index) == self.synthesized_tool_index;
                    if is_synth {
                        out.push(Ok(ModelEvent::TokenDelta { text: partial_json }));
                    } else if let Some(BlockState::ToolUse {
                        call_id,
                        name,
                        name_emitted,
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
            AnthropicEvent::ContentBlockStop { .. } => {
                tracing::debug!(target: "paigasus::anthropic::stream", "content_block_stop");
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
                        self.terminal_emitted = true;
                        out.push(self.finish_or_error(&reason));
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

    /// Flush a stop reason buffered from `message_delta` when the response
    /// body ends cleanly before `message_stop` arrived.
    ///
    /// Returns `None` when a terminal event was already emitted — the
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
    /// **Clean-EOF path only.** Never call this on the cancellation or
    /// transport-error paths — see `model.rs`.
    pub(crate) fn finish(&mut self) -> Option<Result<ModelEvent, ModelError>> {
        if self.terminal_emitted {
            return None;
        }
        let reason = self.stop_reason.take()?;
        self.terminal_emitted = true;
        tracing::warn!(
            target: "paigasus::anthropic::stream",
            stop_reason = %reason,
            "stream body ended without message_stop; flushing buffered stop reason",
        );
        Some(self.finish_or_error(&reason))
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
            content_block: ContentBlockHead::Thinking,
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
            t.finish().is_none(),
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
        match t.finish().expect("a buffered reason must flush") {
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
        match t.finish().expect("a buffered reason must flush") {
            Ok(ModelEvent::Finish { reason }) => assert_eq!(reason, FinishReason::ToolCalls),
            other => panic!("expected Ok(Finish::ToolCalls), got {other:?}"),
        }
    }

    /// Truncation before any stop reason: never reported as a clean `Stop`.
    #[test]
    fn finish_is_none_when_no_stop_reason_observed() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::Text,
        });
        assert!(
            t.finish().is_none(),
            "no stop reason was observed; nothing to flush"
        );
    }

    /// The well-formed path: `message_stop` already emitted, so the EOF flush
    /// is a no-op — and stays one on a repeated call.
    #[test]
    fn finish_is_none_after_message_stop_drained_it() {
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
            t.finish().is_none(),
            "message_stop already emitted the terminal event"
        );
        assert!(t.finish().is_none(), "finish() must be idempotent");
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
        match t.finish().expect("a buffered reason must flush") {
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
        match t.finish().expect("a buffered reason must flush") {
            Err(ModelError::Other(_)) => {}
            other => panic!("expected Err(Other), got {other:?}"),
        }
    }
}
