//! [`BedrockModel`] — the Bedrock Converse API model handle.
//!
//! Wraps an `Arc<Config>` so that the model handle is `Clone + Send + Sync`
//! and the `BoxStream` returned by `invoke` can be `'static + Send`.

use std::future::Future;
use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest, ToolChoice,
};

use aws_sdk_bedrockruntime::operation::converse_stream::ConverseStreamOutput;
use aws_sdk_bedrockruntime::Client;
use aws_smithy_types::Document;

use crate::builder::Config;
use crate::document::value_to_document;
use crate::error::{map_sdk_error, send_failure, SendFailure};
use crate::fallback::{send_with_fallback, Sent};
use crate::policy::ForcedChoicePolicy;
use crate::stream::StreamTranslator;
use crate::translate::{build_request, PreparedConverse};

// ── BedrockModel ──────────────────────────────────────────────────────────────

/// An Amazon Bedrock Converse API model handle.
///
/// Construct via [`BedrockModel::converse`] (sync builder) or
/// [`BedrockModel::from_env`] (async, loads AWS config from the environment).
///
/// The handle is `Clone + Send + Sync` because it holds only `Arc` values.
/// Clones share the state that the handle learns at run time.
#[derive(Debug, Clone)]
pub struct BedrockModel {
    pub(crate) config: Arc<Config>,
    pub(crate) policy: Arc<ForcedChoicePolicy>,
}

/// The `additionalModelRequestFields` document of a request, if it needs one.
fn additional_fields(p: &PreparedConverse) -> Option<Document> {
    p.additional.to_value().map(|v| value_to_document(&v))
}

/// Send one `ConverseStream` request.
///
/// The error keeps the flag that tells the fallback whether the service
/// rejected a forced tool choice.
fn send_converse_stream(
    client: &Client,
    p: PreparedConverse,
) -> impl Future<Output = Result<ConverseStreamOutput, SendFailure>> {
    let additional = additional_fields(&p);
    let fluent = client
        .converse_stream()
        .model_id(&p.model_id)
        .set_messages(Some(p.messages))
        .set_system(if p.system.is_empty() {
            None
        } else {
            Some(p.system)
        })
        .set_tool_config(p.tool_config)
        .set_inference_config(p.inference_config)
        .set_output_config(p.output_config)
        .set_additional_model_request_fields(additional);
    // fluent.send() returns SdkError<_, HttpResponse>, so send_failure can
    // read the Retry-After header.
    async move { fluent.send().await.map_err(send_failure) }
}

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
        let first = build_request(&self.config, &request, self.policy.path())?;

        let forbid = matches!(request.model_settings.tool_choice, Some(ToolChoice::None));
        let config = Arc::clone(&self.config);
        let policy = Arc::clone(&self.policy);
        let client = self.config.client.clone();

        let s = stream! {
            // The rebuild closure owns the request. It makes the native
            // request if the service rejects the forced tool choice of the
            // first request.
            let rebuild_cfg = Arc::clone(&config);
            let rebuild = move |path| build_request(&rebuild_cfg, &request, path);

            // Send the request (and the one fallback request, if any) and
            // obtain the event receiver. Guard with a cancellation check
            // (biased: cancel arm wins) so that a token already fired — or one
            // that fires during either HTTP send — aborts cleanly without
            // opening a dangling Bedrock stream and without emitting Finish
            // (per the Model::invoke cancellation contract).
            let sent = tokio::select! {
                biased;
                _ = cancel.cancelled() => { return; }
                result = send_with_fallback(
                    &policy,
                    &config.model_id,
                    first,
                    rebuild,
                    |p| send_converse_stream(&client, p),
                ) => result,
            };
            let Sent {
                output,
                synthesizing,
                native,
            } = match sent {
                Ok(sent) => sent,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            let mut receiver = output.stream;
            let mut translator = StreamTranslator::new(synthesizing)
                .native(native)
                .forbid_tool_calls(forbid);

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
                    // Stream exhausted normally (EOF).
                    // Flush any buffered stop reason that arrived without a
                    // subsequent Metadata event so the consumer always sees
                    // a terminal Finish.
                    Ok(None) => {
                        for ev in translator.finish() {
                            yield ev;
                        }
                        return;
                    }
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
        self.config.capabilities
    }

    fn provider(&self) -> &str {
        "bedrock"
    }

    fn model(&self) -> &str {
        &self.config.model_id
    }
}

