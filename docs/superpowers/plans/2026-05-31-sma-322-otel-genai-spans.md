# OpenTelemetry GenAI Spans (SMA-322) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Emit GenAI-semconv `tracing` spans (`invoke_agent`/`chat`/`execute_tool` + a custom turn span) from the agent loop so any standard OTLP backend (Langfuse/Datadog/…) ingests them, with a runnable Langfuse example.

**Architecture:** `core` and providers emit plain `tracing` spans (zero new production deps). Span hierarchy is built with explicit `parent:` links and held as `Span` handles (never `Entered` guards) so nothing is held across the `async_stream`'s `.await`/`yield`. Dynamic semconv span names are set via the `otel.name` field that `tracing-opentelemetry` honors. The OTLP→Langfuse exporter — including a wrapper that converts `langfuse.trace.tags` from a JSON-string to a native array — ships only in an example using dev-dependencies.

**Tech Stack:** Rust, `tracing` 0.1, `tracing-opentelemetry` 0.28 / `opentelemetry` 0.27 / `opentelemetry_sdk` 0.27 / `opentelemetry-otlp` 0.27 (dev-only), `tokio`, `serde_json`.

**Spec:** `docs/superpowers/specs/2026-05-31-sma-322-otel-genai-spans-design.md`

---

## File Structure

- `crates/paigasus-helikon-core/src/context.rs` — `TracerHandle` gains real fields + `TracerHandleBuilder` + accessors (Task 1).
- `crates/paigasus-helikon-core/src/model.rs` — `Model::provider()` / `Model::model()` default-impl getters (Task 2).
- `crates/paigasus-helikon-providers-openai/src/model.rs`, `…-anthropic/src/model.rs` — override the two getters (Task 3).
- `Cargo.toml` (root) + `crates/paigasus-helikon/Cargo.toml` — OTel example-stack deps (Task 4).
- `crates/paigasus-helikon/tests/otel_spans.rs` — the InMemory-OTel span-tree integration test; TDD driver for the instrumentation (Task 5).
- `crates/paigasus-helikon-core/src/agent.rs` — open/close run/turn/chat spans in the loop; per-call tool spans in `run_tools_concurrent` (Task 6).
- `crates/paigasus-helikon/examples/langfuse_tracing.rs` — exporter wiring + tags-array exporter wrapper (Task 7).
- `deny.toml` (conditional) — license resolution for the new dev-dep TLS chain (Task 8).

**Release note (no manual version bump in this PR):** `core` is at `0.2.3`; release-plz auto-bumps it (feat → minor → `0.3.0`) on merge and `dependencies_update` cascades the facade. No stub is ascending and no new crate is added, so the manual ascend ritual does **not** apply. `cargo publish --verify` builds only the lib (not examples/tests), so the facade example using new `core` API does not create a publish-time deadlock. Do **not** hand-edit any `version =` or `[workspace.dependencies]` pin here.

---

### Task 1: `TracerHandle` real fields + builder

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs:247-255` (the `TracerHandle` placeholder) and append a `TracerHandleBuilder`.
- Test: same file, in the existing `#[cfg(test)] mod runcontext_tests` (around `context.rs:152`).

- [ ] **Step 1: Write the failing test**

Add to `mod runcontext_tests` in `crates/paigasus-helikon-core/src/context.rs`:

```rust
    #[test]
    fn tracer_handle_builder_roundtrips_and_default_is_empty() {
        let empty = TracerHandle::default();
        assert!(empty.session_id().is_none());
        assert!(empty.user_id().is_none());
        assert!(empty.tags().is_empty());

        let h = TracerHandle::builder()
            .with_session_id("sess-1")
            .with_user_id("user-1")
            .with_tag("prod")
            .with_tag("beta")
            .build();
        assert_eq!(h.session_id(), Some("sess-1"));
        assert_eq!(h.user_id(), Some("user-1"));
        assert_eq!(h.tags(), &["prod".to_string(), "beta".to_string()]);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-core --lib tracer_handle_builder_roundtrips`
Expected: FAIL to compile — `builder`, `with_session_id`, `session_id`, etc. do not exist.

- [ ] **Step 3: Implement the type + builder**

Replace the placeholder at `crates/paigasus-helikon-core/src/context.rs:247-255`:

