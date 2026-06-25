//! [`BedrockModel`] — the Bedrock Converse API model handle.
//!
//! Wraps an `Arc<Config>` so that the model handle is `Clone + Send + Sync`
//! and the `BoxStream` returned by `invoke` can be `'static + Send`.

use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

use crate::builder::Config;
use crate::error::map_sdk_error;
use crate::stream::StreamTranslator;
use crate::translate::build_request;

// ── BedrockModel ──────────────────────────────────────────────────────────────

/// An Amazon Bedrock Converse API model handle.
///
/// Construct via [`BedrockModel::converse`] (sync builder) or
/// [`BedrockModel::from_env`] (async, loads AWS config from the environment).
///
/// The handle is `Clone + Send + Sync` because it holds only an `Arc<Config>`.
#[derive(Debug, Clone)]
pub struct BedrockModel(pub(crate) Arc<Config>);

// ── Model impl ────────────────────────────────────────────────────────────────

#[async_trait]
impl Model for BedrockModel {
    /// Stream model events from Amazon Bedrock's Converse API.
    ///
    /// The returned stream is `'static + Send`.
    ///
    /// **Cancellation contract:** when `cancel` fires mid-stream, the stream
    /// ends immediately without emitting a final `Finish` event (per the core
    /// `Model` trait contract).
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        // Translate the core request into Bedrock wire format (early error on
        // translation failure — before we open any network connection).
        let prepared = build_request(&self.0, &request)?;

        let synthesizing = prepared.synthesizing;
        let client = self.0.client.clone();

        // Build the fluent ConverseStream request from the prepared parts.
        let fluent = client
            .converse_stream()
            .model_id(&prepared.model_id)
            .set_messages(Some(prepared.messages))
            .set_system(if prepared.system.is_empty() {
                None
            } else {
                Some(prepared.system)
            })
            .set_tool_config(prepared.tool_config)
            .set_inference_config(prepared.inference_config);

        let s = stream! {
            // Send the ConverseStream request and obtain the event receiver.
            let output = match fluent.send().await {
                Ok(o) => o,
                Err(e) => {
                    yield Err(map_sdk_error(e));
                    return;
                }
            };

            let mut receiver = output.stream;
            let mut translator = StreamTranslator::new(synthesizing);

            loop {
                let next = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        // Cancellation: end stream without emitting Finish.
                        return;
                    }
                    r = receiver.recv() => r,
                };

                match next {
                    // Stream exhausted normally.
                    Ok(None) => return,
                    // Good event — feed through translator and yield results.
                    Ok(Some(event)) => {
                        for result in translator.consume(event) {
                            yield result;
                        }
                    }
                    // Transport-level error on the event stream.
                    Err(e) => {
                        yield Err(map_sdk_error(e));
                        return;
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
        "bedrock"
    }

    fn model(&self) -> &str {
        &self.0.model_id
    }
}

// ── Cancellation-wrapper helper (extracted for tests) ────────────────────────

/// Wrap any `Stream<Item = Result<ModelEvent, ModelError>>` with a
/// `CancellationToken` guard.
///
/// When `cancel` fires, the returned stream ends immediately **without**
/// emitting a `Finish` event — matching the `Model::invoke` cancellation
/// contract.
///
/// This function is `pub(crate)` and re-exported via `crate::testing` so that
/// `tests/cancellation.rs` can exercise the contract without a live AWS
/// endpoint.
pub fn drive_stream_with_token<S>(
    source: S,
    cancel: CancellationToken,
) -> BoxStream<'static, Result<ModelEvent, ModelError>>
where
    S: futures_core::Stream<Item = Result<ModelEvent, ModelError>> + Send + 'static,
{
    Box::pin(stream! {
        tokio::pin!(source);
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                r = futures_util::StreamExt::next(&mut source) => r,
            };
            match next {
                None => return,
                Some(v) => yield v,
            }
        }
    })
}

// ── Unit tests (descriptor + capability) ─────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use aws_config::{BehaviorVersion, Region};

    fn offline_model(model_id: &str) -> BedrockModel {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async {
            let sdk_cfg = aws_config::defaults(BehaviorVersion::v2026_01_12())
                .region(Region::new("us-east-1"))
                .test_credentials()
                .load()
                .await;
            BedrockModel::converse(model_id)
                .sdk_config(&sdk_cfg)
                .build()
                .expect("build offline model")
        })
    }

    #[test]
    fn provider_returns_bedrock() {
        let m = offline_model("anthropic.claude-3-5-sonnet-20241022-v2:0");
        assert_eq!(m.provider(), "bedrock");
    }

    #[test]
    fn model_returns_configured_id() {
        let id = "anthropic.claude-3-5-sonnet-20241022-v2:0";
        let m = offline_model(id);
        assert_eq!(m.model(), id);
    }

    #[test]
    fn capabilities_matches_caps_for_anthropic() {
        let m = offline_model("anthropic.claude-3-5-sonnet-20241022-v2:0");
        let caps = m.capabilities();
        assert!(caps.streaming);
        assert!(caps.tools);
        assert!(caps.structured_output);
        assert!(caps.vision);
    }

    #[test]
    fn model_is_clone() {
        let m = offline_model("amazon.nova-pro-v1:0");
        let m2 = m.clone();
        assert_eq!(m.model(), m2.model());
    }
}
