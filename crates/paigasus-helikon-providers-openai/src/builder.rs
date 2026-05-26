//! `OpenAiModelBuilder` — fluent constructor for [`OpenAiModel`].
//!
//! Consumed by [`OpenAiModel::chat`] / [`OpenAiModel::responses`] and
//! produces an [`OpenAiModel`] via [`OpenAiModelBuilder::build`]. Auth
//! defaults to reading `OPENAI_API_KEY` from the environment; explicit
//! [`Self::api_key`] or [`Self::bearer`] override.

use crate::capabilities::{self, Backend};
use crate::model::OpenAiModel;
use async_openai::config::OpenAIConfig;
use paigasus_helikon_core::ModelCapabilities;

#[allow(dead_code)]
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Debug, Clone)]
enum AuthSource {
    Env,
    ApiKey(String),
    Bearer(String),
}

/// Fluent builder for [`OpenAiModel`].
#[derive(Debug, Clone)]
pub struct OpenAiModelBuilder {
    pub(crate) model_id: String,
    pub(crate) backend: Backend,
    auth: AuthSource,
    base_url: Option<String>,
    organization: Option<String>,
    project: Option<String>,
    http_client: Option<reqwest::Client>,
    capabilities_override: Option<ModelCapabilities>,
}

/// Construction-time errors. Runtime errors flow through
/// [`paigasus_helikon_core::ModelError`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// `OPENAI_API_KEY` was unset and no explicit auth was supplied.
    #[error("OPENAI_API_KEY not set in environment")]
    MissingApiKey,
    /// `base_url` failed to parse as a URL.
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
}

impl OpenAiModelBuilder {
    pub(crate) fn new(model_id: impl Into<String>, backend: Backend) -> Self {
        Self {
            model_id: model_id.into(),
            backend,
            auth: AuthSource::Env,
            base_url: None,
            organization: None,
            project: None,
            http_client: None,
            capabilities_override: None,
        }
    }

    /// Use the given API key. Last-set auth wins.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.auth = AuthSource::ApiKey(key.into());
        self
    }

    /// Use a pre-minted bearer token (Azure AD, custom proxy). Last-set auth wins.
    pub fn bearer(mut self, token: impl Into<String>) -> Self {
        self.auth = AuthSource::Bearer(token.into());
        self
    }

    /// Override the base URL (LiteLLM, vLLM, Azure-via-proxy, etc.).
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Set the `OpenAI-Organization` header.
    pub fn organization(mut self, org: impl Into<String>) -> Self {
        self.organization = Some(org.into());
        self
    }

    /// Set the `OpenAI-Project` header.
    pub fn project(mut self, project: impl Into<String>) -> Self {
        self.project = Some(project.into());
        self
    }

    /// Use a caller-provided `reqwest::Client`.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Override the capability snapshot. Wins over the built-in model lookup table.
    pub fn with_capabilities(mut self, caps: ModelCapabilities) -> Self {
        self.capabilities_override = Some(caps);
        self
    }

    /// Resolve auth, validate base URL, look up capabilities, produce [`OpenAiModel`].
    pub fn build(self) -> Result<OpenAiModel, BuildError> {
        let api_key = match &self.auth {
            AuthSource::Env => std::env::var("OPENAI_API_KEY")
                .map_err(|_| BuildError::MissingApiKey)?,
            AuthSource::ApiKey(k) => k.clone(),
            AuthSource::Bearer(t) => t.clone(),
        };

        let mut config = OpenAIConfig::new().with_api_key(api_key);
        if let Some(url) = &self.base_url {
            if reqwest::Url::parse(url).is_err() {
                return Err(BuildError::InvalidBaseUrl(url.clone()));
            }
            config = config.with_api_base(url);
        }
        if let Some(org) = &self.organization {
            config = config.with_org_id(org);
        }
        if let Some(project) = &self.project {
            config = config.with_project_id(project);
        }

        let caps = self.capabilities_override.unwrap_or_else(|| {
            capabilities::mask_for_backend(
                capabilities::lookup(&self.model_id),
                self.backend,
            )
        });

        let client = match self.http_client {
            Some(hc) => async_openai::Client::with_config(config).with_http_client(hc),
            None => async_openai::Client::with_config(config),
        };

        Ok(OpenAiModel::new(self.model_id, self.backend, client, caps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::Model as _;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    // Serialize env-mutating tests to prevent races.
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn save_and_set_env_key(value: Option<&str>) -> Option<String> {
        let prev = std::env::var("OPENAI_API_KEY").ok();
        match value {
            Some(v) => std::env::set_var("OPENAI_API_KEY", v),
            None => std::env::remove_var("OPENAI_API_KEY"),
        }
        prev
    }
    fn restore_env_key(prev: Option<String>) {
        match prev {
            Some(v) => std::env::set_var("OPENAI_API_KEY", v),
            None => std::env::remove_var("OPENAI_API_KEY"),
        }
    }

    #[test]
    fn build_without_env_or_explicit_key_errors_missing_api_key() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env_key(None);
        let result = OpenAiModelBuilder::new("gpt-4o", Backend::Chat).build();
        restore_env_key(prev);
        assert!(matches!(result, Err(BuildError::MissingApiKey)));
    }

    #[test]
    fn build_with_explicit_api_key_succeeds() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env_key(None);
        let model = OpenAiModelBuilder::new("gpt-4o", Backend::Chat)
            .api_key("sk-test")
            .build()
            .expect("explicit api_key bypasses env lookup");
        restore_env_key(prev);
        assert!(model.capabilities().tools);
    }

    #[test]
    fn build_with_bearer_token_succeeds() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env_key(None);
        let model = OpenAiModelBuilder::new("gpt-4o", Backend::Chat)
            .bearer("eyJhbGciOi...")
            .build()
            .expect("bearer bypasses env lookup");
        restore_env_key(prev);
        assert!(model.capabilities().streaming);
    }

    #[test]
    fn build_reads_env_when_no_explicit_auth() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env_key(Some("sk-from-env"));
        let model = OpenAiModelBuilder::new("gpt-4o", Backend::Chat).build();
        restore_env_key(prev);
        assert!(model.is_ok());
    }

    #[test]
    fn invalid_base_url_returns_error() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env_key(Some("sk-x"));
        let err = OpenAiModelBuilder::new("gpt-4o", Backend::Chat)
            .base_url("not a url")
            .build()
            .unwrap_err();
        restore_env_key(prev);
        assert!(matches!(err, BuildError::InvalidBaseUrl(_)));
    }

    #[test]
    fn with_capabilities_override_wins_over_table_lookup() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env_key(Some("sk-x"));
        let custom = ModelCapabilities::empty();
        let model = OpenAiModelBuilder::new("gpt-4o", Backend::Chat)
            .with_capabilities(custom)
            .build()
            .unwrap();
        restore_env_key(prev);
        assert!(!model.capabilities().tools, "override should clear tools");
        assert!(!model.capabilities().vision, "override should clear vision");
    }

    #[test]
    fn responses_backend_preserves_reasoning_and_server_state_for_o3() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env_key(Some("sk-x"));
        let model = OpenAiModelBuilder::new("o3", Backend::Responses).build().unwrap();
        restore_env_key(prev);
        assert!(model.capabilities().reasoning);
        assert!(model.capabilities().server_managed_state);
    }

    #[test]
    fn chat_backend_masks_reasoning_for_o3() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env_key(Some("sk-x"));
        let model = OpenAiModelBuilder::new("o3", Backend::Chat).build().unwrap();
        restore_env_key(prev);
        assert!(!model.capabilities().reasoning);
        assert!(!model.capabilities().server_managed_state);
    }
}
