//! OpenAI provider — Chat Completions + Responses APIs for the Paigasus
//! Helikon SDK.
//!
//! See [SMA-316] for the design. The public surface is [`OpenAiModel`] (a
//! [`paigasus_helikon_core::Model`] implementation) and its
//! [`OpenAiModelBuilder`].
//!
//! # Quick start
//!
//! ```ignore
//! // Ignored under doctest because the example reads OPENAI_API_KEY
//! // from env, which isn't available in `cargo doc` runs.
//! use paigasus_helikon_providers_openai::OpenAiModel;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let _model = OpenAiModel::chat("gpt-4o").build()?;  // reads OPENAI_API_KEY
//! # Ok(()) }
//! ```
//!
//! [SMA-316]: https://linear.app/smaschek/issue/SMA-316

mod backend;
mod builder;
mod capabilities;
mod error;
mod model;
mod translate;

pub use builder::{BuildError, OpenAiModelBuilder};
pub use model::OpenAiModel;
