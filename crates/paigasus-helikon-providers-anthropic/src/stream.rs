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
        }
    }

    /// Consume one event. Returns the emitted ModelEvents (most calls
    /// emit zero or one; `message_delta` carrying both stop_reason and
    /// usage emits one Usage followed by Finish on `message_stop`).
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
                if let Some(MessageDeltaUsage { output_tokens }) = usage {
                    out.push(Ok(ModelEvent::Usage {
                        input_tokens: self.last_input_tokens,
                        output_tokens,
                        cached_input_tokens: self.last_cached_input_tokens,
                        reasoning_tokens: None,
                    }));
                }
                if let Some(reason) = delta.stop_reason {
                    self.stop_reason = Some(reason);
                }
            }
            AnthropicEvent::MessageStop => {
                if let Some(reason) = self.stop_reason.take() {
                    out.push(self.finish_or_error(&reason));
                }
            }
            AnthropicEvent::Ping => {}
            AnthropicEvent::Error { error } => {
                return Err(map_error_type(None, &error.ty, &error.message, None));
            }
        }
        Ok(out)
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
}
