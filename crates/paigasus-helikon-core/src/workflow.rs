//! Deterministic workflow agents (SMA-325): [`SequentialAgent`],
//! [`ParallelAgent`], [`LoopAgent`].
//!
//! Each implements the same [`crate::Agent`] trait as `LlmAgent` and drives
//! sub-agents, merging their event streams with the handoff-driver convention:
//! swallow each child's `RunStarted`, fold `RunCompleted.usage` into a running
//! total, pass everything else through, and emit one outer `RunStarted` /
//! `RunCompleted`. Sub-agents coordinate through the run-scoped
//! [`crate::SessionState`]; each workflow agent auto-writes a child's final text
//! to `state[key]`.

use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;
use tracing::Instrument as _;

use crate::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunConfig, RunContext,
    TokenUsage, TracerHandle,
};

/// Concatenate the `ContentPart::Text` of an `Item::AssistantMessage`.
fn assistant_text(item: &Item) -> Option<String> {
    match item {
        Item::AssistantMessage { content, .. } => Some(
            content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

/// The effective `max_agent_depth` for a (sub-)run.
fn max_depth(run_config: Option<&RunConfig>) -> u32 {
    run_config
        .map(|c| c.max_agent_depth)
        .unwrap_or_else(|| RunConfig::default().max_agent_depth)
}

/// Build the `invoke_agent` tracing span for a workflow agent's run, mirroring
/// the `LlmAgent` run span (operation, agent name, Langfuse trace attributes).
fn workflow_run_span(agent_name: &str, tracer: &TracerHandle) -> tracing::Span {
    let span = tracing::info_span!(
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
    span.record("otel.name", format!("invoke_agent {agent_name}").as_str());
    if let Some(v) = tracer.session_id() {
        span.record("langfuse.session.id", v);
    }
    if let Some(v) = tracer.user_id() {
        span.record("langfuse.user.id", v);
    }
    if !tracer.tags().is_empty() {
        if let Ok(json) = serde_json::to_string(tracer.tags()) {
            span.record("langfuse.trace.tags", json.as_str());
        }
    }
    span
}

/// Runs sub-agents in order, threading the shared [`crate::SessionState`].
///
/// After each step completes, its final text is written to `state[key]` (key =
/// the agent's name, or an explicit key via [`SequentialAgent::then_keyed`]), so a
/// later step's dynamic `Instructions` closure can read it. Fail-fast on the first
/// step failure. The outer `RunCompleted` carries usage summed across all steps.
pub struct SequentialAgent<Ctx> {
    name: String,
    description: String,
    agents: Vec<(String, Arc<dyn Agent<Ctx>>)>,
}

impl<Ctx> SequentialAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct an empty sequence.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            agents: Vec::new(),
        }
    }

    /// Append a step keyed by the agent's own name.
    pub fn then(mut self, agent: impl Agent<Ctx> + 'static) -> Self {
        let key = agent.name().to_owned();
        self.agents.push((key, Arc::new(agent)));
        self
    }

    /// Append a step with an explicit state key (use when a name would collide).
    pub fn then_keyed(mut self, key: impl Into<String>, agent: impl Agent<Ctx> + 'static) -> Self {
        self.agents.push((key.into(), Arc::new(agent)));
        self
    }

    /// Append a pre-wrapped step keyed by the agent's name.
    pub fn then_shared(mut self, agent: Arc<dyn Agent<Ctx>>) -> Self {
        let key = agent.name().to_owned();
        self.agents.push((key, agent));
        self
    }
}

#[async_trait]
impl<Ctx> Agent<Ctx> for SequentialAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }

    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let name = self.name.clone();
        let agents = self.agents.clone();

        let stream = async_stream::stream! {
            let parent_failure = ctx.failure_handle();
            let span = workflow_run_span(&name, ctx.tracer());
            yield AgentEvent::RunStarted { agent: name.clone() };

            let max = max_depth(ctx.run_config());
            if ctx.agent_depth() + 1 > max {
                let err = AgentError::MaxAgentDepthExceeded { depth: ctx.agent_depth() + 1, max };
                let msg = err.to_string();
                parent_failure.set(err);
                span.record("otel.status_code", "ERROR");
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            let mut total = TokenUsage::default();
            for (key, agent) in &agents {
                let child = ctx.subagent_child();
                let failure = child.failure_handle();
                yield AgentEvent::AgentUpdated { agent: agent.name().to_owned() };

                let mut sub = match agent.run(child, input.clone()).instrument(span.clone()).await {
                    Ok(s) => s,
                    Err(e) => {
                        let msg = e.to_string();
                        parent_failure.set(e);
                        span.record("otel.status_code", "ERROR");
                        yield AgentEvent::RunFailed { error: msg };
                        return;
                    }
                };

                let mut last_text = String::new();
                let mut failed = false;
                while let Some(ev) = sub.next().instrument(span.clone()).await {
                    match ev {
                        AgentEvent::RunStarted { .. } => {}
                        AgentEvent::RunCompleted { usage } => total.add(usage),
                        AgentEvent::RunFailed { error } => {
                            failed = true;
                            span.record("otel.status_code", "ERROR");
                            yield AgentEvent::RunFailed { error };
                        }
                        AgentEvent::MessageOutput { item } => {
                            if let Some(t) = assistant_text(&item) {
                                last_text = t;
                            }
                            yield AgentEvent::MessageOutput { item };
                        }
                        other => yield other,
                    }
                }

                if failed {
                    if let Some(e) = failure.take() {
                        parent_failure.set(e);
                    }
                    return;
                }
                ctx.state().set(key.clone(), last_text);
            }

            span.record("gen_ai.usage.input_tokens", total.input_tokens as i64);
            span.record("gen_ai.usage.output_tokens", total.output_tokens as i64);
            yield AgentEvent::RunCompleted { usage: total };
        };

        Ok(Box::pin(stream))
    }
}

