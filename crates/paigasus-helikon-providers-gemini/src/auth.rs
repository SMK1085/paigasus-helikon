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

/// Application Default Credentials (ADC) integration, gated by `vertex-adc`.
#[cfg(feature = "vertex-adc")]
mod adc {
    use std::sync::Arc;

    // `gcp_auth` ships its own `TokenProvider` trait; alias it to avoid clashing
    // with the crate-local [`super::TokenProvider`].
    use gcp_auth::TokenProvider as GcpTokenProvider;
    use paigasus_helikon_core::ModelError;

    use super::{async_trait, TokenProvider};

    /// OAuth scope granting access to Vertex AI (and other Cloud Platform APIs).
    const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

    /// Application Default Credentials token provider, backed by `gcp_auth`.
    ///
    /// Discovers credentials from the ambient environment — in order:
    /// `GOOGLE_APPLICATION_CREDENTIALS`, the gcloud
    /// `application_default_credentials.json`, the GCE/Cloud Run metadata
    /// server, or the `gcloud` CLI. Tokens are cached and refreshed by
    /// `gcp_auth` for their lifetime, so each request mints a fresh bearer only
    /// when the cached one has expired.
    ///
    /// Enabled by the `vertex-adc` cargo feature.
    pub struct AdcTokenProvider {
        provider: Arc<dyn GcpTokenProvider>,
    }

    // `gcp_auth::TokenProvider` is not `Debug`, so the field can't derive it;
    // emit a redacted form (never expose credential material).
    impl std::fmt::Debug for AdcTokenProvider {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("AdcTokenProvider { .. }")
        }
    }

    impl AdcTokenProvider {
        /// Build from the ambient ADC environment.
        ///
        /// Runs `gcp_auth`'s credential discovery, which may issue a network
        /// request (e.g. to the metadata server); call it once and reuse the
        /// resulting provider.
        pub async fn from_env() -> Result<Self, ModelError> {
            let provider = gcp_auth::provider()
                .await
                .map_err(|e| ModelError::Other(anyhow::anyhow!("gcp_auth provider: {e}")))?;
            Ok(Self { provider })
        }
    }

    #[async_trait]
    impl TokenProvider for AdcTokenProvider {
        async fn token(&self) -> Result<String, ModelError> {
            let token = self
                .provider
                .token(&[SCOPE])
                .await
                .map_err(|e| ModelError::Other(anyhow::anyhow!("gcp_auth token: {e}")))?;
            Ok(token.as_str().to_owned())
        }
    }
}

#[cfg(feature = "vertex-adc")]
pub use adc::AdcTokenProvider;
