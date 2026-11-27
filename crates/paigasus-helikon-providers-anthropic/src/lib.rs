//! Anthropic provider — Messages API for the Paigasus Helikon SDK.
//!
//! See [SMA-317] for the design. The public surface is [`AnthropicModel`]
//! (a [`paigasus_helikon_core::Model`] implementation), its
//! [`AnthropicModelBuilder`], and the Anthropic-specific settings types
//! [`CacheStrategy`], [`ExtendedThinking`] and [`Effort`].
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
//! # TLS
//!
//! The default feature `rustls-aws-lc` uses the aws-lc rustls provider. The
//! facade feature `anthropic` uses this default. A host that needs
//! `rustls-no-provider` must not enable the facade feature `anthropic`, because
//! it turns on `rustls-aws-lc` and Cargo features only add. With
//! `default-features = false, features = ["rustls-no-provider"]`, the host
//! must install a rustls `CryptoProvider` as the process default before
//! `build()`, or pass its own client with `http_client`. Otherwise `build()`
//! returns [`BuildError::NoCryptoProvider`]. With both TLS features off,
//! `build()` always needs `http_client`. The key is not trimmed. A key
//! with a byte that a header cannot hold, for example a trailing newline,
//! gives [`BuildError::InvalidAuthCredential`]. `Debug` writes `<redacted>`
//! for the key.
//!
//! # Errors
//!
//! An `invalid_request_error` whose message contains `prompt is too long` maps
//! to `ModelError::ContextLengthExceeded`; this check comes first. Otherwise
//! HTTP 400, 401, 403, 404 and 413 map to `ModelError::Rejected` with the
//! status, the error type and the message. The error type `rate_limit_error`,
//! and the error type `billing_error` with HTTP 429, map to
//! `ModelError::RateLimited` with the `retry-after` value. HTTP 500 to 504
//! and 529 map to `ModelError::Unavailable`. The stop reason `refusal` maps to
//! `ModelError::Refused`. `RetryingModel` does not retry `Rejected`.
//!
//! # Extended thinking in a tool loop
//!
//! The stream keeps each `thinking` and `redacted_thinking` block as a
//! `ContentPart::ProviderData` part with the key [`PROVIDER_DATA_KEY`]. The
//! next request sends the eligible blocks back unchanged. Only `Adaptive` and
//! `Enabled` send them. The shape of the `data` is private to this crate.
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
//! - The beta `interleaved-thinking-2025-05-14` has an effect only on models with manual interleaving, with `ExtendedThinking::Enabled`. These models are Claude Haiku 4.5, Opus 4.5 and Sonnet 4.5. On Claude Sonnet 4.6 the beta also works, but Anthropic deprecated it there. Claude Opus 4.6 has no manual interleaving. Add `.beta("interleaved-thinking-2025-05-14")` to the builder. Then the model can think again after a tool result. `Adaptive` interleaves by itself.
//! - With `ExtendedThinking::Disabled` (the default), the provider sends no thinking block back. On a model that thinks by default, for example `claude-sonnet-5-5`, set `ExtendedThinking::Adaptive` to keep the reasoning between tool calls. The provider then also sends `drop_block`.
//! - `claude-sonnet-5-5` needs `ExtendedThinking::Adaptive`.
//! - With `ExtendedThinking::Adaptive`, the provider sends `block_binding.prefix_mismatch_behavior = "drop_block"` and the beta header `thinking-binding-controls-2026-08-01`. If the history before a thinking block changed, the API drops that block and every later thinking block. The model loses that reasoning. The request does not fail. Examples are a hand-off with other `system` text or tools, and an edit of an earlier message.
//! - With `ExtendedThinking::Enabled`, the provider sends no binding. Use `Enabled` only on models that do not run the prefix check, for example `claude-haiku-4-5`. Claude Opus 4.7 and later, Sonnet 5 and later, Haiku 5.5 and Fable reject `Enabled`.
//! - The default sends no effort, and the model uses its own default. On some models, for example `claude-sonnet-5-5` with the default `medium`, adaptive thinking can skip thinking on a short task. A higher effort makes thinking more likely. To set it, call `.effort(Effort::High)` on the builder. The provider then sends `output_config: { effort: "high" }`. Some models reject this field, so set it only for a model that supports effort.
//!
//! **Cost.** Each request sends the eligible thinking blocks of the history again. We
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
#[cfg(test)]
mod test_support;
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

pub use settings::{CacheStrategy, Effort, ExtendedThinking};
