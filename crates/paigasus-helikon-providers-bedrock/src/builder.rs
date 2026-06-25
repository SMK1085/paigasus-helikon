//! Builder for [`crate::BedrockModel`].
use thiserror::Error;

/// Errors that can occur while building a [`crate::BedrockModel`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BuildError {
    /// AWS configuration error.
    #[error("AWS configuration error: {0}")]
    Config(String),
}

/// Builder for [`crate::BedrockModel`].
#[derive(Debug, Default)]
pub struct BedrockModelBuilder {
    _priv: (),
}

/// Baked builder configuration consumed by the translate layer and the model.
///
/// This stub carries only `model_id` for the Tasks 7–9 translation layer.
/// Task 11 will expand it to include the SDK `Client`, capabilities, and
/// `max_output_default`.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Bedrock model identifier (e.g. `anthropic.claude-3-5-sonnet-20241022-v2:0`).
    pub(crate) model_id: String,
}
