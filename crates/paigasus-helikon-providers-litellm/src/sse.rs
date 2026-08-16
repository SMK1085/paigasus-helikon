//! Permissive serde types for one LiteLLM Chat Completions SSE chunk.
//!
//! Every field is `#[serde(default)]` — including `choices`. Measured against
//! LiteLLM 1.97.0: the first delta carries an extra `role`, the finish chunk
//! has `delta: {}`, and the trailing usage chunk has
//! `choices: [{"index":0,"delta":{}}]` with no `finish_reason` at all. A
//! backend behind the proxy may omit more than that, and a single missing
//! field would otherwise fail the whole chunk. See the SMA-451 design §9.1.

use serde::Deserialize;

/// One `data:` frame of a LiteLLM Chat Completions SSE stream.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct StreamChunk {
    /// Per-response choices. Only the first is read (see the design's
    /// single-choice rationale); a proxy configured with `n > 1` returns
    /// more, but that is not this crate's use case.
    pub(crate) choices: Vec<Choice>,
    /// Token-usage snapshot, present only on the trailing usage chunk when
    /// `stream_options.include_usage` is set.
    pub(crate) usage: Option<Usage>,
    /// Defensive: a backend failing mid-generation can emit an error frame
    /// instead of (or alongside) a normal delta. Several OpenAI-compatible
    /// backends emit `"error": null` on an otherwise healthy chunk — serde's
    /// standard `null` → `None` mapping for `Option<T>` already treats that
    /// the same as an absent key, so only a non-null value here is a real
    /// error. Unverified against LiteLLM itself: every reproducible failure
    /// returns non-2xx JSON before the stream opens.
    pub(crate) error: Option<serde_json::Value>,
}

/// One entry of [`StreamChunk::choices`].
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Choice {
    /// The choice index, when the proxy reports one.
    pub(crate) index: Option<u32>,
    /// The incremental delta for this choice.
    pub(crate) delta: Option<Delta>,
    /// Set on the chunk that ends the response for this choice.
    pub(crate) finish_reason: Option<String>,
}

/// The incremental content of a single [`Choice`].
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Delta {
    /// Assistant text fragment.
    pub(crate) content: Option<String>,
    /// LiteLLM normalises Anthropic extended thinking and DeepSeek reasoning
    /// into this field.
    pub(crate) reasoning_content: Option<String>,
    /// Fallback spelling seen on some builds/backends.
    pub(crate) reasoning: Option<String>,
    /// Tool-call fragments for this delta.
    pub(crate) tool_calls: Option<Vec<ToolCallChunk>>,
}

/// One tool-call fragment within a [`Delta`].
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ToolCallChunk {
    /// Position of this tool call among the response's tool calls.
    pub(crate) index: Option<u32>,
    /// Provider-assigned call identifier, when known on this delta.
    pub(crate) id: Option<String>,
    /// The function-call fragment.
    pub(crate) function: Option<FunctionChunk>,
}

/// The function-call portion of a [`ToolCallChunk`].
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct FunctionChunk {
    /// Function-name fragment; fragments across deltas.
    pub(crate) name: Option<String>,
    /// JSON-arguments fragment; fragments across deltas.
    pub(crate) arguments: Option<String>,
}

/// Token-usage snapshot carried on the trailing usage chunk.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Usage {
    /// Prompt / input tokens consumed.
    pub(crate) prompt_tokens: u32,
    /// Completion / output tokens generated.
    pub(crate) completion_tokens: u32,
    /// Absent entirely in observed LiteLLM traffic — the whole object, not
    /// just the field.
    pub(crate) prompt_tokens_details: Option<PromptTokensDetails>,
    /// Reasoning-token breakdown, when the backend reports one.
    pub(crate) completion_tokens_details: Option<CompletionTokensDetails>,
}

/// Prompt-token cache breakdown.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct PromptTokensDetails {
    /// Prompt tokens served from cache.
    pub(crate) cached_tokens: Option<u32>,
}

/// Completion-token reasoning breakdown.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct CompletionTokensDetails {
    /// Tokens spent on reasoning/thinking, separate from visible output.
    pub(crate) reasoning_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the invariant `model.rs`'s mid-stream error check relies on:
    /// a JSON-null `error` deserializes to `None`, indistinguishable from an
    /// absent key — not to `Some(Value::Null)`.
    #[test]
    fn null_error_field_deserializes_to_none() {
        let c: StreamChunk = serde_json::from_str(
            r#"{"choices":[{"index":0,"delta":{"content":"hi"}}],"error":null}"#,
        )
        .expect("must deserialize");
        assert!(c.error.is_none());
        assert_eq!(c.choices.len(), 1);
    }

    #[test]
    fn absent_error_field_deserializes_to_none() {
        let c: StreamChunk =
            serde_json::from_str(r#"{"choices":[{"index":0,"delta":{"content":"hi"}}]}"#)
                .expect("must deserialize");
        assert!(c.error.is_none());
    }

    #[test]
    fn populated_error_field_deserializes_to_some() {
        let c: StreamChunk = serde_json::from_str(
            r#"{"choices":[],"error":{"message":"boom","type":"server_error","code":"500"}}"#,
        )
        .expect("must deserialize");
        assert!(c.error.as_ref().is_some_and(|e| !e.is_null()));
    }
}