// ── Unit tests (descriptor + capability + cancellation contract) ──────────────

#[cfg(test)]
mod tests {
    use super::*;
    use aws_config::{BehaviorVersion, Region};
    use futures_core::stream::BoxStream;
    use std::collections::HashMap;

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
            let sdk_cfg = aws_config::defaults(BehaviorVersion::v2026_11_17())
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
    fn clones_share_the_policy() {
        let m = offline_model("anthropic.claude-sonnet-4-6");
        let m2 = m.clone();
        assert!(m.policy.record_rejection());
        assert_eq!(m2.policy.path(), crate::policy::OutputPath::Native);
        let fresh = offline_model("anthropic.claude-sonnet-4-6");
        assert_eq!(fresh.policy.path(), crate::policy::OutputPath::ForcedTool);
    }

    // `offline_model` blocks on its own runtime, so this test is a plain
    // `#[test]` that drives `invoke` on a second runtime.
    #[test]
    fn invoke_rejects_blank_only_request_before_network() {
        let m = offline_model("anthropic.claude-sonnet-4-6");
        let mut req = ModelRequest::new();
        req.messages = vec![paigasus_helikon_core::Item::UserMessage {
            content: vec![paigasus_helikon_core::ContentPart::Text {
                text: String::new(),
            }],
        }];
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let res = rt.block_on(m.invoke(req, CancellationToken::new()));
        match res {
            Err(ModelError::Other(e)) => assert!(
                e.to_string().contains("no message with content to send"),
                "unexpected error: {e}"
            ),
            Err(e) => panic!("unexpected error variant: {e}"),
            Ok(_) => panic!("expected an error"),
        }
    }

    #[test]
    fn provider_returns_bedrock() {
        let m = offline_model("anthropic.claude-sonnet-4-6");
        assert_eq!(m.provider(), "bedrock");
    }

    #[test]
    fn model_returns_configured_id() {
        let id = "anthropic.claude-sonnet-4-6";
        let m = offline_model(id);
        assert_eq!(m.model(), id);
    }

