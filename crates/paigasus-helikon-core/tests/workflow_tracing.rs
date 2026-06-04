//! SMA-325 — span-capture tests: workflow agents emit an `invoke_agent`
//! tracing span with the expected fields, sub-agent spans **nest** under it,
//! Langfuse trace attributes propagate from the `RunContext` tracer, and a
//! failing run records `otel.status_code = "ERROR"`.
//!
//! These exercise the real `tracing` machinery via a custom capture `Layer`,
//! not a mock — they assert on the spans the agents actually emit.

#[path = "common/mod.rs"]
mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use common::{msg_and_complete, MockAgent};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, CancellationToken, HookRegistry, MemorySession,
    RunContext, RunResultStreaming, SequentialAgent, Session, TracerHandle,
};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::Subscriber;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

// ── Capture layer ────────────────────────────────────────────────────────────

/// One span observed by the [`SpanCapture`] layer.
#[derive(Debug, Clone)]
struct CapturedSpan {
    id: u64,
    /// Contextual parent span id (the span entered when this one was created).
    parent: Option<u64>,
    name: String,
    fields: HashMap<String, String>,
}

/// A `tracing` layer that records every span (id, parent, name, fields) into a
/// shared buffer, including fields recorded after creation via `Span::record`.
#[derive(Clone, Default)]
struct SpanCapture(Arc<Mutex<Vec<CapturedSpan>>>);

impl SpanCapture {
    fn spans(&self) -> Vec<CapturedSpan> {
        self.0.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct FieldVisitor(HashMap<String, String>);

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // `%`/`?` values arrive here; strip the surrounding quotes a `Debug` of a
        // string adds so assertions can compare against the bare value.
        let rendered = format!("{value:?}");
        let rendered = rendered.trim_matches('"').to_owned();
        self.0.insert(field.name().to_owned(), rendered);
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().to_owned(), value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_owned(), value.to_string());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_owned(), value.to_string());
    }
}

impl<S> Layer<S> for SpanCapture
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);
        // Explicit parent if set, else the currently-entered (contextual) span.
        let parent = attrs
            .parent()
            .cloned()
            .or_else(|| ctx.current_span().id().cloned())
            .map(|i| i.into_u64());
        self.0.lock().unwrap().push(CapturedSpan {
            id: id.into_u64(),
            parent,
            name: attrs.metadata().name().to_owned(),
            fields: visitor.0,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        values.record(&mut visitor);
        let mut guard = self.0.lock().unwrap();
        if let Some(s) = guard.iter_mut().find(|s| s.id == id.into_u64()) {
            s.fields.extend(visitor.0);
        }
    }
}

// ── Harness ──────────────────────────────────────────────────────────────────

fn ctx() -> RunContext<()> {
    ctx_with_tracer(TracerHandle::default())
}

fn ctx_with_tracer(tracer: TracerHandle) -> RunContext<()> {
    RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()) as Arc<dyn Session>,
        HookRegistry::new(),
        tracer,
        CancellationToken::new(),
    )
}

/// Drive `fut` to completion under a [`SpanCapture`] subscriber on a single
/// thread (so the thread-local default subscriber is active for every poll) and
/// return the captured spans.
fn run_under_capture<Fut>(fut: impl FnOnce() -> Fut) -> Vec<CapturedSpan>
where
    Fut: std::future::Future<Output = ()>,
{
    let capture = SpanCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    tracing::subscriber::with_default(subscriber, || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(fut());
    });
    capture.spans()
}

