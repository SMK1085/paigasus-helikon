//! Anthropic SSE event envelope deserialization.

use serde::Deserialize;
use serde_json::Value;

/// The typed envelope for one SSE event from `/v1/messages` (stream mode).
///
/// `#[serde(tag = "type")]` matches Anthropic's `"type": "message_start"` etc.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum AnthropicEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: MessageStartPayload },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: u32,
        content_block: ContentBlockHead,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        index: u32,
        delta: ContentBlockDelta,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop {
        #[allow(dead_code)]
        index: u32,
    },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: MessageDeltaPayload,
        usage: Option<MessageDeltaUsage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: AnthropicErrorPayload },
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MessageStartPayload {
    pub(crate) usage: MessageStartUsage,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MessageStartUsage {
    pub(crate) input_tokens: u32,
    #[serde(default)]
    pub(crate) cache_read_input_tokens: Option<u32>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) cache_creation_input_tokens: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ContentBlockHead {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "thinking")]
    Thinking,
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        input: Value,
    },
}

#[allow(clippy::enum_variant_names)] // Anthropic wire names all end in `_delta`; renaming would break serde
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ContentBlockDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(rename = "signature_delta")]
    SignatureDelta {
        #[allow(dead_code)]
        signature: String,
    },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MessageDeltaPayload {
    #[serde(default)]
    pub(crate) stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MessageDeltaUsage {
    pub(crate) output_tokens: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AnthropicErrorPayload {
    #[serde(rename = "type")]
    pub(crate) ty: String,
    pub(crate) message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deserializes_message_start_with_cache_tokens() {
        let v = json!({
            "type": "message_start",
            "message": {
                "usage": {
                    "input_tokens": 100,
                    "cache_read_input_tokens": 80,
                    "cache_creation_input_tokens": 0
                }
            }
        });
        let e: AnthropicEvent = serde_json::from_value(v).unwrap();
        match e {
            AnthropicEvent::MessageStart { message } => {
                assert_eq!(message.usage.input_tokens, 100);
                assert_eq!(message.usage.cache_read_input_tokens, Some(80));
            }
            other => panic!("wrong variant {other:?}"),
        }
    }

    #[test]
    fn deserializes_text_delta() {
        let v = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Hi"}
        });
        let e: AnthropicEvent = serde_json::from_value(v).unwrap();
        match e {
            AnthropicEvent::ContentBlockDelta {
                index,
                delta: ContentBlockDelta::TextDelta { text },
            } => {
                assert_eq!(index, 0);
                assert_eq!(text, "Hi");
            }
            other => panic!("wrong variant {other:?}"),
        }
    }

    #[test]
    fn deserializes_tool_use_start_and_input_json_delta() {
        let start = json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {"type": "tool_use", "id": "tu_1", "name": "search", "input": {}}
        });
        let e: AnthropicEvent = serde_json::from_value(start).unwrap();
        match e {
            AnthropicEvent::ContentBlockStart {
                index: 1,
                content_block: ContentBlockHead::ToolUse { id, name, .. },
            } => {
                assert_eq!(id, "tu_1");
                assert_eq!(name, "search");
            }
            other => panic!("wrong variant {other:?}"),
        }

        let delta = json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "{\"q\":"}
        });
        let e: AnthropicEvent = serde_json::from_value(delta).unwrap();
        match e {
            AnthropicEvent::ContentBlockDelta {
                delta: ContentBlockDelta::InputJsonDelta { partial_json },
                ..
            } => {
                assert_eq!(partial_json, "{\"q\":");
            }
            other => panic!("wrong variant {other:?}"),
        }
    }

    #[test]
    fn deserializes_message_delta_with_stop_and_usage() {
        let v = json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 42}
        });
        let e: AnthropicEvent = serde_json::from_value(v).unwrap();
        match e {
            AnthropicEvent::MessageDelta { delta, usage } => {
                assert_eq!(delta.stop_reason.as_deref(), Some("end_turn"));
                assert_eq!(usage.unwrap().output_tokens, 42);
            }
            other => panic!("wrong variant {other:?}"),
        }
    }

    #[test]
    fn deserializes_error_event() {
        let v = json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "busy"}
        });
        let e: AnthropicEvent = serde_json::from_value(v).unwrap();
        match e {
            AnthropicEvent::Error { error } => {
                assert_eq!(error.ty, "overloaded_error");
                assert_eq!(error.message, "busy");
            }
            other => panic!("wrong variant {other:?}"),
        }
    }
}