```rust
/// Carrier for per-run trace-level attributes (Langfuse `session.id` /
/// `user.id` / `tags`) that the agent loop stamps onto the run and turn
/// spans. Construct an empty handle with [`TracerHandle::default`] or a
/// populated one via [`TracerHandle::builder`].
#[derive(Debug, Clone, Default)]
pub struct TracerHandle {
    session_id: Option<String>,
    user_id: Option<String>,
    tags: Vec<String>,
}

impl TracerHandle {
    /// Start building a populated handle.
    pub fn builder() -> TracerHandleBuilder {
        TracerHandleBuilder::default()
    }
    /// Langfuse session id, if set.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    /// Langfuse user id, if set.
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }
    /// Langfuse trace tags (possibly empty).
    pub fn tags(&self) -> &[String] {
        &self.tags
    }
}

/// Consuming builder for [`TracerHandle`].
#[derive(Debug, Default)]
pub struct TracerHandleBuilder {
    session_id: Option<String>,
    user_id: Option<String>,
    tags: Vec<String>,
}

impl TracerHandleBuilder {
    /// Set the Langfuse session id.
    pub fn with_session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }
    /// Set the Langfuse user id.
    pub fn with_user_id(mut self, id: impl Into<String>) -> Self {
        self.user_id = Some(id.into());
        self
    }
    /// Append one Langfuse trace tag.
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }
    /// Finish building the [`TracerHandle`].
    pub fn build(self) -> TracerHandle {
        TracerHandle {
            session_id: self.session_id,
            user_id: self.user_id,
            tags: self.tags,
        }
    }
}
```

