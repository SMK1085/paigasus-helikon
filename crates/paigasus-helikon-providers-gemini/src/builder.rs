//! `GeminiModelBuilder` — fluent constructor for [`crate::GeminiModel`].

use std::sync::Arc;

use paigasus_helikon_core::ModelCapabilities;

use crate::auth::{Auth, TokenProvider};

/// Transport selected at construction.
#[derive(Debug, Clone)]
pub(crate) enum Transport {
    Developer,
    Vertex { project: String, location: String },
}

/// Errors raised while building a [`crate::GeminiModel`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// `GEMINI_API_KEY`/`GOOGLE_API_KEY` not set and no `api_key` supplied.
    #[error("GEMINI_API_KEY/GOOGLE_API_KEY not set and no api_key supplied")]
    MissingApiKey,
    /// The supplied API key is blank (whitespace-only).
    #[error("api key is empty")]
    EmptyApiKey,
    /// Vertex transport requires a bearer token or `TokenProvider`.
    #[error("vertex transport requires a bearer token or TokenProvider")]
    MissingVertexAuth,
    /// Vertex transport requires a non-empty project identifier.
    #[error("vertex transport requires a non-empty project")]
    MissingVertexProject,
    /// Vertex transport requires a non-empty location/region.
    #[error("vertex transport requires a non-empty location")]
    MissingVertexLocation,
    /// The auth credential supplied does not match the selected transport.
    #[error("auth credential does not match the selected transport")]
    AuthTransportMismatch,
    /// The supplied `base_url` is not a valid URL.
    #[error("base_url is not a valid URL: {0}")]
    InvalidBaseUrl(String),
    /// The model id is empty.
    #[error("model id is empty")]
    EmptyModelId,
    /// Application Default Credentials discovery failed (feature `vertex-adc`).
    #[cfg(feature = "vertex-adc")]
    #[error("adc: {0}")]
    Adc(String),
}

/// Builder-baked, immutable per-request config.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: Option<String>,
    pub(crate) model_id: String,
    pub(crate) transport: Transport,
    pub(crate) auth: Auth,
    pub(crate) capabilities: ModelCapabilities,
}

/// Fluent builder for [`crate::GeminiModel`].
#[derive(Debug)]
pub struct GeminiModelBuilder {
    model_id: String,
    transport: Transport,
    api_key: Option<String>,
    bearer: Option<String>,
    token: Option<Arc<dyn TokenProvider>>,
    base_url: Option<String>,
    http: Option<reqwest::Client>,
    caps_override: Option<ModelCapabilities>,
}

impl GeminiModelBuilder {
    pub(crate) fn new(model_id: impl Into<String>, transport: Transport) -> Self {
        Self {
            model_id: model_id.into(),
            transport,
            api_key: None,
            bearer: None,
            token: None,
            base_url: None,
            http: None,
            caps_override: None,
        }
    }

    /// Set the Developer-API key.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }
    /// Set a static Vertex bearer token.
    pub fn bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer = Some(token.into());
        self
    }
    /// Set a Vertex token provider (fresh token per request).
    pub fn token_provider(mut self, p: impl TokenProvider + 'static) -> Self {
        self.token = Some(Arc::new(p));
        self
    }
    /// Override the API base URL (enables proxies / regional hosts).
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }
    /// Use a caller-provided `reqwest::Client`.
    pub fn http_client(mut self, c: reqwest::Client) -> Self {
        self.http = Some(c);
        self
    }
    /// Override the capability flags.
    pub fn with_capabilities(mut self, c: ModelCapabilities) -> Self {
        self.caps_override = Some(c);
        self
    }

    pub(crate) fn build_config(self) -> Result<Config, BuildError> {
        if self.model_id.trim().is_empty() {
            return Err(BuildError::EmptyModelId);
        }
        if let Some(u) = &self.base_url {
            if reqwest::Url::parse(u).is_err() {
                return Err(BuildError::InvalidBaseUrl(u.clone()));
            }
        }
        let auth = match &self.transport {
            Transport::Developer => {
                if self.bearer.is_some() || self.token.is_some() {
                    return Err(BuildError::AuthTransportMismatch);
                }
                let key = self.api_key.ok_or(BuildError::MissingApiKey)?;
                if key.trim().is_empty() {
                    return Err(BuildError::EmptyApiKey);
                }
                Auth::ApiKey(key)
            }
            Transport::Vertex { project, location } => {
                if self.api_key.is_some() {
                    return Err(BuildError::AuthTransportMismatch);
                }
                if project.trim().is_empty() {
                    return Err(BuildError::MissingVertexProject);
                }
                if location.trim().is_empty() {
                    return Err(BuildError::MissingVertexLocation);
                }
                if let Some(t) = self.token {
                    Auth::Token(t)
                } else if let Some(b) = self.bearer {
                    if b.trim().is_empty() {
                        return Err(BuildError::MissingVertexAuth);
                    }
                    Auth::Bearer(b)
                } else {
                    return Err(BuildError::MissingVertexAuth);
                }
            }
        };
        let capabilities = self
            .caps_override
            .unwrap_or_else(|| crate::capabilities::lookup(&self.model_id).caps);
        Ok(Config {
            http: self.http.unwrap_or_default(),
            base_url: self.base_url,
            model_id: self.model_id,
            transport: self.transport,
            auth,
            capabilities,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::GeminiModel;
    use paigasus_helikon_core::Model;

    #[test]
    fn developer_requires_api_key() {
        let err = GeminiModel::developer("gemini-2.5-flash")
            .build()
            .unwrap_err();
        assert!(matches!(err, crate::BuildError::MissingApiKey));
    }

    #[test]
    fn developer_rejects_empty_api_key() {
        let err = GeminiModel::developer("gemini-2.5-flash")
            .api_key("   ")
            .build()
            .unwrap_err();
        assert!(matches!(err, crate::BuildError::EmptyApiKey));
    }

    #[test]
    fn developer_with_key_builds() {
        let m = GeminiModel::developer("gemini-2.5-flash")
            .api_key("k")
            .build()
            .unwrap();
        assert_eq!(m.model(), "gemini-2.5-flash");
        assert_eq!(m.provider(), "gemini");
    }

    #[test]
    fn vertex_requires_auth() {
        let err = GeminiModel::vertex("gemini-2.5-pro", "proj", "us-central1")
            .build()
            .unwrap_err();
        assert!(matches!(err, crate::BuildError::MissingVertexAuth));
    }

    #[test]
    fn vertex_with_bearer_builds() {
        let m = GeminiModel::vertex("gemini-2.5-pro", "proj", "us-central1")
            .bearer_token("ya29.token")
            .build()
            .unwrap();
        assert_eq!(m.model(), "gemini-2.5-pro");
    }

    #[test]
    fn api_key_in_vertex_mode_is_mismatch() {
        let err = GeminiModel::vertex("gemini-2.5-pro", "p", "l")
            .api_key("k")
            .build()
            .unwrap_err();
        assert!(matches!(err, crate::BuildError::AuthTransportMismatch));
    }
}
