//! Canonical wire-format messages and content blocks.
//!
//! [`Item`] is the superset of OpenAI Chat Completions, OpenAI Responses,
//! Anthropic Messages, and Bedrock Converse content shapes. Provider crates
//! serialize the variant native to their wire format and deserialize the
//! variant the provider returns; both round-trip without lossy translation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Canonical wire-format message.
///
/// `ToolCall` and `ToolResult` mirror OpenAI's sibling "tool" role.
/// Anthropic providers emit equivalent [`ContentPart::ToolUse`] and
/// [`ContentPart::ToolResult`] blocks nested inside `AssistantMessage` /
/// `UserMessage` respectively. Both shapes round-trip cleanly through this
/// type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Item {
    /// A user-authored message.
    UserMessage {
        /// One or more content blocks.
        content: Vec<ContentPart>,
    },
    /// An assistant-authored message.
    AssistantMessage {
        /// One or more content blocks.
        content: Vec<ContentPart>,
        /// Name of the agent that produced this message, when known.
        /// `Option` because the wire format can lose attribution (e.g. a
        /// raw provider response deserialized without context). The
        /// session log keeps `agent: String` because the runner always
        /// knows which agent emitted.
        #[serde(skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    /// A system message.
    System {
        /// One or more content blocks (typically a single `Text` block).
        content: Vec<ContentPart>,
    },
    /// OpenAI-style sibling-role tool call.
    ToolCall {
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        name: String,
        /// JSON arguments.
        args: serde_json::Value,
    },
    /// OpenAI-style "tool" role response.
    ToolResult {
        /// Matching call identifier.
        call_id: String,
        /// One or more content blocks (Anthropic permits text + image inside
        /// a tool result).
        content: Vec<ContentPart>,
    },
}

impl Item {
    /// True for an `AssistantMessage` whose content is not empty and holds only
    /// [`ContentPart::ProviderData`] parts.
    ///
    /// Such a message carries no text for a reader. A text consumer can skip it.
    pub fn is_provider_data_only(&self) -> bool {
        match self {
            Item::AssistantMessage { content, .. } => {
                !content.is_empty()
                    && content
                        .iter()
                        .all(|p| matches!(p, ContentPart::ProviderData { .. }))
            }
            _ => false,
        }
    }
}

/// One content block inside an [`Item`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentPart {
    /// Plain text.
    Text {
        /// The text payload.
        text: String,
    },
    /// An image, by URL or inline base64.
    Image {
        /// Where the image bytes come from.
        source: MediaSource,
    },
    /// Audio, by URL or inline base64.
    Audio {
        /// Where the audio bytes come from.
        source: MediaSource,
    },
    /// Anthropic-style tool_use block nested inside an `AssistantMessage`.
    /// Equivalent to a top-level [`Item::ToolCall`].
    ToolUse {
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        name: String,
        /// JSON arguments.
        args: serde_json::Value,
    },
    /// Anthropic-style tool_result block nested inside a `UserMessage`.
    /// Equivalent to a top-level [`Item::ToolResult`]. The inner content is
    /// itself a `Vec<ContentPart>` because Anthropic permits text + image
    /// blocks inside a tool_result.
    ToolResult {
        /// Matching call identifier.
        call_id: String,
        /// Content blocks comprising the tool's output.
        content: Vec<ContentPart>,
    },
    /// Provider-emitted reasoning trace (e.g. Anthropic extended thinking,
    /// OpenAI reasoning summaries).
    Reasoning {
        /// The reasoning text payload.
        text: String,
    },
    /// Opaque data that one provider translator owns, for example a Gemini
    /// `thoughtSignature`.
    ///
    /// - `provider` is a wire-format key that one translator owns. It is not
    ///   the value of `Model::provider()`. Each provider crate that writes
    ///   provider data exports its key as a `pub const`, so a host can filter
    ///   or remove the data.
    /// - A translator reads only the parts with its own key. It ignores every
    ///   other part without a warning, and it never fails because of one.
    /// - The part is valid only inside an [`Item::AssistantMessage`]. Every
    ///   translator ignores it in any other item.
    /// - `call_id: Some(id)` means the data belongs to the tool call with that
    ///   id in the same model response. That call is an [`Item::ToolCall`]
    ///   that follows the message, or a [`ContentPart::ToolUse`] in the same
    ///   message. Call ids can repeat across responses.
    /// - The position of the part in `content` has no meaning. `ProviderData`
    ///   parts keep their arrival order among themselves. A provider that
    ///   needs block structure stores the whole block in `data`.
    /// - Rollback note: a reader on an earlier core version fails on
    ///   `provider_data` with "unknown variant". After the first turn that
    ///   stores this variant, a session store or a Temporal history holds it.
    ///   Deploy readers and Temporal workflow workers before writers. Do not
    ///   roll back core below this version on a deployment that stored such a
    ///   turn.
    ProviderData {
        /// Wire-format key of the translator that owns `data`, for example
        /// `"gemini"`.
        provider: String,
        /// The tool call that `data` belongs to, when it belongs to one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        /// Opaque payload. Only the owning translator reads it.
        data: serde_json::Value,
    },
}

/// Source of a multimedia content block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MediaSource {
    /// Remote URL.
    Url {
        /// Absolute URL of the media resource.
        url: String,
    },
    /// Inline base64-encoded bytes.
    Base64 {
        /// IANA media type (e.g. `image/png`, `audio/wav`).
        mime_type: String,
        /// Base64-encoded payload.
        data: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pd() -> ContentPart {
        ContentPart::ProviderData {
            provider: "gemini".into(),
            call_id: None,
            data: serde_json::json!({"k": 1}),
        }
    }

    fn assistant(content: Vec<ContentPart>) -> Item {
        Item::AssistantMessage {
            content,
            agent: None,
        }
    }

    #[test]
    fn is_provider_data_only_cases() {
        assert!(!assistant(vec![]).is_provider_data_only());
        assert!(assistant(vec![pd()]).is_provider_data_only());
        assert!(
            !assistant(vec![ContentPart::Text { text: "a".into() }, pd()]).is_provider_data_only()
        );
        assert!(
            !assistant(vec![ContentPart::Reasoning { text: "r".into() }]).is_provider_data_only()
        );
        assert!(!Item::UserMessage {
            content: vec![pd()]
        }
        .is_provider_data_only());
    }
}
