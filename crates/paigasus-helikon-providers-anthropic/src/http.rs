//! HTTP request building for the Messages endpoint.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};

use crate::builder::{AuthHeader, Config};

const X_API_KEY: HeaderName = HeaderName::from_static("x-api-key");
const ANTHROPIC_VERSION: HeaderName = HeaderName::from_static("anthropic-version");
const ANTHROPIC_BETA: HeaderName = HeaderName::from_static("anthropic-beta");

/// Build the static request headers for the Messages endpoint.
///
/// Auth, version, and optional beta-feature header. `Authorization: Bearer`
/// is used when the builder chose `bearer(...)`, otherwise `x-api-key`.
pub(crate) fn build_headers(cfg: &Config) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    match &cfg.auth_header {
        AuthHeader::ApiKey(k) => {
            h.insert(
                X_API_KEY,
                HeaderValue::from_str(k).expect("API key has invalid header bytes"),
            );
        }
        AuthHeader::Bearer(t) => {
            let v = format!("Bearer {t}");
            h.insert(
                reqwest::header::AUTHORIZATION,
                HeaderValue::from_str(&v).expect("bearer token has invalid header bytes"),
            );
        }
    }
    h.insert(
        ANTHROPIC_VERSION,
        HeaderValue::from_str(&cfg.anthropic_version).expect("anthropic-version invalid"),
    );
    if let Some(beta) = &cfg.anthropic_beta {
        h.insert(
            ANTHROPIC_BETA,
            HeaderValue::from_str(beta).expect("anthropic-beta value invalid"),
        );
    }
    h
}

/// Build the full URL: `<base_url>/v1/messages` with no trailing slash.
pub(crate) fn messages_url(cfg: &Config) -> String {
    let trimmed = cfg.base_url.trim_end_matches('/');
    format!("{trimmed}/v1/messages")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::AnthropicModelBuilder;

    fn build_config_with(auth: &str, beta: &[&str]) -> Config {
        // The builder's tests serialize ANTHROPIC_API_KEY env access; here we
        // skip env entirely by passing api_key explicitly.
        let mut b = AnthropicModelBuilder::new("claude-sonnet-4-6").api_key(auth);
        for v in beta {
            b = b.beta(*v);
        }
        b.build_config().unwrap()
    }

    #[test]
    fn api_key_auth_uses_x_api_key() {
        let cfg = build_config_with("sk-test", &[]);
        let h = build_headers(&cfg);
        assert_eq!(h.get("x-api-key").unwrap().to_str().unwrap(), "sk-test");
        assert!(h.get("authorization").is_none());
        assert_eq!(h.get("content-type").unwrap(), "application/json");
        assert_eq!(h.get("anthropic-version").unwrap(), "2023-06-01");
        assert!(h.get("anthropic-beta").is_none());
    }

    #[test]
    fn bearer_uses_authorization() {
        let b = AnthropicModelBuilder::new("claude-sonnet-4-6").bearer("ey...");
        let cfg = b.build_config().unwrap();
        let h = build_headers(&cfg);
        assert!(h.get("x-api-key").is_none());
        assert_eq!(
            h.get("authorization").unwrap().to_str().unwrap(),
            "Bearer ey..."
        );
    }

    #[test]
    fn beta_header_is_comma_joined() {
        let cfg = build_config_with("sk-x", &["a", "b"]);
        let h = build_headers(&cfg);
        assert_eq!(h.get("anthropic-beta").unwrap().to_str().unwrap(), "a,b");
    }

    #[test]
    fn messages_url_appends_v1_messages() {
        let cfg = build_config_with("sk-x", &[]);
        assert_eq!(messages_url(&cfg), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn messages_url_trims_trailing_slash() {
        let b = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .api_key("sk-x")
            .base_url("https://proxy.example.com/anthropic/");
        let cfg = b.build_config().unwrap();
        assert_eq!(
            messages_url(&cfg),
            "https://proxy.example.com/anthropic/v1/messages"
        );
    }
}
