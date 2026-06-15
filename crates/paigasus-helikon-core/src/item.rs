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
