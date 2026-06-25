//! Bedrock model family detection and capability routing.

/// Identifies the model family of a Bedrock Converse model ID.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelFamily {
    /// Anthropic Claude model family.
    Anthropic,
    /// Amazon Titan / Nova model family.
    Amazon,
    /// Meta Llama model family.
    Meta,
    /// Mistral model family.
    Mistral,
    /// Cohere Command model family.
    Cohere,
    /// AI21 Labs Jamba model family.
    Ai21,
    /// Unknown / unrecognized model family.
    Unknown,
}