(`pub use context::*;` at `core/src/lib.rs:35` re-exports `TracerHandleBuilder` automatically — no export edit needed.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p paigasus-helikon-core --lib tracer_handle_builder_roundtrips`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/context.rs
git commit -m "feat(core): SMA-322 add TracerHandle trace-metadata fields + builder"
```

---

### Task 2: `Model::provider()` / `model()` getters

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs:50-73` (the `Model` trait).
- Test: same file, in the `#[cfg(test)] mod tests` (the module that has `model_event_usage_constructs` around `model.rs:503`).

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/paigasus-helikon-core/src/model.rs`:

```rust
    #[test]
    fn model_descriptor_getters_default_to_unknown() {
        struct Bare;
        #[async_trait::async_trait]
        impl crate::Model for Bare {
            async fn invoke(
                &self,
                _req: crate::ModelRequest,
                _cancel: crate::CancellationToken,
            ) -> Result<
                futures_core::stream::BoxStream<
                    'static,
                    Result<crate::ModelEvent, crate::ModelError>,
                >,
                crate::ModelError,
            > {
                Ok(Box::pin(futures_util::stream::empty()))
            }
            fn capabilities(&self) -> crate::ModelCapabilities {
                crate::ModelCapabilities::default()
            }
        }
        let m = Bare;
        assert_eq!(m.provider(), "unknown");
        assert_eq!(m.model(), "");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-core --lib model_descriptor_getters_default`
Expected: FAIL to compile — no method `provider` / `model` on `Model`.

- [ ] **Step 3: Add the default-impl getters**

In `crates/paigasus-helikon-core/src/model.rs`, inside `pub trait Model`, after `fn capabilities(&self) -> ModelCapabilities;` (currently `model.rs:72`):

```rust
    /// GenAI `gen_ai.provider.name` — the provider identifier (e.g.
    /// `"openai"`, `"anthropic"`). Default `"unknown"` is elided from spans.
    fn provider(&self) -> &str {
        "unknown"
    }

    /// GenAI `gen_ai.request.model` — the configured model id (e.g.
    /// `"gpt-4o"`). Default `""` is elided from spans.
    fn model(&self) -> &str {
        ""
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p paigasus-helikon-core --lib model_descriptor_getters_default`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/model.rs
git commit -m "feat(core): SMA-322 add Model::provider/model getters for gen_ai semconv"
```

---

### Task 3: Provider overrides (`openai`, `anthropic`)

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/model.rs:54-69` (the `impl Model for OpenAiModel`).
- Modify: `crates/paigasus-helikon-providers-anthropic/src/model.rs:38-167` (the `impl Model for AnthropicModel`).
- Test: each provider's existing `#[cfg(test)] mod tests`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/paigasus-helikon-providers-anthropic/src/model.rs` (next to `capabilities_reflects_builder_lookup`):

```rust
    #[test]
    fn provider_and_model_getters() {
        let m = AnthropicModel::messages("claude-sonnet-4-6")
            .api_key("sk-test")
            .build()
            .unwrap();
        assert_eq!(m.provider(), "anthropic");
        assert_eq!(m.model(), "claude-sonnet-4-6");
    }
```

Add a `#[cfg(test)] mod tests` to `crates/paigasus-helikon-providers-openai/src/model.rs` (the file has none today):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_and_model_getters() {
        let m = OpenAiModel::chat("gpt-4o").api_key("sk-test").build().unwrap();
        assert_eq!(m.provider(), "openai");
        assert_eq!(m.model(), "gpt-4o");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p paigasus-helikon-providers-openai -p paigasus-helikon-providers-anthropic provider_and_model_getters`
Expected: FAIL to compile — `provider`/`model` not overridden (they resolve to defaults `"unknown"`/`""`, so assertions fail).

- [ ] **Step 3: Add the overrides**

In `crates/paigasus-helikon-providers-openai/src/model.rs`, inside `impl Model for OpenAiModel`, after `fn capabilities`:

```rust
    fn provider(&self) -> &str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.model_id
    }
```

In `crates/paigasus-helikon-providers-anthropic/src/model.rs`, inside `impl Model for AnthropicModel`, after `fn capabilities`:

```rust
    fn provider(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.cfg.model_id
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p paigasus-helikon-providers-openai -p paigasus-helikon-providers-anthropic provider_and_model_getters`
Expected: PASS (both).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-openai/src/model.rs crates/paigasus-helikon-providers-anthropic/src/model.rs
git commit -m "feat(providers-openai,providers-anthropic): SMA-322 report gen_ai provider/model"
```

---

### Task 4: OTel example-stack dependencies (dev-only)

**Files:**
- Modify: `Cargo.toml` (root) `[workspace.dependencies]`.
- Modify: `crates/paigasus-helikon/Cargo.toml` `[dev-dependencies]`.

- [ ] **Step 1: Add workspace dependency pins**

In root `Cargo.toml` `[workspace.dependencies]`, after the `tracing` line:

```toml
tracing-subscriber    = { version = "0.3", features = ["env-filter", "fmt"] }
tracing-opentelemetry = "0.28"
opentelemetry_sdk     = { version = "0.27", features = ["rt-tokio", "testing"] }
opentelemetry-otlp    = { version = "0.27", default-features = false, features = ["trace", "http-proto", "reqwest-client"] }
```

(`opentelemetry = "0.27"` is already present.)

- [ ] **Step 2: Add facade dev-dependencies**

In `crates/paigasus-helikon/Cargo.toml` `[dev-dependencies]`, append:

```toml
# SMA-322 — OTel export stack for the langfuse_tracing example + otel_spans test.
async-trait           = { workspace = true }
futures-core          = { workspace = true }
futures-util          = { workspace = true }
tracing               = { workspace = true }
tracing-subscriber    = { workspace = true }
tracing-opentelemetry = { workspace = true }
opentelemetry         = { workspace = true }
opentelemetry_sdk     = { workspace = true }
opentelemetry-otlp    = { workspace = true }
```

- [ ] **Step 3: Verify the graph resolves**

Run: `cargo build -p paigasus-helikon --tests`
Expected: PASS (compiles; no example/test references the new crates yet).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/paigasus-helikon/Cargo.toml Cargo.lock
git commit -m "build(facade): SMA-322 add OTel export stack as dev-dependencies"
```

---

### Task 5: Failing span-tree integration test (InMemory OTel)

**Files:**
- Create: `crates/paigasus-helikon/tests/otel_spans.rs`

This test is the TDD driver for Task 6. It installs a real `tracing-opentelemetry` layer backed by an in-memory span exporter, runs a two-turn scripted agent (model → tool call → model), and asserts the exported span tree + attributes. It fails now because `core` emits no spans yet.

- [ ] **Step 1: Write the failing test**

Create `crates/paigasus-helikon/tests/otel_spans.rs`:

```rust
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
    Agent, AgentInput, CancellationToken, FinishReason, HookRegistry, Instructions, LlmAgent,
    Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest, ModelSettings, RunConfig,
    RunContext, RunResultStreaming, Session, SessionError, SessionEvent, SequenceId,
    ConversationSnapshot, Tool, ToolContext, ToolError, ToolOutput, TracerHandle,
};

// --- minimal scripted model that reports provider/model + usage ---
struct ScriptedModel {
    scripts: std::sync::Mutex<std::collections::VecDeque<Vec<ModelEvent>>>,
}
impl ScriptedModel {
    fn new(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self { scripts: std::sync::Mutex::new(scripts.into()) })
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
    fn provider(&self) -> &str { "openai" }
    fn model(&self) -> &str { "gpt-4o" }
}

struct EchoTool;
#[async_trait]
impl Tool<()> for EchoTool {
    fn name(&self) -> &str { "echo" }
    fn description(&self) -> &str { "echo tool" }
    fn schema(&self) -> &serde_json::Value {
        static S: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
        S.get_or_init(|| serde_json::json!({"type": "object"}))
    }
    async fn invoke(&self, _c: &ToolContext<()>, _a: serde_json::Value)
        -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(serde_json::json!("ok")))
    }
}

struct NoopSession;
#[async_trait]
impl Session for NoopSession {
    async fn append(&self, _: &[SessionEvent]) -> Result<(), SessionError> { Ok(()) }
    async fn events(&self, _: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        Ok(Vec::new())
    }
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        Ok(ConversationSnapshot::default())
    }
}

