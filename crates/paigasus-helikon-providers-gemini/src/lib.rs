//! Google Gemini provider for the Paigasus Helikon SDK.
//!
//! The public surface is [`GeminiModel`] (a [`paigasus_helikon_core::Model`])
//! and its [`GeminiModelBuilder`]. Supports both the Gemini **Developer API**
//! (API key) and **Vertex AI** (OAuth bearer / `TokenProvider`).
//!
//! ```ignore
//! use paigasus_helikon_providers_gemini::GeminiModel;
//! # async fn f() -> Result<(), Box<dyn std::error::Error>> {
//! let _model = GeminiModel::from_env("gemini-2.5-flash")?;
//! # Ok(()) }
//! ```

mod error;
