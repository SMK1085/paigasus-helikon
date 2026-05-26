//! Anthropic provider — Messages API for the Paigasus Helikon SDK.
//!
//! See [SMA-317] for the design. The public surface is [`AnthropicModel`]
//! (a [`paigasus_helikon_core::Model`] implementation), its
//! [`AnthropicModelBuilder`], and the Anthropic-specific settings types
//! [`CacheStrategy`] and [`ExtendedThinking`].
//!
//! # Quick start
//!
//! ```ignore
//! // Ignored under doctest because the example reads ANTHROPIC_API_KEY
//! // from env, which isn't available in `cargo doc` runs.
//! use paigasus_helikon_providers_anthropic::AnthropicModel;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let _model = AnthropicModel::messages("claude-sonnet-4-6").build()?;
//! # Ok(()) }
//! ```
//!
//! [SMA-317]: https://linear.app/smaschek/issue/SMA-317

mod builder;
mod capabilities;
mod error;
mod http;
mod model;
mod settings;
mod sse;
mod stream;
mod translate;

pub use builder::{AnthropicModelBuilder, BuildError};
// pub use model::AnthropicModel;
pub use settings::{CacheStrategy, ExtendedThinking};
