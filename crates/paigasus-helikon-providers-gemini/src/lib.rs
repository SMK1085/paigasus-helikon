//! Google Gemini provider for the Paigasus Helikon SDK.
//!
//! # Overview
//!
//! This crate implements [`paigasus_helikon_core::Model`] for Google Gemini,
//! covering both the **Developer API** (API key) and **Vertex AI**
//! (OAuth bearer / [`TokenProvider`]).
//!
//! ## Public surface
//!
//! - [`GeminiModel`] — the `Model` implementation. Constructed via the fluent
//!   [`GeminiModelBuilder`] or the convenience shorthands below.
//! - [`GeminiModelBuilder`] — returned by [`GeminiModel::developer`] and
//!   [`GeminiModel::vertex`]. Call `.build()` to materialise the model.
//! - [`TokenProvider`] — trait for per-request Vertex bearer-token refresh.
//!   Implement it to supply rotating tokens (e.g. Workload Identity Federation).
//! - `AdcTokenProvider` (feature `vertex-adc`) — Application Default
//!   Credentials implementation backed by `gcp_auth`. Use
//!   `GeminiModel::vertex_from_env` for the one-call path.
//! - [`BuildError`] — validation errors raised by `.build()`.
//!
//! ## Transports
//!
//! **Developer API** — authenticates with an API key:
//!
//! ```ignore
//! use paigasus_helikon_providers_gemini::GeminiModel;
//! # fn f() -> Result<(), Box<dyn std::error::Error>> {
//! // Reads GEMINI_API_KEY or GOOGLE_API_KEY from the environment.
//! let model = GeminiModel::from_env("gemini-2.5-flash")?;
//! # Ok(()) }
//! ```
//!
//! **Vertex AI** — authenticates with a bearer token or [`TokenProvider`]:
//!
//! ```ignore
//! use paigasus_helikon_providers_gemini::GeminiModel;
//! # async fn f() -> Result<(), Box<dyn std::error::Error>> {
//! // vertex-adc feature: reads GOOGLE_CLOUD_PROJECT + GOOGLE_CLOUD_LOCATION.
//! let model = GeminiModel::vertex_from_env("gemini-2.5-flash").await?;
//! # Ok(()) }
//! ```
//!
//! ## Structured output
//!
//! Set `ModelSettings::response_format` to `ResponseFormat::JsonSchema`. The
//! provider passes the schema directly to Gemini's `generationConfig.responseSchema`
//! field (native structured output — no forced-tool synthesis). A JSON-Schema
//! sanitizer runs automatically to strip unsupported keywords and rewrite
//! incompatible constructs before sending.
//!
//! Note: Gemini rejects requests that combine `responseSchema` with tool
//! declarations; the provider returns a conflict error before sending if both
//! are present.
//!
//! ## Limitations
//!
//! Remote-URL images, audio parts, and non-text tool-result parts are silently
//! dropped during request translation. Reasoning streaming (`ModelEvent::ReasoningDelta`)
//! is not yet emitted by this provider.

mod auth;
mod builder;
mod capabilities;
mod error;
mod model;
mod sse;
mod stream;
mod translate;
mod transport;

#[cfg(feature = "vertex-adc")]
pub use auth::AdcTokenProvider;
pub use auth::TokenProvider;
pub use builder::{BuildError, GeminiModelBuilder};
pub use model::GeminiModel;
