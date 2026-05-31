//! SMA-322: asserts the agent loop emits the GenAI-semconv span tree.
//! Uses a real tracing-opentelemetry layer + an in-memory OTel exporter.

use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::Value;
use opentelemetry_sdk::testing::trace::InMemorySpanExporter;
use opentelemetry_sdk::trace::{SimpleSpanProcessor, TracerProvider};
use tracing_subscriber::prelude::*;

use paigasus_helikon::core::{
    Agent, AgentInput, CancellationToken, ConversationSnapshot, FinishReason, HookRegistry,
    Instructions, LlmAgent, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
    ModelSettings, RunConfig, RunContext, RunResultStreaming, SequenceId, Session, SessionError,
    SessionEvent, Tool, ToolContext, ToolError, ToolOutput, TracerHandle,
};

struct ScriptedModel {
    scripts: std::sync::Mutex<std::collections::VecDeque<Vec<ModelEvent>>>,
}
impl ScriptedModel {
    fn new(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: std::sync::Mutex::new(scripts.into()),
        })
    }
}
#[async_trait]
impl Model for ScriptedModel {
    async fn invoke(
        &self,
        _req: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let s = self.scripts.lock().unwrap().pop_front().unwrap_or_default();
        Ok(Box::pin(futures_util::stream::iter(s.into_iter().map(Ok))))
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    fn provider(&self) -> &str {
        "openai"
    }
    fn model(&self) -> &str {
        "gpt-4o"
    }
}

struct EchoTool;
#[async_trait]
impl Tool<()> for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "echo tool"
    }
    fn schema(&self) -> &serde_json::Value {
        static S: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
        S.get_or_init(|| serde_json::json!({"type": "object"}))
    }
    async fn invoke(
        &self,
        _c: &ToolContext<()>,
        _a: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(serde_json::json!("ok")))
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

fn attr<'a>(span: &'a opentelemetry_sdk::export::trace::SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| &kv.value)
}

