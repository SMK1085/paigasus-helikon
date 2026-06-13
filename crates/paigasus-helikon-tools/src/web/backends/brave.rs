//! [`BraveBackend`] — the Brave Search API behind [`SearchBackend`].

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use crate::web::http::build_client;
use crate::web::search::{SearchBackend, SearchResult};

const DEFAULT_UA: &str = concat!("paigasus-helikon-tools/", env!("CARGO_PKG_VERSION"));
const DEFAULT_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

/// Brave Search API backend.
pub struct BraveBackend {
    client: reqwest::Client,
    api_key: String,
    endpoint: String,
}

impl BraveBackend {
    /// Build a backend with an explicit API key.
    pub fn new(api_key: impl Into<String>) -> Result<Self, anyhow::Error> {
        let client = build_client(DEFAULT_UA, Duration::from_secs(30), true)
            .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {e}"))?;
        Ok(Self {
            client,
            api_key: api_key.into(),
            endpoint: DEFAULT_ENDPOINT.to_owned(),
        })
    }

    /// Build a backend, reading the key from `BRAVE_SEARCH_API_KEY`.
    pub fn from_env() -> Result<Self, anyhow::Error> {
        let key = std::env::var("BRAVE_SEARCH_API_KEY")
            .map_err(|_| anyhow::anyhow!("BRAVE_SEARCH_API_KEY is not set"))?;
        Self::new(key)
    }

    #[cfg(test)]
    fn with_endpoint(api_key: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            client: build_client(DEFAULT_UA, Duration::from_secs(30), true).unwrap(),
            api_key: api_key.into(),
            endpoint: endpoint.into(),
        }
    }
}

#[async_trait]
impl SearchBackend for BraveBackend {
    fn name(&self) -> &str {
        "brave"
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, anyhow::Error> {
        let resp = self
            .client
            .get(&self.endpoint)
            .header("X-Subscription-Token", &self.api_key)
            .header(reqwest::header::ACCEPT, "application/json")
            .query(&[("q", query), ("count", &limit.to_string())])
            .send()
            .await
            .map_err(|e| super::sanitize_err("brave", &e))?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!(
                "brave request failed: HTTP {}",
                resp.status().as_u16()
            ));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| super::sanitize_err("brave", &e))?;
        Ok(parse_brave(&body, limit))
    }
}

/// Map a Brave response body to normalized results.
fn parse_brave(body: &Value, limit: usize) -> Vec<SearchResult> {
    body.get("web")
        .and_then(|w| w.get("results"))
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .take(limit)
                .map(|item| {
                    SearchResult::new(
                        str_field(item, "title"),
                        str_field(item, "url"),
                        str_field(item, "description"),
                        None,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn str_field(item: &Value, key: &str) -> String {
    item.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn parses_brave_fixture() {
        let body: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/brave_search.json"
        )))
        .unwrap();
        let results = parse_brave(&body, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Mount Helicon - Wikipedia");
        assert_eq!(
            results[0].url,
            "https://en.wikipedia.org/wiki/Mount_Helicon"
        );
        assert_eq!(
            results[0].snippet,
            "Mount Helicon is a mountain in Boeotia, Greece."
        );
        assert!(results[0].content.is_none());
    }

    #[test]
    fn parse_brave_respects_limit() {
        let body: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/brave_search.json"
        )))
        .unwrap();
        assert_eq!(parse_brave(&body, 1).len(), 1);
    }

    #[tokio::test]
    async fn error_never_leaks_api_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let secret = "brv-SUPER-SECRET-KEY";
        let backend = BraveBackend::with_endpoint(secret, server.uri());
        let err = backend.search("anything", 3).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(!msg.contains(secret), "key leaked in error: {msg}");
    }
}
