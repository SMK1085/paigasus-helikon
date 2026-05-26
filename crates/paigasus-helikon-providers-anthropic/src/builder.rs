//! [`AnthropicModelBuilder`] — fluent constructor for [`crate::AnthropicModel`].

use paigasus_helikon_core::ModelCapabilities;
use reqwest::Url;

use crate::capabilities::{self, ModelEntry};
use crate::settings::{CacheStrategy, ExtendedThinking};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

/// Construction-time errors. Runtime errors flow through
/// [`paigasus_helikon_core::ModelError`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// `ANTHROPIC_API_KEY` was unset and no explicit auth was supplied.
    #[error("ANTHROPIC_API_KEY not set in environment")]
    MissingApiKey,
    /// `base_url` failed to parse as a URL.
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
}

#[derive(Debug, Clone)]
enum AuthSource {
    Env,
    ApiKey(String),
    Bearer(String),
}

/// Resolved configuration handed off to [`crate::AnthropicModel`].
#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) model_id: String,
    pub(crate) base_url: String,
    pub(crate) auth_header: AuthHeader,
    pub(crate) anthropic_version: String,
    pub(crate) anthropic_beta: Option<String>,
    pub(crate) cache_strategy: CacheStrategy,
    pub(crate) extended_thinking: ExtendedThinking,
    pub(crate) top_k: Option<u32>,
    pub(crate) max_output_default: u32,
    pub(crate) capabilities: ModelCapabilities,
    pub(crate) http: reqwest::Client,
}

/// One of `x-api-key: <key>` or `authorization: Bearer <token>`.
#[derive(Debug, Clone)]
pub(crate) enum AuthHeader {
    ApiKey(String),
    Bearer(String),
}

/// Fluent builder for [`crate::AnthropicModel`].
#[derive(Debug, Clone)]
pub struct AnthropicModelBuilder {
    model_id: String,
    auth: AuthSource,
    base_url: Option<String>,
    anthropic_version: Option<String>,
    beta_headers: Vec<String>,
    http_client: Option<reqwest::Client>,
    cache_strategy: CacheStrategy,
    extended_thinking: ExtendedThinking,
    top_k: Option<u32>,
    capabilities_override: Option<ModelCapabilities>,
}

