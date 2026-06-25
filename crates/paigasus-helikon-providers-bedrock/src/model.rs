//! [`BedrockModel`] — the Bedrock Converse API model handle.

/// A handle to a Bedrock Converse API model.
///
/// Construct via [`crate::BedrockModelBuilder`] or the `from_env` convenience
/// constructor (implemented in a later task).
#[derive(Debug)]
pub struct BedrockModel {
    _priv: (),
}
