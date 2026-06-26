//! Serde types for one Gemini `GenerateContentResponse` SSE chunk.

// These wire types are deserialized by the stream tests; they are consumed by
// the HTTP transport / `Model::invoke` SSE loop in Task 10/11.
#![allow(dead_code)]

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiChunk {
    #[serde(default)]
    pub(crate) candidates: Vec<Candidate>,
    #[serde(default)]
    pub(crate) usage_metadata: Option<UsageMetadata>,
    #[serde(default)]
    pub(crate) prompt_feedback: Option<PromptFeedback>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Candidate {
    #[serde(default)]
    pub(crate) content: Option<Content>,
    #[serde(default)]
    pub(crate) finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Content {
    #[serde(default)]
    pub(crate) parts: Vec<Part>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Part {
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) function_call: Option<FunctionCall>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FunctionCall {
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) args: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageMetadata {
    #[serde(default)]
    pub(crate) prompt_token_count: u32,
    #[serde(default)]
    pub(crate) candidates_token_count: u32,
    #[serde(default)]
    pub(crate) cached_content_token_count: Option<u32>,
    #[serde(default)]
    pub(crate) thoughts_token_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PromptFeedback {
    #[serde(default)]
    pub(crate) block_reason: Option<String>,
}
