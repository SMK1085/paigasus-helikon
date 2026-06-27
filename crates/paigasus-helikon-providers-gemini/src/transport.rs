//! Build per-transport request URLs and auth headers.

use paigasus_helikon_core::ModelError;

use crate::auth::Auth;
use crate::builder::{Config, Transport};

/// Host for the selected transport, honoring `base_url`.
fn host(cfg: &Config) -> String {
    if let Some(b) = &cfg.base_url {
        return b.trim_end_matches('/').to_owned();
    }
    match &cfg.transport {
        Transport::Developer => "https://generativelanguage.googleapis.com".to_owned(),
        Transport::Vertex { location, .. } if location == "global" => {
            "https://aiplatform.googleapis.com".to_owned()
        }
        Transport::Vertex { location, .. } => {
            format!("https://{location}-aiplatform.googleapis.com")
        }
    }
}

/// Streaming endpoint URL (`:streamGenerateContent?alt=sse`).
pub(crate) fn stream_url(cfg: &Config) -> String {
    let host = host(cfg);
    match &cfg.transport {
        Transport::Developer => format!(
            "{host}/v1beta/models/{}:streamGenerateContent?alt=sse",
            cfg.model_id
        ),
        Transport::Vertex { project, location } => format!(
            "{host}/v1/projects/{project}/locations/{location}/publishers/google/models/{}:streamGenerateContent?alt=sse",
            cfg.model_id
        ),
    }
}

/// Auth header for a non-async credential (`ApiKey`/`Bearer`).
pub(crate) fn auth_header(
    auth: &Auth,
) -> Result<(reqwest::header::HeaderName, String), ModelError> {
    use reqwest::header::{HeaderName, AUTHORIZATION};
    match auth {
        Auth::ApiKey(k) => Ok((HeaderName::from_static("x-goog-api-key"), k.clone())),
        Auth::Bearer(b) => Ok((AUTHORIZATION, format!("Bearer {b}"))),
        Auth::Token(_) => Err(ModelError::Other(anyhow::anyhow!(
            "Auth::Token must be resolved before auth_header"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::ModelCapabilities;

    fn dev_cfg() -> Config {
        Config {
            http: reqwest::Client::new(),
            base_url: None,
            model_id: "gemini-2.5-flash".into(),
            transport: Transport::Developer,
            auth: Auth::ApiKey("k".into()),
            capabilities: ModelCapabilities::empty(),
        }
    }
    fn vertex_cfg(loc: &str) -> Config {
        Config {
            http: reqwest::Client::new(),
            base_url: None,
            model_id: "gemini-2.5-pro".into(),
            transport: Transport::Vertex {
                project: "proj".into(),
                location: loc.into(),
            },
            auth: Auth::Bearer("ya29".into()),
            capabilities: ModelCapabilities::empty(),
        }
    }

    #[test]
    fn developer_stream_url() {
        let u = stream_url(&dev_cfg());
        assert_eq!(u, "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse");
    }

    #[test]
    fn vertex_regional_stream_url() {
        let u = stream_url(&vertex_cfg("us-central1"));
        assert_eq!(u, "https://us-central1-aiplatform.googleapis.com/v1/projects/proj/locations/us-central1/publishers/google/models/gemini-2.5-pro:streamGenerateContent?alt=sse");
    }

    #[test]
    fn vertex_global_host() {
        let u = stream_url(&vertex_cfg("global"));
        assert!(
            u.starts_with("https://aiplatform.googleapis.com/v1/projects/proj/locations/global/")
        );
    }

    #[test]
    fn base_url_override_developer() {
        let mut c = dev_cfg();
        c.base_url = Some("http://localhost:8080".into());
        assert_eq!(
            stream_url(&c),
            "http://localhost:8080/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn api_key_header() {
        let (n, v) = auth_header(&Auth::ApiKey("secret".into())).unwrap();
        assert_eq!(n.as_str(), "x-goog-api-key");
        assert_eq!(v, "secret");
    }

    #[test]
    fn bearer_header() {
        let (n, v) = auth_header(&Auth::Bearer("ya29".into())).unwrap();
        assert_eq!(n.as_str(), "authorization");
        assert_eq!(v, "Bearer ya29");
    }
}
