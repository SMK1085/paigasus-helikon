//! LiteLLM proxy provider for the Paigasus Helikon SDK.
//!
//! Talks to a [LiteLLM](https://docs.litellm.ai) proxy over its
//! OpenAI-compatible Chat Completions endpoint, adding LiteLLM's own router
//! and observability fields. See [SMA-451] for the design.
//!
//! # Quick start
//!
//! ```ignore
//! // Ignored under doctest: the example needs a reachable proxy.
//! use paigasus_helikon_providers_litellm::LiteLlmModel;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let _model = LiteLlmModel::chat("claude-sonnet-4")
//!     .base_url("http://litellm:4000")
//!     .api_key("sk-…")
//!     .build()?;
//! # Ok(()) }
//! ```
//!
//! [SMA-451]: https://linear.app/smaschek/issue/SMA-451

mod builder;
mod capabilities;
mod transport;

pub use builder::{BuildError, LiteLlmModelBuilder};

/// LiteLLM proxy provider.
#[derive(Debug, Clone)]
pub struct LiteLlmModel(std::sync::Arc<builder::Config>);

impl LiteLlmModel {
    /// Chat Completions builder for a proxy model alias.
    pub fn chat(model_id: impl Into<String>) -> LiteLlmModelBuilder {
        LiteLlmModelBuilder::new(model_id)
    }

    pub(crate) fn from_config(cfg: builder::Config) -> Self {
        Self(std::sync::Arc::new(cfg))
    }

    #[cfg(test)]
    pub(crate) fn endpoint(&self) -> &str {
        &self.0.endpoint
    }

    #[cfg(test)]
    pub(crate) fn auth(&self) -> Option<&str> {
        self.0.auth.as_deref()
    }
}
