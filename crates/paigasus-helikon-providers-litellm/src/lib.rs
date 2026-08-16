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

mod capabilities;
mod transport;
