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
//! let _model = BedrockModel::from_env("global.anthropic.claude-sonnet-5-5").await?;
//! # Ok(()) }
//! ```
mod builder;
mod capabilities;
mod document;
mod error;
mod fallback;
mod family;
mod model;
mod policy;
mod stream;
mod translate;

pub use builder::{BedrockModelBuilder, BuildError};
pub use family::ModelFamily;
pub use model::BedrockModel;
pub use translate::schema::{rewrite_tool_schema, Ruleset};
