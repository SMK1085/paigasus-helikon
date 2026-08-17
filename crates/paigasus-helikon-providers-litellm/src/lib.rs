//! LiteLLM proxy provider for the Paigasus Helikon SDK.
//!
//! Talks to a [LiteLLM](https://docs.litellm.ai) proxy over its
//! OpenAI-compatible Chat Completions endpoint, adding LiteLLM's own router
//! and observability fields. See [SMA-451] for the design.
//!
//! # When to use this instead of the OpenAI provider
//!
//! `OpenAiModel::base_url()` can also point at a LiteLLM proxy, and is the
//! better choice when the proxy simply fronts OpenAI models. Reach for this
//! crate when you need LiteLLM's router fallbacks and retries, spend/trace
//! metadata, reasoning streaming from non-OpenAI backends, or arbitrary
//! operator-chosen model aliases.
//!
//! # Capabilities are your declaration
//!
//! A LiteLLM alias (`prod-fast`, `team-a/gpt`) carries no information the SDK
//! can act on, so [`LiteLlmModel`] defaults to a conservative
//! streaming + tools capability set. Declare what the backend actually
//! supports with `with_capabilities`.
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
//!     .fallbacks(["gpt-4o-mini"])
//!     .num_retries(2)
//!     .build()?;
//! # Ok(()) }
//! ```
//!
//! # Limitations
//!
//! Chat Completions only — there is no Responses backend, and
//! `ModelSettings::previous_response_id` is ignored. The provider always
//! streams. Multi-choice responses (`n > 1`) are not supported: only the first
//! choice is read.
//!
//! [SMA-451]: https://linear.app/smaschek/issue/SMA-451

mod builder;
mod capabilities;
mod error;
mod model;
mod sse;
mod stream;
mod translate;
mod transport;

pub use builder::{BuildError, LiteLlmModelBuilder};
pub use model::LiteLlmModel;
