//! The [`Model`] trait — the single canonical async interface to an LLM
//! provider — and its carrier types.
//!
//! One trait covers OpenAI Chat Completions, OpenAI Responses, Anthropic
//! Messages, Bedrock Converse, and Gemini `FunctionDeclaration`. Capability
//! differences are surfaced via [`ModelCapabilities`], not split traits.

use async_trait::async_trait;
use futures_core::stream::BoxStream;

use crate::CancellationToken;

/// An LLM provider. The single canonical async interface.
///
/// One trait covers Chat Completions, Responses, Anthropic Messages,
/// Bedrock Converse, and Gemini `FunctionDeclaration`. Capability
/// differences are surfaced via [`ModelCapabilities`], not split traits.
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use futures_core::stream::BoxStream;
/// use paigasus_helikon_core::{
///     CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent,
///     ModelRequest,
/// };
///
/// struct NoopModel;
///
/// #[async_trait]
/// impl Model for NoopModel {
///     async fn invoke(
///         &self,
///         _request: ModelRequest,
///         _cancel: CancellationToken,
///     ) -> Result<
///         BoxStream<'static, Result<ModelEvent, ModelError>>,
///         ModelError,
///     > {
///         Err(ModelError::Unavailable)
///     }
///
///     fn capabilities(&self) -> ModelCapabilities {
///         ModelCapabilities::default()
///     }
/// }
/// ```
#[async_trait]
pub trait Model: Send + Sync {
    /// Invoke the model. Returns a stream of [`ModelEvent`]s on success or a
    /// [`ModelError`] if the request could not be sent. Individual events in
    /// the stream may themselves carry a [`ModelError`].
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError>;

    /// Provider capabilities. Stable across calls.
    fn capabilities(&self) -> ModelCapabilities;
}

/// The request envelope crossing the model boundary.
///
/// Carries the conversation, the tools available for the model to
/// invoke, and provider-tuning knobs. Field shape is the minimum SMA-314
/// needs to drive the loop; SMA-316 / SMA-317 add `tool_choice`,
/// `response_format`, `temperature`, and `previous_response_id`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ModelRequest {
    /// The full accumulated conversation so far.
    pub messages: Vec<crate::Item>,
    /// Tool definitions the model may invoke this turn.
    pub tools: Vec<ToolDef>,
    /// Provider-tuning knobs.
    pub model_settings: ModelSettings,
}

impl ModelRequest {
    /// Construct an empty request. Callers populate fields directly.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Owned snapshot of a [`crate::Tool`] for cross-async-boundary use
/// inside [`ModelRequest`].
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// Identifier the model uses when emitting a tool call.
    pub name: String,
    /// One-line tool description shown to the model.
    pub description: String,
    /// JSON Schema for the tool's argument object.
    pub schema: serde_json::Value,
}

/// Provider-tuning knobs (temperature, max tokens, sampling, ...).
///
/// Field shape lands with SMA-316 / SMA-317. Today this is a
/// `#[non_exhaustive]` placeholder so [`ModelRequest::model_settings`]
/// has a type.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ModelSettings {}

impl ModelSettings {
    /// Construct default model settings.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Streaming union — token, reasoning, tool-call delta, finish.
///
/// See ADR-1 (*Single Model trait with capabilities flags*).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ModelEvent {
    /// A chunk of assistant text.
    TokenDelta {
        /// The text fragment.
        text: String,
    },
    /// A chunk of reasoning/scratchpad text (for providers that emit it
    /// separately from the assistant text channel).
    ReasoningDelta {
        /// The text fragment.
        text: String,
    },
    /// A partial tool call. `name` is `Some` on the first delta for a given
    /// `call_id`, then `None` on subsequent deltas as `args_delta` chunks
    /// arrive.
    ToolCallDelta {
        /// Provider-assigned identifier for the call.
        call_id: String,
        /// Tool name; `Some` on the first delta only.
        name: Option<String>,
        /// JSON-encoded argument fragment.
        args_delta: String,
    },
    /// Terminal event for a single response.
    Finish {
        /// Why the response ended.
        reason: FinishReason,
    },
}

/// Why a single model response stopped emitting tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FinishReason {
    /// Natural stop.
    Stop,
    /// Hit the model's max-output-tokens limit.
    Length,
    /// Model emitted tool calls and is awaiting their results.
    ToolCalls,
    /// Provider's content filter rejected the response.
    ContentFilter,
    /// Provider-specific stop reason that does not map to a known variant.
    Other(String),
}

/// Provider capability flags. See ADR-1.
///
/// Capability flags inform the agent loop's behavior (e.g. whether to use
/// JSON-mode structured output, whether to expect parallel tool calls).
/// They are stable per [`Model`] instance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelCapabilities {
    /// Provider streams tokens.
    pub streaming: bool,
    /// Provider supports tool/function calling.
    pub tools: bool,
    /// Provider can emit multiple tool calls in a single response.
    pub parallel_tool_calls: bool,
    /// Provider supports schema-constrained structured output.
    pub structured_output: bool,
    /// Provider holds conversation state server-side (e.g. OpenAI
    /// Responses' `previous_response_id`).
    pub server_managed_state: bool,
    /// Provider emits reasoning tokens distinct from the main channel.
    pub reasoning: bool,
    /// Provider accepts image inputs.
    pub vision: bool,
    /// Provider accepts audio inputs.
    pub audio: bool,
}

/// Errors raised by [`Model::invoke`] or surfaced through the
/// [`ModelEvent`] stream.
///
/// Per ADR-10 (*No silent auto-retry inside the loop*), the runner never
/// retries on these — retries are an application-layer concern configured
/// via `RunConfig::retry_policy` (lands with the runner ticket).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModelError {
    /// Provider returned a no-route / 503 / connection-refused style error.
    #[error("model provider unavailable")]
    Unavailable,

    /// Provider rate-limited the request. `retry_after_ms` carries the
    /// provider's hint when one is supplied (e.g. via `Retry-After`).
    #[error("rate limited (retry after {retry_after_ms:?} ms)")]
    RateLimited {
        /// Provider-supplied retry hint in milliseconds.
        retry_after_ms: Option<u64>,
    },

    /// Request exceeded the provider's context-length limit.
    #[error("context length exceeded")]
    ContextLengthExceeded,

    /// Provider refused the request (content policy, account state, …).
    #[error("model refused: {reason}")]
    Refused {
        /// Human-readable reason supplied by the provider.
        reason: String,
    },

    /// Transport-level failure (DNS, TLS, socket reset). The string is
    /// provider-formatted.
    #[error("transport error: {0}")]
    Transport(String),

    /// Escape hatch for arbitrary upstream failures. See ADR-10.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
