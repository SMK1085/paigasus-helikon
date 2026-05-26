//! `AnthropicModel` — public [`Model`] implementation.

use async_stream::stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

use crate::builder::{AnthropicModelBuilder, Config};
use crate::error::{map_error_type, parse_retry_after_ms};
use crate::http::{build_headers, messages_url};
use crate::sse::AnthropicEvent;
use crate::stream::MessageTranslator;
use crate::translate::build_body;

/// Anthropic provider — Messages API.
///
/// Construct via [`Self::messages`].
#[derive(Debug)]
pub struct AnthropicModel {
    pub(crate) cfg: Config,
}

impl AnthropicModel {
    /// Construct a Messages-API model builder.
    pub fn messages(model_id: impl Into<String>) -> AnthropicModelBuilder {
        AnthropicModelBuilder::new(model_id)
    }

    pub(crate) fn from_config(cfg: Config) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl Model for AnthropicModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let prepared = build_body(&self.cfg, &request)?;
        let synthesizing = prepared.synthesizing_output;
        let headers = build_headers(&self.cfg);
        let url = messages_url(&self.cfg);
        let client = self.cfg.http.clone();

        let s = stream! {
            let send_fut = client
                .post(&url)
                .headers(headers)
                .json(&prepared.body)
                .send();

            let response = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                r = send_fut => match r {
                    Ok(r) => r,
                    Err(e) => {
                        yield Err(ModelError::Transport(e.to_string()));
                        return;
                    }
                },
            };

            let status = response.status();
            if !status.is_success() {
                let retry_after_ms = parse_retry_after_ms(response.headers());
                let body_bytes = response.bytes().await.unwrap_or_default();
                let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&body_bytes);
                let (ty, message) = parsed
                    .as_ref()
                    .ok()
                    .map(|v| {
                        let ty = v
                            .get("error")
                            .and_then(|e| e.get("type"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_owned();
                        let msg = v
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("")
                            .to_owned();
                        (ty, msg)
                    })
                    .unwrap_or_else(|| {
                        (
                            String::new(),
                            String::from_utf8_lossy(&body_bytes).into_owned(),
                        )
                    });
                yield Err(map_error_type(Some(status.as_u16()), &ty, &message, retry_after_ms));
                return;
            }

            let mut event_stream = response.bytes_stream().eventsource();
            let mut translator = MessageTranslator::new(synthesizing);

            loop {
                let next = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    n = event_stream.next() => n,
                };
                match next {
                    None => return,
                    Some(Err(e)) => {
                        yield Err(ModelError::Transport(e.to_string()));
                        return;
                    }
                    Some(Ok(event)) => {
                        let parsed: Result<AnthropicEvent, _> = serde_json::from_str(&event.data);
                        let Ok(parsed) = parsed else {
                            tracing::warn!(
                                target: "paigasus::anthropic::sse",
                                "unparseable SSE event: {}", event.data,
                            );
                            continue;
                        };
                        match translator.consume(parsed) {
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
                            Ok(events) => {
                                for ev in events {
                                    yield ev;
                                }
                            }
                        }
                    }
                }
            }
        };

        Ok(Box::pin(s))
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.cfg.capabilities
    }
}

impl AnthropicModelBuilder {
    /// Resolve auth, validate inputs, materialize the [`AnthropicModel`].
    pub fn build(self) -> Result<AnthropicModel, crate::BuildError> {
        Ok(AnthropicModel::from_config(self.build_config()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_reflects_builder_lookup() {
        let m = AnthropicModel::messages("claude-sonnet-4-6")
            .api_key("sk-test")
            .build()
            .unwrap();
        let c = m.capabilities();
        assert!(c.streaming);
        assert!(c.tools);
        assert!(c.prompt_caching);
    }
}
