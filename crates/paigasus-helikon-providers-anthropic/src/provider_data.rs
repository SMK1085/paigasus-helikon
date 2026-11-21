//! The private `data` shapes of the `ProviderData` parts and events.
//!
//! The stream writes these shapes under [`PROVIDER_DATA_KEY`]. The shapes are
//! private to this crate and can change. A host must not parse them.

use paigasus_helikon_core::{ContentPart, ModelEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::PROVIDER_DATA_KEY;

/// One thinking block of a response, as Anthropic sent it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ThinkingBlock {
    /// A `thinking` block with its text and its signature.
    Thinking {
        /// The joined thinking text. It can be empty.
        thinking: String,
        /// The signature that Anthropic checks on a later request.
        signature: String,
    },
    /// A `redacted_thinking` block with its opaque data.
    RedactedThinking {
        /// The opaque data that Anthropic checks on a later request.
        data: String,
    },
}

impl ThinkingBlock {
    /// A block is valid when Anthropic can check it on a later request.
    ///
    /// A `thinking` block needs a non-empty signature. Its text can be empty.
    /// A `redacted_thinking` block needs non-empty data.
    pub(crate) fn is_valid(&self) -> bool {
        match self {
            Self::Thinking { signature, .. } => !signature.is_empty(),
            Self::RedactedThinking { data } => !data.is_empty(),
        }
    }

    /// The block in its request form.
    ///
    /// The form holds only `type` and the known fields of the block. A field
    /// that the stored data has in addition is not sent.
    pub(crate) fn to_wire(&self) -> Value {
        match self {
            Self::Thinking {
                thinking,
                signature,
            } => json!({
                "type": "thinking",
                "thinking": thinking,
                "signature": signature,
            }),
            Self::RedactedThinking { data } => json!({
                "type": "redacted_thinking",
                "data": data,
            }),
        }
    }

    /// The Anthropic block type, for log lines.
    fn type_name(&self) -> &'static str {
        match self {
            Self::Thinking { .. } => "thinking",
            Self::RedactedThinking { .. } => "redacted_thinking",
        }
    }
}

/// The `data` of one `ProviderData` part or event of this crate.
///
/// `index` is the Anthropic content-block index in the response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum AnthropicData {
    /// A thinking block.
    Block {
        /// The content-block index of the block.
        index: u32,
        /// The block itself.
        block: ThinkingBlock,
    },
    /// The position of the first text block of the response.
    Text {
        /// The content-block index of the text block.
        index: u32,
    },
    /// The position of a `tool_use` block.
    ToolUse {
        /// The content-block index of the `tool_use` block.
        index: u32,
    },
}

impl AnthropicData {
    /// Make the `ModelEvent` that carries this data.
    pub(crate) fn into_event(self, call_id: Option<String>) -> ModelEvent {
        ModelEvent::ProviderData {
            provider: PROVIDER_DATA_KEY.to_owned(),
            call_id,
            data: serde_json::to_value(self).expect("AnthropicData serializes"),
        }
    }
}

/// Read the data of one part of this crate.
///
/// Returns `None` with no log for a part that is not `ProviderData` or that
/// has the key of another provider. Returns `None` with a debug log for a part
/// with this key that does not parse, or that holds a block that is not valid.
/// The log never holds the content of the part.
pub(crate) fn from_part(part: &ContentPart) -> Option<(AnthropicData, Option<&str>)> {
    let ContentPart::ProviderData {
        provider,
        call_id,
        data,
    } = part
    else {
        return None;
    };
    if provider != PROVIDER_DATA_KEY {
        return None;
    }
    // The parse error is not logged, because it can hold a value of the data.
    let Ok(parsed) = AnthropicData::deserialize(data) else {
        tracing::debug!(
            target: "paigasus::anthropic::translate",
            "skipping anthropic provider data that does not parse",
        );
        return None;
    };
    if let AnthropicData::Block { index, block } = &parsed {
        if !block.is_valid() {
            tracing::debug!(
                target: "paigasus::anthropic::translate",
                index = *index,
                block_type = block.type_name(),
                "skipping anthropic thinking block that is not valid",
            );
            return None;
        }
    }
    Some((parsed, call_id.as_deref()))
}

/// Make a `ProviderData` part of this crate (test helper).
#[cfg(test)]
pub(crate) fn part(data: AnthropicData, call_id: Option<&str>) -> ContentPart {
    ContentPart::ProviderData {
        provider: PROVIDER_DATA_KEY.to_owned(),
        call_id: call_id.map(str::to_owned),
        data: serde_json::to_value(data).expect("AnthropicData serializes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn data_serializes_to_the_documented_shapes() {
        let thinking = AnthropicData::Block {
            index: 0,
            block: ThinkingBlock::Thinking {
                thinking: "t".to_owned(),
                signature: "SIG_FAKE_1".to_owned(),
            },
        };
        assert_eq!(
            serde_json::to_value(&thinking).unwrap(),
            json!({"kind":"block","index":0,"block":{"type":"thinking","thinking":"t","signature":"SIG_FAKE_1"}})
        );
        let redacted = AnthropicData::Block {
            index: 1,
            block: ThinkingBlock::RedactedThinking {
                data: "REDACTED_FAKE_1".to_owned(),
            },
        };
        assert_eq!(
            serde_json::to_value(&redacted).unwrap(),
            json!({"kind":"block","index":1,"block":{"type":"redacted_thinking","data":"REDACTED_FAKE_1"}})
        );
        assert_eq!(
            serde_json::to_value(AnthropicData::Text { index: 2 }).unwrap(),
            json!({"kind":"text","index":2})
        );
        assert_eq!(
            serde_json::to_value(AnthropicData::ToolUse { index: 3 }).unwrap(),
            json!({"kind":"tool_use","index":3})
        );
    }

    #[test]
    fn into_event_uses_the_carrier_key_and_call_id() {
        let event = AnthropicData::ToolUse { index: 3 }.into_event(Some("tu_1".to_owned()));
        match event {
            ModelEvent::ProviderData {
                provider,
                call_id,
                data,
            } => {
                assert_eq!(provider, PROVIDER_DATA_KEY);
                assert_eq!(call_id.as_deref(), Some("tu_1"));
                assert_eq!(data, json!({"kind":"tool_use","index":3}));
            }
            other => panic!("expected ProviderData, got {other:?}"),
        }
    }

    #[test]
    fn is_valid_needs_a_signature_or_data() {
        let t = |thinking: &str, signature: &str| ThinkingBlock::Thinking {
            thinking: thinking.to_owned(),
            signature: signature.to_owned(),
        };
        assert!(t("", "SIG_FAKE_1").is_valid());
        assert!(!t("text", "").is_valid());
        assert!(ThinkingBlock::RedactedThinking {
            data: "REDACTED_FAKE_1".to_owned()
        }
        .is_valid());
        assert!(!ThinkingBlock::RedactedThinking {
            data: String::new()
        }
        .is_valid());
    }

    #[test]
    fn provider_data_key_is_anthropic() {
        // This test pins the public key. A host filters `ProviderData` parts by
        // this value, so a change to it is a breaking change.
        assert_eq!(PROVIDER_DATA_KEY, "anthropic");
    }
}