fn attr<'a>(span: &'a opentelemetry_sdk::export::trace::SpanData, key: &str) -> Option<&'a Value> {
    span.attributes.iter().find(|kv| kv.key.as_str() == key).map(|kv| &kv.value)
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

        // turn 0: tool call; turn 1: final text. Usage on each model call.
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
                ModelEvent::Finish { reason: FinishReason::ToolCalls },
            ],
            vec![
                ModelEvent::TokenDelta { text: "done".into() },
                ModelEvent::Usage {
                    input_tokens: 33,
                    output_tokens: 44,
                    cached_input_tokens: None,
                    reasoning_tokens: None,
                },
                ModelEvent::Finish { reason: FinishReason::Stop },
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

        let stream = agent.run(ctx, AgentInput::from_user_text("hi")).await.unwrap();
        let _ = RunResultStreaming::new(stream).collect().await.unwrap();
    } // drop guard

    provider.force_flush();
    let spans = exporter.get_finished_spans().unwrap();
    let names: Vec<&str> = spans.iter().map(|s| s.name.as_ref()).collect();

    // run span
    let run = spans.iter().find(|s| s.name.starts_with("invoke_agent")).expect("run span");
    assert_eq!(run.name.as_ref(), "invoke_agent assistant");
    assert_eq!(attr(run, "gen_ai.operation.name"), Some(&Value::from("invoke_agent")));
    assert_eq!(attr(run, "gen_ai.agent.name"), Some(&Value::from("assistant")));
    assert_eq!(attr(run, "langfuse.session.id"), Some(&Value::from("sess-1")));
    assert_eq!(attr(run, "langfuse.user.id"), Some(&Value::from("user-1")));
    assert_eq!(attr(run, "langfuse.trace.tags"), Some(&Value::from("[\"prod\"]")));
    assert_eq!(attr(run, "gen_ai.usage.input_tokens"), Some(&Value::from(44_i64))); // 11+33
    assert_eq!(attr(run, "gen_ai.usage.output_tokens"), Some(&Value::from(66_i64))); // 22+44

    // two chat spans
    let chats: Vec<_> = spans.iter().filter(|s| s.name.starts_with("chat ")).collect();
    assert_eq!(chats.len(), 2, "names were {names:?}");
    for c in &chats {
        assert_eq!(c.name.as_ref(), "chat gpt-4o");
        assert_eq!(attr(c, "gen_ai.operation.name"), Some(&Value::from("chat")));
        assert_eq!(attr(c, "gen_ai.provider.name"), Some(&Value::from("openai")));
        assert_eq!(attr(c, "gen_ai.request.model"), Some(&Value::from("gpt-4o")));
    }
    // first chat reported 11/22
    assert!(chats.iter().any(|c|
        attr(c, "gen_ai.usage.input_tokens") == Some(&Value::from(11_i64))
            && attr(c, "gen_ai.usage.output_tokens") == Some(&Value::from(22_i64))));

    // one tool span
    let tool = spans.iter().find(|s| s.name.starts_with("execute_tool")).expect("tool span");
    assert_eq!(tool.name.as_ref(), "execute_tool echo");
    assert_eq!(attr(tool, "gen_ai.operation.name"), Some(&Value::from("execute_tool")));
    assert_eq!(attr(tool, "gen_ai.tool.name"), Some(&Value::from("echo")));

    // a custom turn span exists
    assert!(spans.iter().any(|s| s.name.as_ref() == "agent.turn"), "names were {names:?}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon --test otel_spans`
Expected: FAIL — `expect("run span")` panics (no spans emitted yet; `spans` is empty).

- [ ] **Step 3: Commit the failing test**

```bash
git add crates/paigasus-helikon/tests/otel_spans.rs
git commit -m "test(facade): SMA-322 add failing GenAI-semconv span-tree test"
```

---

### Task 6: Instrument the agent loop (make Task 5 pass)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` — `run_tools_concurrent` (`agent.rs:488`) and `LlmAgent::run` (`agent.rs:584-747`).

All spans use `tracing::field::Empty` for deferred fields and are closed by dropping their `Span` handle. No `Span::enter()` anywhere (would deadlock the `async_stream`).

- [ ] **Step 1: Add the `Instrument` import**

At the top of `crates/paigasus-helikon-core/src/agent.rs` (with the other `use` lines near the existing imports), add:

```rust
use tracing::Instrument as _;
```

- [ ] **Step 2: Give `run_tools_concurrent` a parent span + per-call tool spans**

Change the signature at `agent.rs:488` to add a `parent` param:

```rust
async fn run_tools_concurrent<Ctx>(
    tools: &[std::sync::Arc<dyn crate::Tool<Ctx>>],
    calls: &[crate::ToolCallRequest],
    tool_ctx: &crate::ToolContext<Ctx>,
    limit: Option<std::num::NonZeroUsize>,
    parent: &tracing::Span,
) -> Vec<crate::ToolCallOutcome>
where
    Ctx: Send + Sync + 'static,
{
```

Replace the `let futures = calls.iter().map(|call| { … });` closure body so each future is wrapped in an `execute_tool` span:

```rust
    let futures = calls.iter().map(|call| {
        let tool = tools.iter().find(|t| t.name() == call.name).cloned();
        let call_id = call.call_id.clone();
        let args = call.args.clone();
        let name = call.name.clone();
        let span = tracing::info_span!(
            parent: parent,
            "tool.execute",
            otel.name = tracing::field::Empty,
            otel.kind = "internal",
            gen_ai.operation.name = "execute_tool",
            gen_ai.tool.name = %name,
            otel.status_code = tracing::field::Empty,
        );
        span.record("otel.name", format!("execute_tool {name}").as_str());
        async move {
            match tool {
                Some(t) => match t.invoke(tool_ctx, args).await {
                    Ok(output) => crate::ToolCallOutcome {
                        call_id,
                        result: Ok(tool_output_to_content_parts(&output)),
                    },
                    Err(e) => {
                        tracing::Span::current().record("otel.status_code", "ERROR");
                        crate::ToolCallOutcome { call_id, result: Err(e.to_string()) }
                    }
                },
                None => {
                    tracing::Span::current().record("otel.status_code", "ERROR");
                    crate::ToolCallOutcome { call_id, result: Err(format!("unknown tool: {name}")) }
                }
            }
        }
        .instrument(span)
    });
```

(The `match limit { … }` block below is unchanged.)

- [ ] **Step 3: Open the run span + track the turn span in `LlmAgent::run`**

In `agent.rs`, immediately before `yield crate::AgentEvent::RunStarted { agent: agent_name.clone() };` (currently `agent.rs:604`), insert:

```rust
            let run_span = tracing::info_span!(
                "agent.run",
                otel.name = tracing::field::Empty,
                otel.kind = "internal",
                gen_ai.operation.name = "invoke_agent",
                gen_ai.agent.name = %agent_name,
                langfuse.session.id = tracing::field::Empty,
                langfuse.user.id = tracing::field::Empty,
                langfuse.trace.tags = tracing::field::Empty,
                gen_ai.usage.input_tokens = tracing::field::Empty,
                gen_ai.usage.output_tokens = tracing::field::Empty,
                otel.status_code = tracing::field::Empty,
            );
            run_span.record("otel.name", format!("invoke_agent {agent_name}").as_str());
            if let Some(v) = ctx.tracer().session_id() {
                run_span.record("langfuse.session.id", v);
            }
            if let Some(v) = ctx.tracer().user_id() {
                run_span.record("langfuse.user.id", v);
            }
            if !ctx.tracer().tags().is_empty() {
                if let Ok(json) = serde_json::to_string(ctx.tracer().tags()) {
                    run_span.record("langfuse.trace.tags", json.as_str());
                }
            }
            let mut turn_span: Option<tracing::Span> = None;
```

- [ ] **Step 4: Drive turn/usage/failure spans off the event stream**

Replace the existing `for ev in events { yield ev; }` (currently `agent.rs:616`) with:

```rust
                for ev in events {
                    match &ev {
                        crate::AgentEvent::TurnStarted { turn } => {
                            let s = tracing::info_span!(
                                parent: &run_span,
                                "agent.turn",
                                otel.kind = "internal",
                                turn = *turn,
                                langfuse.session.id = tracing::field::Empty,
                                langfuse.user.id = tracing::field::Empty,
                                langfuse.trace.tags = tracing::field::Empty,
                            );
                            if let Some(v) = ctx.tracer().session_id() {
                                s.record("langfuse.session.id", v);
                            }
                            if let Some(v) = ctx.tracer().user_id() {
                                s.record("langfuse.user.id", v);
                            }
                            if !ctx.tracer().tags().is_empty() {
                                if let Ok(json) = serde_json::to_string(ctx.tracer().tags()) {
                                    s.record("langfuse.trace.tags", json.as_str());
                                }
                            }
                            turn_span = Some(s);
                        }
                        crate::AgentEvent::RunCompleted { usage } => {
                            run_span.record("gen_ai.usage.input_tokens", usage.input_tokens);
                            run_span.record("gen_ai.usage.output_tokens", usage.output_tokens);
                        }
                        crate::AgentEvent::RunFailed { .. } => {
                            run_span.record("otel.status_code", "ERROR");
                        }
                        _ => {}
                    }
                    yield ev;
                }
```

- [ ] **Step 5: Open the chat span around the model call + record usage/errors**

In the `crate::NextAction::CallModel { request } => {` arm (`agent.rs:621`), insert at the very top of the arm (before `let cancel = …`):

```rust
                        let chat_parent = turn_span.as_ref().unwrap_or(&run_span);
                        let chat_span = tracing::info_span!(
                            parent: chat_parent,
                            "gen_ai.chat",
                            otel.name = tracing::field::Empty,
                            otel.kind = "client",
                            gen_ai.operation.name = "chat",
                            gen_ai.provider.name = %model.provider(),
                            gen_ai.request.model = %model.model(),
                            gen_ai.usage.input_tokens = tracing::field::Empty,
                            gen_ai.usage.output_tokens = tracing::field::Empty,
                            otel.status_code = tracing::field::Empty,
                        );
                        chat_span.record("otel.name", format!("chat {}", model.model()).as_str());
```

In the same arm, in the `model.invoke(...)` error branch, before `yield crate::AgentEvent::RunFailed { error: msg };`:

```rust
                                chat_span.record("otel.status_code", "ERROR");
                                run_span.record("otel.status_code", "ERROR");
```

In the `Ok(crate::ModelEvent::Usage { … })` arm, after `latest_usage = Some(…);`, add:

```rust
                                    chat_span.record(
                                        "gen_ai.usage.input_tokens",
                                        u64::from(input_tokens),
                                    );
                                    chat_span.record(
                                        "gen_ai.usage.output_tokens",
                                        u64::from(output_tokens),
                                    );
```

In the stream-error arm (`Err(e) => { … }` inside the `while let`) and the `build_items` error arm, add before each `yield crate::AgentEvent::RunFailed { … };`:

```rust
                                chat_span.record("otel.status_code", "ERROR");
                                run_span.record("otel.status_code", "ERROR");
```

(The chat span drops at the end of the `CallModel` arm, closing it after the model stream is fully consumed.)

- [ ] **Step 6: Pass the turn span into tool execution**

In the `crate::NextAction::ExecuteTools { calls } => {` arm (`agent.rs:715`), change the `run_tools_concurrent(...)` call to pass the parent:

```rust
                        let tool_parent = turn_span.as_ref().unwrap_or(&run_span);
                        let outcomes = run_tools_concurrent(
                            &tools,
                            &calls,
                            &tool_ctx,
                            parallel_tool_call_limit,
                            tool_parent,
                        )
                        .await;
```

- [ ] **Step 7: Run the span-tree test**

Run: `cargo test -p paigasus-helikon --test otel_spans`
Expected: PASS.

- [ ] **Step 8: Run the existing loop tests (no regressions)**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS (the `loop_*`, `structured_output`, `failure_slot`, etc. suites are unaffected — spans are inert without a subscriber).

- [ ] **Step 9: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs
git commit -m "feat(core): SMA-322 emit GenAI-semconv spans across the agent loop"
```

---

### Task 7: Langfuse OTLP example + tags-array exporter

**Files:**
- Create: `crates/paigasus-helikon/examples/langfuse_tracing.rs`
- Modify: `crates/paigasus-helikon/Cargo.toml` (add the `[[example]]` entry).

- [ ] **Step 1: Register the example**

In `crates/paigasus-helikon/Cargo.toml`, after the existing `[[example]]` block:

```toml
[[example]]
name              = "langfuse_tracing"
required-features = ["runtime-tokio"]
```

- [ ] **Step 2: Write the example**

Create `crates/paigasus-helikon/examples/langfuse_tracing.rs`:

```rust
//! SMA-322 — export the agent's GenAI-semconv spans to Langfuse via OTLP.
//!
//! Run:
//!   LANGFUSE_HOST=https://cloud.langfuse.com \
//!   LANGFUSE_PUBLIC_KEY=pk-... LANGFUSE_SECRET_KEY=sk-... \
//!   cargo run -p paigasus-helikon --example langfuse_tracing --features runtime-tokio
//!
//! Manual verification (acceptance criterion): open the run in Langfuse and
//! confirm the trace tree `invoke_agent → agent.turn → chat / execute_tool`,
//! with token counts on the `chat` observation and session/user/tags on the
//! trace.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use futures_core::stream::BoxStream;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{KeyValue, Value};
use opentelemetry_sdk::export::trace::{SpanData, SpanExporter};
use opentelemetry_sdk::trace::{BatchSpanProcessor, TracerProvider};
use tracing_subscriber::prelude::*;

use paigasus_helikon::core::{
    Agent, AgentInput, CancellationToken, FinishReason, HookRegistry, Instructions, LlmAgent,
    Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest, ModelSettings, RunConfig,
    RunContext, Session, SessionError, SessionEvent, SequenceId, ConversationSnapshot,
    TracerHandle,
};
use paigasus_helikon::runtime_tokio::TokioRunner;
use paigasus_helikon::core::Runner;

/// Wraps an OTLP exporter and rewrites the `langfuse.trace.tags` JSON-string
/// attribute into a native `string[]` before export (Langfuse requires an
/// array; `core` stays `tracing`-only and can only emit a scalar string).
#[derive(Debug)]
struct TagsArrayExporter<E: SpanExporter>(E);

impl<E: SpanExporter> SpanExporter for TagsArrayExporter<E> {
    fn export(
        &mut self,
        mut batch: Vec<SpanData>,
    ) -> futures_core::future::BoxFuture<'static, opentelemetry_sdk::export::trace::ExportResult>
    {
        for span in &mut batch {
            if let Some(kv) = span
                .attributes
                .iter_mut()
                .find(|kv| kv.key.as_str() == "langfuse.trace.tags")
            {
                if let Value::String(s) = &kv.value {
                    if let Ok(tags) = serde_json::from_str::<Vec<String>>(s.as_str()) {
                        kv.value = Value::Array(
                            tags.into_iter().map(opentelemetry::StringValue::from).collect::<Vec<_>>().into(),
                        );
                    }
                }
            }
        }
        self.0.export(batch)
    }
    fn shutdown(&mut self) {
        self.0.shutdown();
    }
}

// --- a tiny scripted model so the example needs no LLM API key ---
struct DemoModel;
#[async_trait]
impl Model for DemoModel {
    async fn invoke(
        &self,
        _req: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let events = vec![
            Ok(ModelEvent::TokenDelta { text: "hello from helikon".into() }),
            Ok(ModelEvent::Usage {
                input_tokens: 128,
                output_tokens: 16,
                cached_input_tokens: None,
                reasoning_tokens: None,
            }),
            Ok(ModelEvent::Finish { reason: FinishReason::Stop }),
        ];
        Ok(Box::pin(futures_util::stream::iter(events)))
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    fn provider(&self) -> &str { "demo" }
    fn model(&self) -> &str { "demo-1" }
}

struct NoopSession;
#[async_trait]
impl Session for NoopSession {
    async fn append(&self, _: &[SessionEvent]) -> Result<(), SessionError> { Ok(()) }
    async fn events(&self, _: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        Ok(Vec::new())
    }
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        Ok(ConversationSnapshot::default())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let host = std::env::var("LANGFUSE_HOST")
        .unwrap_or_else(|_| "https://cloud.langfuse.com".into());
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

    let provider = TracerProvider::builder()
        .with_span_processor(BatchSpanProcessor::builder(
            TagsArrayExporter(otlp),
            opentelemetry_sdk::runtime::Tokio,
        ).build())
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
        .run(&agent, ctx, AgentInput::from_user_text("hi"), RunConfig::default())
        .await?;
    println!("final output: {}", result.final_output);

    provider.force_flush();
    let _ = provider.shutdown();
    let _ = KeyValue::new("k", "v"); // (KeyValue import used by TagsArrayExporter)
    Ok(())
}
```

> Note for the implementer: `base64` is not yet a workspace dep. Add `base64 = "0.22"` to `[workspace.dependencies]` and to the facade `[dev-dependencies]` (`base64 = { workspace = true }`) in this task. If the exact `opentelemetry-otlp` 0.27 builder method names differ (`with_http` / `with_headers`), adjust to the 0.27 API surface — the shape (HTTP exporter + endpoint + Authorization header) is the contract.

- [ ] **Step 3: Build the example**

Run: `cargo build -p paigasus-helikon --example langfuse_tracing --features runtime-tokio`
Expected: PASS (compiles; running it requires `LANGFUSE_*` env vars).

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon/examples/langfuse_tracing.rs crates/paigasus-helikon/Cargo.toml Cargo.toml Cargo.lock
git commit -m "docs(examples): SMA-322 add Langfuse OTLP tracing example"
```

---

### Task 8: Supply-chain gate (deny / audit / sbom)

**Files:**
- Modify (conditional): `deny.toml`

The new dev-deps pull a reqwest→rustls→TLS chain into the `cargo deny` / `cargo audit` graph (both scan dev-deps). This task verifies and resolves it. It is a run-inspect-resolve task, not TDD.

- [ ] **Step 1: Run the license/advisory gates**

Run: `cargo deny check 2>&1 | tee /tmp/deny.txt` and `cargo audit`
Expected: `cargo deny` may report a `license` error for `ring` and/or `aws-lc-rs` (their licenses are not in the current allowlist). Note the exact crate + version from the output.

- [ ] **Step 2: Resolve any license finding**

Pick the resolution that the output dictates:
- **Preferred — add a scoped exception.** In `deny.toml` under `[licenses]`, add the flagged crate to `exceptions` (this is the documented mechanism for a single crate whose license is acceptable but not globally allowlisted):

```toml
[[licenses.exceptions]]
name = "ring"          # use the exact crate name cargo deny flagged
allow = ["LicenseRef-ring", "MIT", "ISC", "OpenSSL"]
```

- **Alternative — avoid the crate.** If preferred, pin reqwest's TLS backend in the facade dev-dep (e.g. add `features = ["rustls-tls"]` or `["native-tls"]` to the `opentelemetry-otlp`/reqwest dev-dep) so the dependency tree uses a backend already covered by the allowlist, then re-run.

- [ ] **Step 3: Re-run the gates to green**

Run: `cargo deny check` and `cargo audit`
Expected: PASS (no license/advisory errors).

- [ ] **Step 4: Confirm SBOM is unaffected**

Run: `cargo cyclonedx --manifest-path crates/paigasus-helikon/Cargo.toml --format json --spec-version 1.5 --all-features`
Expected: Succeeds; the produced `*.cdx.json` covers the published (non-dev) graph as before. (Delete the generated SBOM file; it is not committed here.)

- [ ] **Step 5: Commit (only if `deny.toml` changed)**

```bash
git add deny.toml
git commit -m "build: SMA-322 allow OTel dev-dep TLS license in cargo-deny"
```

---

### Task 9: Full local CI gate sweep

**Files:** none (verification + any doc fixes surfaced).

- [ ] **Step 1: Format**

Run: `cargo fmt --all -- --check`
Expected: PASS. If it fails, run `cargo fmt --all` and re-check.

- [ ] **Step 2: Clippy (all features, all targets)**

Run: `cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: PASS. Fix any warnings in the touched files (common: an unused import, or a needless `format!`).

- [ ] **Step 3: Tests**

Run: `cargo test --workspace --all-features`
Expected: PASS (includes `otel_spans`, the new unit tests, and all pre-existing suites).

- [ ] **Step 4: Docs (warnings as errors)**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
Expected: PASS — confirms every new `pub` item (`TracerHandle`, `TracerHandleBuilder`, `Model::provider`/`model`) has a `///` doc and no broken intra-doc links.

- [ ] **Step 5: Doc coverage**

Run: `DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh`
Expected: PASS (≥ 80%). The new public items are all documented.

- [ ] **Step 6: Commit any fixes**

```bash
git add -A
git commit -m "chore(core): SMA-322 satisfy fmt/clippy/doc gates"
```

(Skip if Steps 1–5 produced no changes.)

---

## Self-Review

**Spec coverage:**
- §5 run/turn/chat/tool spans + attributes → Task 6 (impl), Task 5 (asserts names, `otel.kind`, `gen_ai.*`, `langfuse.*`, token counts).
- §6.1 `TracerHandle` API → Task 1. §6.2 `Model::provider`/`model` → Tasks 2–3.
- §4.1 span-guard mechanics (handles, `parent:`, `.instrument`, `field::Empty`) → Task 6 steps. §4.2 dynamic `otel.name` → Task 6 steps 3/5/2.
- §7 example + tags-array conversion → Task 7. §8 verification → Tasks 5 & 8. §8.1 supply-chain gate → Task 8. Doc gates → Task 9.
- Decision 5 (tags JSON-string in `core`, array in example) → Task 6 (records JSON string) + Task 7 (`TagsArrayExporter`).

**Placeholder scan:** No "TBD/handle errors/similar to". The two implementer notes in Task 7 (add `base64`; confirm exact 0.27 OTLP builder method names) are flagged explicitly because the precise OTLP 0.27 builder surface and a new dep can't be asserted without compiling — the contract (HTTP exporter, endpoint, Basic auth header) is stated. Task 8 is a run-inspect-resolve gate by nature; both concrete resolution options are given.

**Type consistency:** `provider()`/`model()` used identically in Tasks 2/3/5/6/7. `TracerHandle::builder().with_session_id/with_user_id/with_tag/build` + `session_id()/user_id()/tags()` consistent across Tasks 1/5/6/7. Span field keys (`gen_ai.operation.name`, `gen_ai.provider.name`, `gen_ai.request.model`, `gen_ai.agent.name`, `gen_ai.tool.name`, `gen_ai.usage.input_tokens`/`output_tokens`, `langfuse.session.id`/`user.id`/`trace.tags`, `otel.name`/`kind`/`status_code`) match between Task 6 (emit) and Task 5 (assert). `run_tools_concurrent`'s new `parent: &tracing::Span` param matches its only call site (Task 6 step 6).
