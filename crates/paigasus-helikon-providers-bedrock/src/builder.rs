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