/// Find the single `agent.run` span whose `gen_ai.agent.name` equals `name`,
/// asserting there is exactly one (so a duplicate-span regression fails the test
/// rather than silently selecting the first match).
fn agent_run_span<'a>(spans: &'a [CapturedSpan], name: &str) -> &'a CapturedSpan {
    let matches: Vec<&CapturedSpan> = spans
        .iter()
        .filter(|s| {
            s.name == "agent.run"
                && s.fields.get("gen_ai.agent.name").map(String::as_str) == Some(name)
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one agent.run span for {name:?}; captured: {spans:?}",
    );
    matches[0]
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn sequential_run_emits_invoke_agent_span_with_tracer_fields() {
    let tracer = TracerHandle::builder()
        .with_session_id("sess-1")
        .with_user_id("user-1")
        .build();
    let spans = run_under_capture(|| async {
        let seq = SequentialAgent::new("seq", "")
            .then(MockAgent::new("a", |_| msg_and_complete("a", "A", 0)));
        let _ = RunResultStreaming::new(
            seq.run(ctx_with_tracer(tracer), AgentInput::from_user_text("go"))
                .await
                .unwrap(),
        )
        .collect()
        .await
        .unwrap();
    });

    let span = agent_run_span(&spans, "seq");
    assert_eq!(
        span.fields.get("gen_ai.operation.name").map(String::as_str),
        Some("invoke_agent"),
    );
    assert_eq!(
        span.fields.get("otel.kind").map(String::as_str),
        Some("internal")
    );
    assert_eq!(
        span.fields.get("otel.name").map(String::as_str),
        Some("invoke_agent seq")
    );
    // Langfuse trace attributes propagate from the RunContext tracer.
    assert_eq!(
        span.fields.get("langfuse.session.id").map(String::as_str),
        Some("sess-1")
    );
    assert_eq!(
        span.fields.get("langfuse.user.id").map(String::as_str),
        Some("user-1")
    );
}

#[test]
fn sub_agent_span_nests_under_workflow_span() {
    // outer Sequential → inner Sequential → leaf MockAgent. Both Sequential agents
    // emit `agent.run` spans; the inner must be a child of the outer (proves the
    // `.instrument(span.clone())` wrapping makes sub-agent spans nest).
    let spans = run_under_capture(|| async {
        let inner = SequentialAgent::new("inner", "")
            .then(MockAgent::new("leaf", |_| msg_and_complete("leaf", "x", 0)));
        let outer = SequentialAgent::new("outer", "").then(inner);
        let _ = RunResultStreaming::new(
            outer
                .run(ctx(), AgentInput::from_user_text("go"))
                .await
                .unwrap(),
        )
        .collect()
        .await
        .unwrap();
    });

    let outer = agent_run_span(&spans, "outer");
    let inner = agent_run_span(&spans, "inner");
    assert_eq!(
        inner.parent,
        Some(outer.id),
        "inner workflow span must nest under the outer workflow span",
    );
}

#[test]
fn successful_run_records_usage_on_the_span() {
    let spans = run_under_capture(|| async {
        let seq = SequentialAgent::new("seq", "")
            .then(MockAgent::new("a", |_| msg_and_complete("a", "A", 7)));
        let _ = RunResultStreaming::new(
            seq.run(ctx(), AgentInput::from_user_text("go"))
                .await
                .unwrap(),
        )
        .collect()
        .await
        .unwrap();
    });

    let span = agent_run_span(&spans, "seq");
    assert_eq!(
        span.fields
            .get("gen_ai.usage.input_tokens")
            .map(String::as_str),
        Some("7")
    );
    // A successful run must NOT mark the span as errored.
    assert_ne!(
        span.fields.get("otel.status_code").map(String::as_str),
        Some("ERROR")
    );
}

#[test]
fn failed_run_records_error_status_on_the_span() {
    let spans = run_under_capture(|| async {
        let boom = MockAgent::new("boom", |ctx| {
            ctx.failure_handle()
                .set(AgentError::Other(anyhow::anyhow!("kaboom")));
            vec![
                AgentEvent::RunStarted {
                    agent: "boom".to_owned(),
                },
                AgentEvent::RunFailed {
                    error: "kaboom".to_owned(),
                },
            ]
        });
        let seq = SequentialAgent::new("seq", "").then(boom);
        let _ = RunResultStreaming::new(
            seq.run(ctx(), AgentInput::from_user_text("go"))
                .await
                .unwrap(),
        )
        .collect()
        .await;
    });

    let span = agent_run_span(&spans, "seq");
    assert_eq!(
        span.fields.get("otel.status_code").map(String::as_str),
        Some("ERROR"),
        "a failed workflow run must record otel.status_code = ERROR",
    );
}
