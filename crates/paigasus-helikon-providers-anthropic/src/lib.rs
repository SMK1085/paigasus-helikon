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
//! # Extended thinking in a tool loop
//!
//! The stream keeps each `thinking` and `redacted_thinking` block as a
//! `ContentPart::ProviderData` part with the key [`PROVIDER_DATA_KEY`]. The
//! next request sends the blocks back unchanged for every assistant message in
//! the history. The shape of the `data` is private to this crate.
//!
//! A thinking block is sent back only in the same assistant message as a text
//! block or a `tool_use` block of its own response. If its response had
//! `tool_use` blocks, one of them must be in that message. So after a
//! hand-off that removed the tool calls of a response, or when the history
//! lost those calls, the SDK sends no thinking block of that response. At the end of a history, the SDK
//! sends no thinking block in a last assistant message. The cache never marks
//! a thinking block as a breakpoint. A response with no kept thinking block
//! writes no provider data.
//!
//! - For a 4.x model with `ExtendedThinking::Enabled`, add
//!   `.beta("interleaved-thinking-2025-05-14")` to the builder. Then the model
//!   can think again after a tool result.
//! - `claude-sonnet-5-5` needs `ExtendedThinking::Adaptive`.
//!
//! **Cost.** Each request sends the thinking blocks of the history again. We
//! measured this on Claude Sonnet 4.5 and Opus 4.5 with
//! `Enabled { budget_tokens: 2048 }`. It added about 100 input tokens to each
//! round without the beta header. It added about 250 to 300 input tokens with
//! the header.
//!
//! **Recovery.** Anthropic checks each signature. If it rejects one block,
//! each later request of the session fails. To recover, remove every part
//! with `provider == PROVIDER_DATA_KEY` from the stored history.
//!
//! **Privacy.** The thinking text is stored two times: as a `Reasoning` part
//! and inside the provider data. A host that removes `Reasoning` must also
//! remove the parts with this key. If a host removes these parts, the SDK
//! cannot send the thinking blocks back.
//!
//! [SMA-317]: https://linear.app/smaschek/issue/SMA-317

mod builder;
mod capabilities;
mod error;
mod http;
#[cfg(test)]
mod live_round_trip;
mod model;
mod provider_data;
mod settings;
mod sse;
mod stream;
mod translate;

pub use builder::{AnthropicModelBuilder, BuildError};
pub use model::AnthropicModel;
/// The provider id of this crate: `"anthropic"`.
///
/// Use it to declare an Anthropic tool type on a tool, for example
/// `ProviderToolType::new(PROVIDER_ID, "memory_20250818")`.
pub const PROVIDER_ID: &str = "anthropic";

/// The key of the `ProviderData` parts and events that this crate writes.
///
/// A host can filter or remove these parts by this key. The shape of the
/// `data` is private to this crate and can change. This key is separate from
/// [`PROVIDER_ID`], although both have the same value today.
pub const PROVIDER_DATA_KEY: &str = "anthropic";

pub use settings::{CacheStrategy, ExtendedThinking};