/// Runs sub-agents concurrently (cooperative `futures::stream::select_all` — core
/// has no tokio runtime), interleaving their events live. Each branch is keyed;
/// on completion its final text is written to `state[key]` (disjoint keys → safe).
///
/// `final_output` is deterministic: a synthesized terminal `MessageOutput` carrying
/// a sorted-key JSON object `{key: branch_output}` is emitted before the outer
/// `RunCompleted`. Per-branch results are addressed individually via `state[key]`.
/// Failure is **collect-all**: child `RunFailed` events are swallowed, siblings
/// finish, and one aggregate `RunFailed` is emitted.
///
/// Cooperative concurrency suits IO-bound `model.invoke`; a CPU-bound branch would
/// starve siblings between `.await` points. This is not OS-thread parallelism.
pub struct ParallelAgent<Ctx> {
    name: String,
    description: String,
    branches: Vec<(String, Arc<dyn Agent<Ctx>>)>,
}

impl<Ctx> ParallelAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct an empty parallel block.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            branches: Vec::new(),
        }
    }

    /// Add a branch keyed by the agent's own name.
    #[allow(clippy::should_implement_trait)]
    pub fn add(mut self, agent: impl Agent<Ctx> + 'static) -> Self {
        let key = agent.name().to_owned();
        self.branches.push((key, Arc::new(agent)));
        self
    }

    /// Add a branch with an explicit state key.
    pub fn branch(mut self, key: impl Into<String>, agent: impl Agent<Ctx> + 'static) -> Self {
        self.branches.push((key.into(), Arc::new(agent)));
        self
    }

    /// Add a pre-wrapped branch keyed by the agent's name.
    pub fn add_shared(mut self, agent: Arc<dyn Agent<Ctx>>) -> Self {
        let key = agent.name().to_owned();
        self.branches.push((key, agent));
        self
    }
}

