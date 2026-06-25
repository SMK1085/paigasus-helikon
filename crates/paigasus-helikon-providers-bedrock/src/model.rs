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

// ── Unit tests (descriptor + capability + cancellation contract) ──────────────

#[cfg(test)]
mod tests {
    use super::*;
    use aws_config::{BehaviorVersion, Region};
    use futures_core::stream::BoxStream;

    // ── Cancellation-wrapper helper (test-only) ───────────────────────────────

    /// Wrap any `Stream<Item = Result<ModelEvent, ModelError>>` with a
    /// `CancellationToken` guard.
    ///
    /// When `cancel` fires, the returned stream ends immediately **without**
    /// emitting a `Finish` event — matching the `Model::invoke` cancellation
    /// contract.
    fn drive_stream_with_token<S>(
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

    // ── Cancellation contract tests ───────────────────────────────────────────

    use futures_core::Stream;
    use futures_util::StreamExt;

    /// Build a stream that yields `n` `TokenDelta` events then ends.
    fn token_stream(
        n: usize,
    ) -> impl Stream<Item = Result<ModelEvent, ModelError>> + Send + 'static {
        let events: Vec<Result<ModelEvent, ModelError>> = (0..n)
            .map(|i| {
                Ok(ModelEvent::TokenDelta {
                    text: format!("chunk-{i}"),
                })
            })
            .collect();
        futures_util::stream::iter(events)
    }

    /// Build a stream that yields `n` `TokenDelta` events then a `Finish` event.
    fn token_stream_with_finish(
        n: usize,
    ) -> impl Stream<Item = Result<ModelEvent, ModelError>> + Send + 'static {
        use paigasus_helikon_core::FinishReason;
        let mut events: Vec<Result<ModelEvent, ModelError>> = (0..n)
            .map(|i| {
                Ok(ModelEvent::TokenDelta {
                    text: format!("chunk-{i}"),
                })
            })
            .collect();
        events.push(Ok(ModelEvent::Finish {
            reason: FinishReason::Stop,
        }));
        futures_util::stream::iter(events)
    }

    #[tokio::test]
    async fn cancel_before_stream_ends_no_finish() {
        let cancel = CancellationToken::new();
        let source = token_stream(10);

        // Cancel immediately — the stream should yield 0 events and no Finish.
        cancel.cancel();
        let events: Vec<_> = drive_stream_with_token(source, cancel).collect().await;

        let has_finish = events
            .iter()
            .any(|r| matches!(r, Ok(ModelEvent::Finish { .. })));
        assert!(!has_finish, "cancelled stream must not emit Finish");
    }

    #[tokio::test]
    async fn cancel_mid_stream_no_finish() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        // A stream that cancels itself after yielding the first token.
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_clone = cancelled.clone();

        let source = async_stream::stream! {
            yield Ok::<ModelEvent, ModelError>(ModelEvent::TokenDelta { text: "first".to_owned() });
            // Signal cancellation after the first token.
            if !cancelled_clone.swap(true, Ordering::SeqCst) {
                cancel_clone.cancel();
            }
            // `biased;` in drive_stream_with_token's tokio::select! checks the
            // cancel arm first on every iteration, so once cancel() fires the
            // driver exits before polling the source again — making this
            // single-threaded ordering deterministic and guaranteeing no Finish
            // is emitted after cancel.
            yield Ok(ModelEvent::TokenDelta { text: "second".to_owned() });
            yield Ok(ModelEvent::Finish { reason: paigasus_helikon_core::FinishReason::Stop });
        };

        let events: Vec<_> = drive_stream_with_token(source, cancel).collect().await;

        let has_finish = events
            .iter()
            .any(|r| matches!(r, Ok(ModelEvent::Finish { .. })));
        assert!(!has_finish, "mid-stream cancel must not emit Finish");
    }

    #[tokio::test]
    async fn uncancelled_stream_emits_finish() {
        let cancel = CancellationToken::new();
        let source = token_stream_with_finish(3);

        let events: Vec<_> = drive_stream_with_token(source, cancel).collect().await;

        let has_finish = events
            .iter()
            .any(|r| matches!(r, Ok(ModelEvent::Finish { .. })));
        assert!(has_finish, "uncancelled stream must emit Finish");
        // All 3 token deltas + finish = 4 events.
        assert_eq!(events.len(), 4, "expected 3 tokens + 1 finish");
    }

    #[tokio::test]
    async fn cancel_does_not_drop_events_already_yielded() {
        let cancel = CancellationToken::new();
        // 5 events, no finish.  Don't cancel — all should arrive.
        let source = token_stream(5);
        let events: Vec<_> = drive_stream_with_token(source, cancel).collect().await;
        assert_eq!(events.len(), 5, "all events must arrive when not cancelled");
    }
}
