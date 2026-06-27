//! `GeminiModel` — public [`paigasus_helikon_core::Model`] implementation.

use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

use crate::auth::Auth;
use crate::builder::{BuildError, Config, GeminiModelBuilder, Transport};
use crate::error::{classify, parse_retry_after_ms};
use crate::sse::GeminiChunk;
use crate::stream::StreamTranslator;
use crate::transport::{auth_header, stream_url};

/// Google Gemini provider (Developer API + Vertex).
#[derive(Debug, Clone)]
pub struct GeminiModel(pub(crate) Arc<Config>);

impl GeminiModel {
    /// Developer-API builder (API key).
    pub fn developer(model_id: impl Into<String>) -> GeminiModelBuilder {
        GeminiModelBuilder::new(model_id, Transport::Developer)
    }
    /// Vertex-AI builder (project + location + bearer/token-provider).
    pub fn vertex(
        model_id: impl Into<String>,
        project: impl Into<String>,
        location: impl Into<String>,
    ) -> GeminiModelBuilder {
        GeminiModelBuilder::new(
            model_id,
            Transport::Vertex {
                project: project.into(),
                location: location.into(),
            },
        )
    }
    /// Developer API from `GEMINI_API_KEY` (fallback `GOOGLE_API_KEY`).
    pub fn from_env(model_id: impl Into<String>) -> Result<Self, BuildError> {
        let key = std::env::var("GEMINI_API_KEY")
            .or_else(|_| std::env::var("GOOGLE_API_KEY"))
            .map_err(|_| BuildError::MissingApiKey)?;
        Self::developer(model_id).api_key(key).build()
    }

    /// Vertex AI from the ambient environment, authenticating via Application
    /// Default Credentials (ADC).
    ///
    /// Reads `GOOGLE_CLOUD_PROJECT` (required) and `GOOGLE_CLOUD_LOCATION`
    /// (defaults to `global`), then discovers ADC credentials through
    /// [`crate::AdcTokenProvider`]. Enabled by the `vertex-adc` cargo feature.
    #[cfg(feature = "vertex-adc")]
    pub async fn vertex_from_env(model_id: impl Into<String>) -> Result<Self, BuildError> {
        let project =
            std::env::var("GOOGLE_CLOUD_PROJECT").map_err(|_| BuildError::MissingVertexProject)?;
        let location =
            std::env::var("GOOGLE_CLOUD_LOCATION").unwrap_or_else(|_| "global".to_owned());
        let provider = crate::auth::AdcTokenProvider::from_env()
            .await
            .map_err(|e| BuildError::Adc(e.to_string()))?;
        Self::vertex(model_id, project, location)
            .token_provider(provider)
            .build()
    }

    pub(crate) fn from_config(cfg: Config) -> Self {
        Self(Arc::new(cfg))
    }
}

#[async_trait]
impl Model for GeminiModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let cfg = self.0.clone();
        let prepared = crate::translate::build_request(&cfg, &request)?;
        let url = stream_url(&cfg);

        // Resolve the auth header up-front (async for Auth::Token), inside the
        // caller's await so a token-fetch failure returns Err from invoke.
        let (header_name, header_value) = match &cfg.auth {
            Auth::Token(p) => {
                let tok = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(ModelError::Unavailable),
                    t = p.token() => t?,
                };
                (reqwest::header::AUTHORIZATION, format!("Bearer {tok}"))
            }
            other => auth_header(other)?,
        };

        let client = cfg.http.clone();
        let body = prepared.body;

        let s = stream! {
            let send_fut = client
                .post(&url)
                .header(header_name, header_value)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .json(&body)
                .send();

            let response = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                r = send_fut => match r {
                    Ok(r) => r,
                    Err(e) => { yield Err(ModelError::Transport(e.to_string())); return; }
                },
            };

            let status = response.status();
            if !status.is_success() {
                let retry_after_ms = parse_retry_after_ms(response.headers());
                let bytes = response.bytes().await.unwrap_or_default();
                let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&bytes);
                let (sfield, message) = parsed
                    .as_ref()
                    .ok()
                    .map(|v| {
                        let s = v.get("error").and_then(|e| e.get("status")).and_then(|t| t.as_str()).unwrap_or("").to_owned();
                        let m = v.get("error").and_then(|e| e.get("message")).and_then(|t| t.as_str()).unwrap_or("").to_owned();
                        (s, m)
                    })
                    .unwrap_or_else(|| (String::new(), String::from_utf8_lossy(&bytes).into_owned()));
                yield Err(classify(status.as_u16(), Some(&sfield).filter(|s| !s.is_empty()).map(|s| s.as_str()), &message, retry_after_ms));
                return;
            }

            let mut event_stream = response.bytes_stream().eventsource();
            let mut translator = StreamTranslator::new();
            loop {
                let next = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    n = event_stream.next() => n,
                };
                match next {
                    None => {
                        for ev in translator.finish() { yield ev; }
                        return;
                    }
                    Some(Err(e)) => { yield Err(ModelError::Transport(e.to_string())); return; }
                    Some(Ok(event)) => {
                        if event.data == "[DONE]" {
                            for ev in translator.finish() { yield ev; }
                            return;
                        }
                        let chunk: GeminiChunk = match serde_json::from_str(&event.data) {
                            Ok(c) => c,
                            Err(parse_err) => {
                                tracing::warn!(
                                    target: "paigasus::gemini::sse",
                                    %parse_err, event_len = event.data.len(),
                                    "unparseable SSE event payload"
                                );
                                continue;
                            }
                        };
                        for ev in translator.consume(chunk) {
                            let is_err = ev.is_err();
                            yield ev;
                            if is_err { return; }
                        }
                    }
                }
            }
        };
        Ok(Box::pin(s))
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.0.capabilities
    }
    fn provider(&self) -> &str {
        "gemini"
    }
    fn model(&self) -> &str {
        &self.0.model_id
    }
}

impl GeminiModelBuilder {
    /// Validate inputs and materialize the [`GeminiModel`].
    pub fn build(self) -> Result<GeminiModel, BuildError> {
        Ok(GeminiModel::from_config(self.build_config()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::Model;

    #[test]
    fn getters() {
        let m = GeminiModel::developer("gemini-2.5-flash")
            .api_key("k")
            .build()
            .unwrap();
        assert_eq!(m.provider(), "gemini");
        assert_eq!(m.model(), "gemini-2.5-flash");
        assert!(m.capabilities().streaming);
    }
}
