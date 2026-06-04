//! SMA-322 — export the agent's GenAI-semconv spans to Langfuse via OTLP.
//!
//! Run:
//!   LANGFUSE_HOST=https://cloud.langfuse.com \
//!   LANGFUSE_PUBLIC_KEY=pk-... LANGFUSE_SECRET_KEY=sk-... \
//!   cargo run -p paigasus-helikon --example langfuse_tracing --features runtime-tokio
//!
//! Manual verification (acceptance criterion): open the run in Langfuse and confirm
//! the trace tree `invoke_agent → agent.turn → chat / execute_tool`, token counts on
//! the `chat` observation, and session/user/tags on the trace.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use futures_core::stream::BoxStream;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{Array, Value};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::{BatchSpanProcessor, SdkTracerProvider, SpanData, SpanExporter};
use tracing_subscriber::prelude::*;

use paigasus_helikon::core::{
    AgentInput, CancellationToken, ConversationSnapshot, FinishReason, HookRegistry, Instructions,
    LlmAgent, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest, ModelSettings,
    RunConfig, RunContext, Runner, SequenceId, Session, SessionError, SessionEvent, TracerHandle,
};
use paigasus_helikon::runtime_tokio::TokioRunner;

/// Wraps an OTLP exporter and rewrites the `langfuse.trace.tags` JSON-string
/// attribute into a native `string[]` before export. Keeps `core` `tracing`-only.
#[derive(Debug)]
struct TagsArrayExporter<E: SpanExporter>(E);

impl<E: SpanExporter> SpanExporter for TagsArrayExporter<E> {
    fn export(
        &self,
        mut batch: Vec<SpanData>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        for span in &mut batch {
            if let Some(kv) = span
                .attributes
                .iter_mut()
                .find(|kv| kv.key.as_str() == "langfuse.trace.tags")
            {
                if let Value::String(s) = &kv.value {
                    if let Ok(tags) = serde_json::from_str::<Vec<String>>(s.as_str()) {
                        let arr: Vec<opentelemetry::StringValue> =
                            tags.into_iter().map(Into::into).collect();
                        kv.value = Value::Array(Array::String(arr));
                    }
                }
            }
        }
        self.0.export(batch)
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.0.shutdown()
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }

    fn set_resource(&mut self, resource: &opentelemetry_sdk::Resource) {
        self.0.set_resource(resource);
    }
}

/// A scripted model so the example needs no LLM API key.
struct DemoModel;

#[async_trait]
impl Model for DemoModel {
    async fn invoke(
        &self,
        _req: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let events = vec![
            Ok(ModelEvent::TokenDelta {
                text: "hello from helikon".into(),
            }),
            Ok(ModelEvent::Usage {
                input_tokens: 128,
                output_tokens: 16,
                cached_input_tokens: None,
                reasoning_tokens: None,
            }),
            Ok(ModelEvent::Finish {
                reason: FinishReason::Stop,
            }),
        ];
        Ok(Box::pin(futures_util::stream::iter(events)))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    fn provider(&self) -> &str {
        "demo"
    }

    fn model(&self) -> &str {
        "demo-1"
    }
}

struct NoopSession;

#[async_trait]
impl Session for NoopSession {
    async fn append(&self, _: &[SessionEvent]) -> Result<(), SessionError> {
        Ok(())
    }

    async fn events(&self, _: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        Ok(Vec::new())
    }

    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        Ok(ConversationSnapshot::default())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let host =
        std::env::var("LANGFUSE_HOST").unwrap_or_else(|_| "https://cloud.langfuse.com".into());
    let pk = std::env::var("LANGFUSE_PUBLIC_KEY")?;
    let sk = std::env::var("LANGFUSE_SECRET_KEY")?;
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("{pk}:{sk}"));

    let otlp = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(format!("{host}/api/public/otel/v1/traces"))
        .with_headers(std::collections::HashMap::from([(
            "Authorization".to_string(),
            format!("Basic {auth}"),
        )]))
        .build()?;

    let provider = SdkTracerProvider::builder()
        .with_span_processor(BatchSpanProcessor::builder(TagsArrayExporter(otlp)).build())
        .build();
    let tracer = provider.tracer("paigasus-helikon");
    tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .init();

    let agent: LlmAgent<(), DemoModel> = LlmAgent {
        name: "assistant".into(),
        description: "demo".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model: Arc::new(DemoModel),
        tools: Vec::new(),
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    };

    let ctx = RunContext::new(
        Arc::new(()),
        Arc::new(NoopSession) as Arc<dyn Session>,
        HookRegistry::<()>::new(),
        TracerHandle::builder()
            .with_session_id("demo-session")
            .with_user_id("demo-user")
            .with_tag("example")
            .with_tag("sma-322")
            .build(),
        CancellationToken::new(),
    );

    let result = TokioRunner
        .run(
            &agent,
            ctx,
            AgentInput::from_user_text("hi"),
            RunConfig::default(),
        )
        .await?;
    println!("final output: {}", result.final_output);

    let _ = provider.force_flush();
    let _ = provider.shutdown();
    Ok(())
}