impl AnthropicModelBuilder {
    pub(crate) fn new(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            auth: AuthSource::Env,
            base_url: None,
            anthropic_version: None,
            beta_headers: Vec::new(),
            http_client: None,
            cache_strategy: CacheStrategy::None,
            extended_thinking: ExtendedThinking::Disabled,
            top_k: None,
            capabilities_override: None,
        }
    }

    /// Use the given API key. Last-set auth wins.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.auth = AuthSource::ApiKey(key.into());
        self
    }

    /// Use a pre-minted bearer token (Bedrock/Vertex proxy). Last-set auth wins.
    pub fn bearer(mut self, token: impl Into<String>) -> Self {
        self.auth = AuthSource::Bearer(token.into());
        self
    }

    /// Override the base URL. Default: `https://api.anthropic.com`.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Override the `anthropic-version` header. Default: `"2023-06-01"`.
    pub fn anthropic_version(mut self, v: impl Into<String>) -> Self {
        self.anthropic_version = Some(v.into());
        self
    }

    /// Append a value to the `anthropic-beta` header. Multiple calls
    /// accumulate; rendered as a comma-separated list at `build()`.
    pub fn beta(mut self, header: impl Into<String>) -> Self {
        self.beta_headers.push(header.into());
        self
    }

    /// Use a caller-provided `reqwest::Client`.
    pub fn http_client(mut self, c: reqwest::Client) -> Self {
        self.http_client = Some(c);
        self
    }

    /// Prompt-caching strategy. Default: [`CacheStrategy::None`].
    pub fn cache_strategy(mut self, s: CacheStrategy) -> Self {
        self.cache_strategy = s;
        self
    }

    /// Extended-thinking configuration. Default: [`ExtendedThinking::Disabled`].
    pub fn extended_thinking(mut self, t: ExtendedThinking) -> Self {
        self.extended_thinking = t;
        self
    }

    /// Set the `top_k` sampling parameter. Anthropic-specific.
    pub fn top_k(mut self, k: u32) -> Self {
        self.top_k = Some(k);
        self
    }

    /// Override the capability snapshot. Wins over the built-in lookup.
    pub fn with_capabilities(mut self, c: ModelCapabilities) -> Self {
        self.capabilities_override = Some(c);
        self
    }

    /// Resolve auth, validate base URL, look up capabilities, materialize the
    /// internal [`Config`].
    pub(crate) fn build_config(self) -> Result<Config, BuildError> {
        let auth_header = match &self.auth {
            AuthSource::Env => {
                let key =
                    std::env::var("ANTHROPIC_API_KEY").map_err(|_| BuildError::MissingApiKey)?;
                AuthHeader::ApiKey(key)
            }
            AuthSource::ApiKey(k) => AuthHeader::ApiKey(k.clone()),
            AuthSource::Bearer(t) => AuthHeader::Bearer(t.clone()),
        };

        let base_url = self.base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
        if Url::parse(&base_url).is_err() {
            return Err(BuildError::InvalidBaseUrl(base_url));
        }

        let entry: ModelEntry = capabilities::lookup(&self.model_id);
        let capabilities = self.capabilities_override.unwrap_or(entry.caps);

        let anthropic_beta = if self.beta_headers.is_empty() {
            None
        } else {
            Some(self.beta_headers.join(","))
        };

        let http = self.http_client.unwrap_or_default();

        Ok(Config {
            model_id: self.model_id,
            base_url,
            auth_header,
            anthropic_version: self
                .anthropic_version
                .unwrap_or_else(|| DEFAULT_ANTHROPIC_VERSION.to_owned()),
            anthropic_beta,
            cache_strategy: self.cache_strategy,
            extended_thinking: self.extended_thinking,
            top_k: self.top_k,
            max_output_default: entry.max_output_default,
            capabilities,
            http,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn save_and_set_env(value: Option<&str>) -> Option<String> {
        let prev = std::env::var("ANTHROPIC_API_KEY").ok();
        match value {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        prev
    }
    fn restore_env(prev: Option<String>) {
        match prev {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
    }

    #[test]
    fn build_without_env_or_explicit_key_errors_missing_api_key() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(None);
        let r = AnthropicModelBuilder::new("claude-sonnet-4-6").build_config();
        restore_env(prev);
        assert!(matches!(r, Err(BuildError::MissingApiKey)));
    }

    #[test]
    fn build_with_explicit_api_key_succeeds() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(None);
        let c = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .api_key("sk-test")
            .build_config()
            .unwrap();
        restore_env(prev);
        assert!(matches!(c.auth_header, AuthHeader::ApiKey(_)));
        assert_eq!(c.anthropic_version, "2023-06-01");
        assert_eq!(c.max_output_default, 32_768);
    }

    #[test]
    fn build_with_bearer_succeeds() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(None);
        let c = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .bearer("eyJhbGciOi...")
            .build_config()
            .unwrap();
        restore_env(prev);
        assert!(matches!(c.auth_header, AuthHeader::Bearer(_)));
    }

    #[test]
    fn build_reads_env_when_no_explicit_auth() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(Some("sk-from-env"));
        let r = AnthropicModelBuilder::new("claude-sonnet-4-6").build_config();
        restore_env(prev);
        assert!(r.is_ok());
    }

    #[test]
    fn invalid_base_url_errors() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(Some("sk-x"));
        let err = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .base_url("not a url")
            .build_config()
            .unwrap_err();
        restore_env(prev);
        assert!(matches!(err, BuildError::InvalidBaseUrl(_)));
    }

    #[test]
    fn multiple_beta_calls_accumulate_into_comma_list() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(Some("sk-x"));
        let c = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .beta("prompt-caching-2024-07-31")
            .beta("max-tokens-3-5-sonnet-2024-07-15")
            .build_config()
            .unwrap();
        restore_env(prev);
        assert_eq!(
            c.anthropic_beta.as_deref(),
            Some("prompt-caching-2024-07-31,max-tokens-3-5-sonnet-2024-07-15"),
        );
    }

    #[test]
    fn no_beta_calls_yields_no_header() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(Some("sk-x"));
        let c = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .build_config()
            .unwrap();
        restore_env(prev);
        assert!(c.anthropic_beta.is_none());
    }

    #[test]
    fn capability_override_wins_over_lookup() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(Some("sk-x"));
        let custom = ModelCapabilities::empty();
        let c = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .with_capabilities(custom)
            .build_config()
            .unwrap();
        restore_env(prev);
        assert!(!c.capabilities.tools, "override clears tools");
        assert!(!c.capabilities.prompt_caching);
    }

    #[test]
    fn unknown_model_uses_conservative_max_output_default() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(Some("sk-x"));
        let c = AnthropicModelBuilder::new("claude-mystery-9000")
            .build_config()
            .unwrap();
        restore_env(prev);
        assert_eq!(c.max_output_default, 4096);
    }
}