#[async_trait]
impl<Ctx> Agent<Ctx> for ParallelAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }

    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let name = self.name.clone();
        let branches = self.branches.clone();

        let stream = async_stream::stream! {
            let parent_failure = ctx.failure_handle();
            let span = workflow_run_span(&name, ctx.tracer());
            yield AgentEvent::RunStarted { agent: name.clone() };

            let max = max_depth(ctx.run_config());
            if ctx.agent_depth() + 1 > max {
                let err = AgentError::MaxAgentDepthExceeded { depth: ctx.agent_depth() + 1, max };
                let msg = err.to_string();
                parent_failure.set(err);
                span.record("otel.status_code", "ERROR");
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            // Start every branch; tag its stream with the branch index.
            let mut tagged: Vec<BoxStream<'static, (usize, AgentEvent)>> = Vec::new();
            let mut failures: Vec<crate::FailureSlot> = Vec::new();
            for (i, (_key, agent)) in branches.iter().enumerate() {
                let child = ctx.subagent_child();
                failures.push(child.failure_handle());
                yield AgentEvent::AgentUpdated { agent: agent.name().to_owned() };
                match agent.run(child, input.clone()).instrument(span.clone()).await {
                    Ok(s) => tagged.push(Box::pin(s.map(move |ev| (i, ev)))),
                    Err(e) => {
                        let msg = e.to_string();
                        parent_failure.set(e);
                        span.record("otel.status_code", "ERROR");
                        yield AgentEvent::RunFailed { error: msg };
                        return;
                    }
                }
            }

            let mut merged = futures_util::stream::select_all(tagged);
            let mut total = TokenUsage::default();
            let mut finals: Vec<String> = vec![String::new(); branches.len()];
            let mut completed: std::collections::BTreeMap<String, String> =
                std::collections::BTreeMap::new();
            let mut saw_failure = false;

            while let Some((i, ev)) = merged.next().instrument(span.clone()).await {
                match ev {
                    AgentEvent::RunStarted { .. } => {}
                    AgentEvent::RunCompleted { usage } => {
                        total.add(usage);
                        let key = branches[i].0.clone();
                        ctx.state().set(key.clone(), finals[i].clone());
                        completed.insert(key, finals[i].clone());
                    }
                    AgentEvent::RunFailed { .. } => saw_failure = true,
                    AgentEvent::MessageOutput { item } => {
                        if let Some(t) = assistant_text(&item) {
                            finals[i] = t;
                        }
                        yield AgentEvent::MessageOutput { item };
                    }
                    other => yield other,
                }
            }

            let mut first_err: Option<AgentError> = None;
            for fh in &failures {
                if let Some(e) = fh.take() {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
            if saw_failure || first_err.is_some() {
                let err = first_err
                    .unwrap_or_else(|| AgentError::Other(anyhow::anyhow!("a parallel branch failed")));
                let msg = err.to_string();
                parent_failure.set(err);
                span.record("otel.status_code", "ERROR");
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            let json = serde_json::to_string(&completed).unwrap_or_else(|_| "{}".to_owned());
            yield AgentEvent::MessageOutput {
                item: Item::AssistantMessage {
                    content: vec![ContentPart::Text { text: json }],
                    agent: Some(name.clone()),
                },
            };
            span.record("gen_ai.usage.input_tokens", total.input_tokens as i64);
            span.record("gen_ai.usage.output_tokens", total.output_tokens as i64);
            yield AgentEvent::RunCompleted { usage: total };
        };

        Ok(Box::pin(stream))
    }
}

/// Repeats sub-agents (in order) up to `max_iterations`. After each sub-agent
/// completes, its final text is written to `state[key]` and its
/// [`crate::ActionsHandle`] is checked: if a tool escalated, the loop emits
/// `RunCompleted` and stops (success). Exhausting `max_iterations` without an
/// escalate emits `RunFailed` with [`AgentError::MaxIterationsExceeded`].
///
/// Escalate means "no more iterations," not "stop the current sub-agent now" —
/// the active sub-agent always finishes its run first. The signal is checked
/// after **each** sub-agent, so in a multi-sub-agent loop a mid-pass escalate
/// stops before the remaining sub-agents of that pass run.
pub struct LoopAgent<Ctx> {
    name: String,
    description: String,
    agents: Vec<(String, Arc<dyn Agent<Ctx>>)>,
    max_iterations: u32,
}

impl<Ctx> LoopAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct an empty loop with the given iteration budget.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        max_iterations: u32,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            agents: Vec::new(),
            max_iterations,
        }
    }

    /// Append a sub-agent keyed by its own name.
    pub fn then(mut self, agent: impl Agent<Ctx> + 'static) -> Self {
        let key = agent.name().to_owned();
        self.agents.push((key, Arc::new(agent)));
        self
    }

    /// Append a sub-agent with an explicit state key.
    pub fn then_keyed(mut self, key: impl Into<String>, agent: impl Agent<Ctx> + 'static) -> Self {
        self.agents.push((key.into(), Arc::new(agent)));
        self
    }

    /// Append a pre-wrapped sub-agent keyed by its name.
    pub fn then_shared(mut self, agent: Arc<dyn Agent<Ctx>>) -> Self {
        let key = agent.name().to_owned();
        self.agents.push((key, agent));
        self
    }
}