#[tokio::test(flavor = "current_thread")]
async fn emits_genai_semconv_span_tree() {
    let exporter = InMemorySpanExporter::default();
    let provider = TracerProvider::builder()
        .with_span_processor(SimpleSpanProcessor::new(Box::new(exporter.clone())))
        .build();
    let tracer = provider.tracer("otel_spans_test");
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));

    {
        let _guard = tracing::subscriber::set_default(subscriber);

        let model = ScriptedModel::new(vec![
            vec![
                ModelEvent::ToolCallDelta {
                    call_id: "1".into(),
                    name: Some("echo".into()),
                    args_delta: "{}".into(),
                },
                ModelEvent::Usage {
                    input_tokens: 11,
                    output_tokens: 22,
                    cached_input_tokens: None,
                    reasoning_tokens: None,
                },
                ModelEvent::Finish {
                    reason: FinishReason::ToolCalls,
                },
            ],
            vec![
                ModelEvent::TokenDelta {
                    text: "done".into(),
                },
                ModelEvent::Usage {
                    input_tokens: 33,
                    output_tokens: 44,
                    cached_input_tokens: None,
                    reasoning_tokens: None,
                },
                ModelEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ],
        ]);

        let agent: LlmAgent<(), ScriptedModel> = LlmAgent {
            name: "assistant".into(),
            description: "test".into(),
            instructions: Arc::new("") as Arc<dyn Instructions<()>>,
            model,
            tools: vec![Arc::new(EchoTool) as Arc<dyn Tool<()>>],
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
                .with_session_id("sess-1")
                .with_user_id("user-1")
                .with_tag("prod")
                .build(),
            CancellationToken::new(),
        );

        let stream = agent
            .run(ctx, AgentInput::from_user_text("hi"))
            .await
            .unwrap();
        let _ = RunResultStreaming::new(stream).collect().await.unwrap();
    }

    provider.force_flush();
    let spans = exporter.get_finished_spans().unwrap();
    let names: Vec<&str> = spans.iter().map(|s| s.name.as_ref()).collect();

    let run = spans
        .iter()
        .find(|s| s.name.starts_with("invoke_agent"))
        .expect("run span");
    assert_eq!(run.name.as_ref(), "invoke_agent assistant");
    assert_eq!(
        attr(run, "gen_ai.operation.name"),
        Some(&Value::from("invoke_agent"))
    );
    assert_eq!(
        attr(run, "gen_ai.agent.name"),
        Some(&Value::from("assistant"))
    );
    assert_eq!(
        attr(run, "langfuse.session.id"),
        Some(&Value::from("sess-1"))
    );
    assert_eq!(attr(run, "langfuse.user.id"), Some(&Value::from("user-1")));
    assert_eq!(
        attr(run, "langfuse.trace.tags"),
        Some(&Value::from("[\"prod\"]"))
    );
    assert_eq!(
        attr(run, "gen_ai.usage.input_tokens"),
        Some(&Value::from(44_i64))
    );
    assert_eq!(
        attr(run, "gen_ai.usage.output_tokens"),
        Some(&Value::from(66_i64))
    );

    let chats: Vec<_> = spans
        .iter()
        .filter(|s| s.name.starts_with("chat "))
        .collect();
    assert_eq!(chats.len(), 2, "names were {names:?}");
    for c in &chats {
        assert_eq!(c.name.as_ref(), "chat gpt-4o");
        assert_eq!(attr(c, "gen_ai.operation.name"), Some(&Value::from("chat")));
        assert_eq!(
            attr(c, "gen_ai.provider.name"),
            Some(&Value::from("openai"))
        );
        assert_eq!(
            attr(c, "gen_ai.request.model"),
            Some(&Value::from("gpt-4o"))
        );
    }
    assert!(chats.iter().any(|c| attr(c, "gen_ai.usage.input_tokens")
        == Some(&Value::from(11_i64))
        && attr(c, "gen_ai.usage.output_tokens") == Some(&Value::from(22_i64))));

    let tool = spans
        .iter()
        .find(|s| s.name.starts_with("execute_tool"))
        .expect("tool span");
    assert_eq!(tool.name.as_ref(), "execute_tool echo");
    assert_eq!(
        attr(tool, "gen_ai.operation.name"),
        Some(&Value::from("execute_tool"))
    );
    assert_eq!(attr(tool, "gen_ai.tool.name"), Some(&Value::from("echo")));

    assert!(
        spans.iter().any(|s| s.name.as_ref() == "agent.turn"),
        "names were {names:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn run_span_usage_is_last_seen_not_summed_within_a_turn() {
    let exporter = InMemorySpanExporter::default();
    let provider = TracerProvider::builder()
        .with_span_processor(SimpleSpanProcessor::new(Box::new(exporter.clone())))
        .build();
    let tracer = provider.tracer("otel_spans_incremental_usage");
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
    {
        let _guard = tracing::subscriber::set_default(subscriber);
        // One turn; the model emits TWO incremental Usage events (Anthropic-style).
        // Per the Model contract, consumers retain the LAST Usage seen, not the sum.
        let model = ScriptedModel::new(vec![vec![
            ModelEvent::TokenDelta { text: "hi".into() },
            ModelEvent::Usage {
                input_tokens: 5,
                output_tokens: 5,
                cached_input_tokens: None,
                reasoning_tokens: None,
            },
            ModelEvent::Usage {
                input_tokens: 11,
                output_tokens: 22,
                cached_input_tokens: None,
                reasoning_tokens: None,
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ]]);
        let agent: LlmAgent<(), ScriptedModel> = LlmAgent {
            name: "assistant".into(),
            description: "test".into(),
            instructions: Arc::new("") as Arc<dyn Instructions<()>>,
            model,
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
            TracerHandle::default(),
            CancellationToken::new(),
        );
        let stream = agent
            .run(ctx, AgentInput::from_user_text("hi"))
            .await
            .unwrap();
        let _ = RunResultStreaming::new(stream).collect().await.unwrap();
    }
    provider.force_flush();
    let spans = exporter.get_finished_spans().unwrap();
    let run = spans
        .iter()
        .find(|s| s.name.starts_with("invoke_agent"))
        .expect("run span");
    // Must be the LAST snapshot 11/22, NOT the sum 16/27.
    assert_eq!(
        attr(run, "gen_ai.usage.input_tokens"),
        Some(&Value::from(11_i64))
    );
    assert_eq!(
        attr(run, "gen_ai.usage.output_tokens"),
        Some(&Value::from(22_i64))
    );
    let chat = spans
        .iter()
        .find(|s| s.name.starts_with("chat "))
        .expect("chat span");
    assert_eq!(
        attr(chat, "gen_ai.usage.input_tokens"),
        Some(&Value::from(11_i64))
    );
    assert_eq!(
        attr(chat, "gen_ai.usage.output_tokens"),
        Some(&Value::from(22_i64))
    );
}