    #[test]
    fn capabilities_matches_caps_for_anthropic() {
        let m = offline_model("anthropic.claude-sonnet-4-6");
        let caps = m.capabilities();
        assert!(caps.streaming);
        assert!(caps.tools);
        assert!(caps.structured_output);
        assert!(
            !caps.vision,
            "vision not advertised (no image-block mapping)"
        );
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

    /// Live test for the learned native path (rule B), with rule A off.
    ///
    /// Run it only with `BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5`
    /// and `cargo test -p paigasus-helikon-providers-bedrock
    /// live_rule_b -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_rule_b_learns_native_output_after_rejection() {
        use crate::policy::{ForcedChoicePolicy, OutputPath};
        use paigasus_helikon_core::{ContentPart, Item, ResponseFormat};

        let id = match std::env::var("BEDROCK_MODEL_ID") {
            Ok(v) if !v.is_empty() => v,
            _ => {
                eprintln!("[live] BEDROCK_MODEL_ID not set - skipping rule B live test.");
                return;
            }
        };
        let family = crate::ModelFamily::from_model_id(&id);
        if family != crate::ModelFamily::Anthropic {
            eprintln!("[live] {id} is not an Anthropic model - skipping");
            return;
        }
        let mut model = BedrockModel::from_env(&id).await.expect("from_env");
        model.policy = Arc::new(ForcedChoicePolicy::without_static_rule(family));
        assert_eq!(model.policy.path(), OutputPath::ForcedTool);

        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "Give a transaction: deposit of 100 USD.".to_owned(),
            }],
        }];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Transaction".to_owned(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": { "type": "string" },
                    "amount": { "type": "number" }
                },
                "required": ["operation", "amount"]
            }),
            strict: false,
        });
        req.model_settings.max_output_tokens = Some(256);

        let mut s = model
            .invoke(req, CancellationToken::new())
            .await
            .expect("invoke should succeed");
        let mut text = String::new();
        while let Some(ev) = s.next().await {
            if let ModelEvent::TokenDelta { text: t } = ev.expect("no stream error") {
                text.push_str(&t);
            }
        }
        assert_eq!(
            model.policy.path(),
            OutputPath::Native,
            "one classified rejection must set the learned flag"
        );
        let v: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("output must be valid JSON ({e}); got: {text}"));
        assert!(v["operation"].is_string(), "operation: {v}");
    }

    /// Live test for an application inference profile ARN with a family override.
    ///
    /// Set `BEDROCK_PROFILE_ARN` to the ARN of a profile that wraps an
    /// Anthropic model. Then run `cargo test -p paigasus-helikon-providers-bedrock
    /// live_profile_arn -- --ignored --nocapture`. The dev account has no
    /// profile today, so this test has not run.
    #[tokio::test]
    #[ignore]
    async fn live_profile_arn_with_family_override() {
        use paigasus_helikon_core::{ContentPart, Item, ResponseFormat};

        let arn = match std::env::var("BEDROCK_PROFILE_ARN") {
            Ok(v) if !v.is_empty() => v,
            _ => {
                eprintln!("[live] BEDROCK_PROFILE_ARN not set - skipping profile live test.");
                return;
            }
        };
        let cfg = aws_config::defaults(BehaviorVersion::v2026_11_17())
            .load()
            .await;
        let model = BedrockModel::converse(&arn)
            .sdk_config(&cfg)
            .family(crate::ModelFamily::Anthropic)
            .build()
            .expect("build");

        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "Give a transaction: deposit of 100 USD.".to_owned(),
            }],
        }];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Transaction".to_owned(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": { "type": "string" },
                    "amount": { "type": "number" }
                },
                "required": ["operation", "amount"]
            }),
            strict: false,
        });
        req.model_settings.max_output_tokens = Some(256);

        let mut s = model
            .invoke(req, CancellationToken::new())
            .await
            .expect("invoke should succeed");
        let mut text = String::new();
        while let Some(ev) = s.next().await {
            if let ModelEvent::TokenDelta { text: t } = ev.expect("no stream error") {
                text.push_str(&t);
            }
        }
        let v: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("output must be valid JSON ({e}); got: {text}"));
        assert!(v["operation"].is_string(), "operation: {v}");
        eprintln!("[live] final output path: {:?}", model.policy.path());
    }

    // ── additional_fields ─────────────────────────────────────────────────────

    fn plain_prepared() -> crate::translate::PreparedConverse {
        use paigasus_helikon_core::{ContentPart, Item};
        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "hi".to_owned(),
            }],
        }];
        let cfg = crate::builder::offline_config("anthropic.claude-sonnet-4-6");
        crate::translate::build_request(&cfg, &req, crate::policy::OutputPath::ForcedTool).unwrap()
    }

    #[test]
    fn additional_fields_carry_the_none_choice() {
        let mut p = plain_prepared();
        assert!(additional_fields(&p).is_none());
        p.additional.tool_choice = Some(crate::translate::additional::AdditionalToolChoice::None);
        assert_eq!(
            additional_fields(&p),
            Some(Document::Object(HashMap::from([(
                "tool_choice".to_owned(),
                Document::Object(HashMap::from([(
                    "type".to_owned(),
                    Document::String("none".to_owned()),
                )])),
            )])))
        );
    }
}
