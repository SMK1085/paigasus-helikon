//! Amazon Bedrock (Converse API) provider for the Paigasus Helikon SDK.
//!
//! The public surface is [`BedrockModel`] (a [`paigasus_helikon_core::Model`])
//! and its [`BedrockModelBuilder`]. This is the Bedrock **Converse model
//! provider** — distinct from the `runtime-agentcore` crate (the Bedrock
//! *AgentCore runtime*).
//!
//! ```ignore
//! use paigasus_helikon_providers_bedrock::BedrockModel;
//! # async fn f() -> Result<(), Box<dyn std::error::Error>> {
//! let _model = BedrockModel::from_env("anthropic.claude-3-5-sonnet-20241022-v2:0").await?;
//! # Ok(()) }
//! ```
mod builder;
mod capabilities;
mod document;
mod error;
mod family;
mod model;
mod stream;
mod translate;

pub use builder::{BedrockModelBuilder, BuildError};
pub use family::ModelFamily;
pub use model::BedrockModel;
pub use translate::schema::{rewrite_tool_schema, Ruleset};

/// Re-exports for integration tests.
///
/// This module exposes internal types that are not part of the public library
/// API but are required by the crate's own integration tests in `tests/`.
/// Consumers should not depend on this module — it is subject to change
/// without a semver bump.
#[doc(hidden)]
pub mod testing {
    pub use crate::model::drive_stream_with_token;
    pub use crate::stream::StreamTranslator;
    pub use crate::translate::tools::SYNTHESIZED_TOOL_NAME;
}
