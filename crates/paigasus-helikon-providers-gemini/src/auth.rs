//! Gemini authentication: API key (Developer) or bearer/token-provider (Vertex).

use std::sync::Arc;

use async_trait::async_trait;
use paigasus_helikon_core::ModelError;

/// Supplies a fresh OAuth bearer access token for Vertex requests.
#[async_trait]
pub trait TokenProvider: Send + Sync + std::fmt::Debug {
    /// Return a bearer access token (without the `Bearer ` prefix).
    async fn token(&self) -> Result<String, ModelError>;
}

/// Resolved credential. Representation is crate-private; callers configure it
/// via builder methods.
#[derive(Clone)]
pub(crate) enum Auth {
    ApiKey(String),
    Bearer(String),
    Token(Arc<dyn TokenProvider>),
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Auth::ApiKey(_) => f.write_str("Auth::ApiKey(***)"),
            Auth::Bearer(_) => f.write_str("Auth::Bearer(***)"),
            Auth::Token(_) => f.write_str("Auth::Token(<provider>)"),
        }
    }
}
