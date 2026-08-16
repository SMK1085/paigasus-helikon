//! `LiteLlmModel` — the public [`paigasus_helikon_core::Model`] implementation.

use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

use crate::builder::{BuildError, Config, LiteLlmModelBuilder};
use crate::error::{classify, parse_retry_after_ms};
use crate::sse::StreamChunk;
use crate::stream::ChatTranslator;

/// LiteLLM proxy provider.
///
/// Construct via [`Self::chat`] or [`Self::from_env`]. Always streams.
#[derive(Debug, Clone)]
pub struct LiteLlmModel(pub(crate) Arc<Config>);

impl LiteLlmModel {
    /// Chat Completions builder for a proxy model alias.
    ///
    /// The alias is whatever the proxy operator configured — it need not be an
    /// OpenAI model id.
    pub fn chat(model_id: impl Into<String>) -> LiteLlmModelBuilder {
        LiteLlmModelBuilder::new(model_id)
    }

    /// Build from the ambient environment.
    ///
    /// Reads `LITELLM_API_BASE` (falling back to `LITELLM_PROXY_API_BASE`) and
    /// `LITELLM_API_KEY` (falling back to `LITELLM_PROXY_API_KEY`). The key is
    /// optional — an unset key yields an unauthenticated model, not an error —
    /// so the only failures are [`BuildError::MissingBaseUrl`] and
    /// [`BuildError::InvalidBaseUrl`].
    pub fn from_env(model_id: impl Into<String>) -> Result<Self, BuildError> {
        Self::chat(model_id).build()
    }

    pub(crate) fn from_config(cfg: Config) -> Self {
        Self(Arc::new(cfg))
    }

    #[cfg(test)]
    pub(crate) fn endpoint(&self) -> &str {
        &self.0.endpoint
    }

    #[cfg(test)]
    pub(crate) fn auth(&self) -> Option<&str> {
        self.0.auth.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn config_for_test(&self) -> Config {
        Config {
            http: self.0.http.clone(),
            endpoint: self.0.endpoint.clone(),
            model_id: self.0.model_id.clone(),
            auth: self.0.auth.clone(),
            headers: self.0.headers.clone(),
            capabilities: self.0.capabilities,
            extras: self.0.extras.clone(),
        }
    }
}

#[async_trait]
impl Model for LiteLlmModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let cfg = self.0.clone();
        let body = crate::translate::build_request(&cfg, &request);

        let s = stream! {
            let mut req = cfg.http
                .post(&cfg.endpoint)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::ACCEPT, "text/event-stream");

            if let Some(key) = &cfg.auth {
                req = req.header(reqwest::header::AUTHORIZATION, format!("Bearer {key}"));
            }
            if let Some(n) = cfg.extras.num_retries {
                // Also sent in the body. Upstream documents the header as
                // outranking the body, so the two cannot disagree.
                req = req.header("x-litellm-num-retries", n.to_string());
            }
            // Caller headers last, so `.header()` is a genuine escape hatch.
            for (name, value) in &cfg.headers {
                req = req.header(name.as_str(), value.as_str());
            }

            let send_fut = req.json(&body).send();

            let response = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                r = send_fut => match r {
                    Ok(r) => r,
                    Err(e) => { yield Err(ModelError::Transport(e.to_string())); return; }
                },
            };

            let status = response.status();
            let headers = response.headers().clone();

            // Correlation ids, logged on every response including streaming
            // ones. A routing chokepoint with no call id is not debuggable.
            let call_id = headers
                .get("x-litellm-call-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            tracing::debug!(
                target: "paigasus::litellm::http",
                status = status.as_u16(),
                call_id = %call_id,
                model_id = headers.get("x-litellm-model-id").and_then(|v| v.to_str().ok()).unwrap_or(""),
                attempted_retries = headers.get("x-litellm-attempted-retries").and_then(|v| v.to_str().ok()).unwrap_or(""),
                attempted_fallbacks = headers.get("x-litellm-attempted-fallbacks").and_then(|v| v.to_str().ok()).unwrap_or(""),
                "litellm response"
            );

            // A failing request returns non-2xx JSON, NOT an SSE stream —
            // check before entering the framing loop.
            let is_sse = headers
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.starts_with("text/event-stream"));

            if !status.is_success() || !is_sse {
                let retry_after_ms = parse_retry_after_ms(&headers);
                let bytes = response.bytes().await.unwrap_or_default();
                let err = error_from_body(status.as_u16(), &bytes, retry_after_ms, &call_id);
                yield Err(err);
                return;
            }

            let mut events = response.bytes_stream().eventsource();
            let mut translator = ChatTranslator::new();

            loop {
                let next = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    n = events.next() => n,
                };
                match next {
                    None => {
                        for ev in translator.finish() { yield Ok(ev); }
                        return;
                    }
                    Some(Err(e)) => {
                        yield Err(ModelError::Transport(e.to_string()));
                        return;
                    }
                    Some(Ok(event)) => {
                        if event.data == "[DONE]" {
                            for ev in translator.finish() { yield Ok(ev); }
                            return;
                        }
                        // Defensive: a backend failing mid-generation can emit
                        // an error frame. Unverified — every reproducible
                        // failure returns non-2xx JSON before the stream opens.
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&event.data) {
                            if v.get("error").is_some() {
                                yield Err(error_from_body(
                                    500, event.data.as_bytes(), None, &call_id,
                                ));
                                return;
                            }
                        }
                        let chunk: StreamChunk = match serde_json::from_str(&event.data) {
                            Ok(c) => c,
                            Err(parse_err) => {
                                tracing::warn!(
                                    target: "paigasus::litellm::sse",
                                    %parse_err,
                                    event_len = event.data.len(),
                                    "unparseable SSE event payload; skipping"
                                );
                                continue;
                            }
                        };
                        for ev in translator.consume(chunk) {
                            yield Ok(ev);
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
        "litellm"
    }

    fn model(&self) -> &str {
        &self.0.model_id
    }
}

/// Extract LiteLLM's error envelope and classify it.
fn error_from_body(
    status: u16,
    bytes: &[u8],
    retry_after_ms: Option<u64>,
    call_id: &str,
) -> ModelError {
    let parsed: Option<serde_json::Value> = serde_json::from_slice(bytes).ok();
    let (code, err_type, message) = parsed
        .as_ref()
        .and_then(|v| v.get("error"))
        .map(|e| {
            let as_str = |k: &str| e.get(k).and_then(|x| x.as_str()).map(str::to_owned);
            (
                as_str("code"),
                as_str("type"),
                as_str("message").unwrap_or_default(),
            )
        })
        .unwrap_or_else(|| (None, None, String::from_utf8_lossy(bytes).into_owned()));

    let message = if call_id.is_empty() {
        message
    } else {
        format!("{message} (x-litellm-call-id: {call_id})")
    };

    classify(
        status,
        code.as_deref(),
        err_type.as_deref(),
        &message,
        retry_after_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_and_model_getters() {
        let m = LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .api_key("sk-x")
            .build()
            .unwrap();
        assert_eq!(m.provider(), "litellm");
        assert_eq!(m.model(), "prod-fast");
    }

    #[test]
    fn capabilities_default_to_streaming_and_tools() {
        let m = LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .build()
            .unwrap();
        assert!(m.capabilities().streaming);
        assert!(m.capabilities().tools);
        assert!(!m.capabilities().vision);
    }
}