#[async_trait]
impl<Ctx> Agent<Ctx> for LoopAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }

    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let name = self.name.clone();
        let agents = self.agents.clone();
        let max_iterations = self.max_iterations;

        let stream = async_stream::stream! {
            let parent_failure = ctx.failure_handle();
            let span = workflow_run_span(&name, ctx.tracer());
            yield AgentEvent::RunStarted { agent: name.clone() };

            let max = max_depth(ctx.run_config());
            if ctx.agent_depth() + 1 > max {
                let err = AgentError::MaxAgentDepthExceeded { depth: ctx.agent_depth() + 1, max };
                let msg = err.to_string();
                parent_failure.set(err);
                span.record("otel.status_code", "ERROR");
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            let mut total = TokenUsage::default();
            for _iteration in 0..max_iterations {
                for (key, agent) in &agents {
                    let child = ctx.subagent_child();
                    let actions = child.actions().clone();
                    let failure = child.failure_handle();
                    yield AgentEvent::AgentUpdated { agent: agent.name().to_owned() };

                    let mut sub = match agent.run(child, input.clone()).instrument(span.clone()).await {
                        Ok(s) => s,
                        Err(e) => {
                            let msg = e.to_string();
                            parent_failure.set(e);
                            span.record("otel.status_code", "ERROR");
                            yield AgentEvent::RunFailed { error: msg };
                            return;
                        }
                    };

                    let mut last_text = String::new();
                    let mut failed = false;
                    while let Some(ev) = sub.next().instrument(span.clone()).await {
                        match ev {
                            AgentEvent::RunStarted { .. } => {}
                            AgentEvent::RunCompleted { usage } => total.add(usage),
                            AgentEvent::RunFailed { error } => {
                                failed = true;
                                span.record("otel.status_code", "ERROR");
                                yield AgentEvent::RunFailed { error };
                            }
                            AgentEvent::MessageOutput { item } => {
                                if let Some(t) = assistant_text(&item) {
                                    last_text = t;
                                }
                                yield AgentEvent::MessageOutput { item };
                            }
                            other => yield other,
                        }
                    }

                    if failed {
                        if let Some(e) = failure.take() {
                            parent_failure.set(e);
                        }
                        return;
                    }
                    ctx.state().set(key.clone(), last_text);

                    if actions.is_escalated() {
                        span.record("gen_ai.usage.input_tokens", total.input_tokens as i64);
                        span.record("gen_ai.usage.output_tokens", total.output_tokens as i64);
                        yield AgentEvent::RunCompleted { usage: total };
                        return;
                    }
                }
            }

            let err = AgentError::MaxIterationsExceeded { max: max_iterations };
            let msg = err.to_string();
            parent_failure.set(err);
            span.record("otel.status_code", "ERROR");
            yield AgentEvent::RunFailed { error: msg };
        };

        Ok(Box::pin(stream))
    }
}
