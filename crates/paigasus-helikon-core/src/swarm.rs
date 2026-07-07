//! `SwarmAgent` — a pool of `LlmAgent`s with full-mesh handoff tools
//! auto-injected; the first member to produce a final output instead of
//! handing off wins (SMA-333, ADR-11).

use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;
use tracing::Instrument as _;

use crate::workflow::{max_depth, workflow_run_span};
use crate::{Agent, AgentError, AgentEvent, AgentInput, Handoff, LlmAgent, RunContext};

/// Errors from [`SwarmAgentBuilder::build`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SwarmBuildError {
    /// The swarm has no members.
    #[error("swarm has no members")]
    Empty,
    /// `.name(…)` was never called.
    #[error("swarm has no name")]
    MissingName,
    /// Two members share a name (handoff tool names would collide).
    #[error("duplicate swarm member name: {0}")]
    DuplicateMember(String),
    /// `.entry(…)` names an unknown member.
    #[error("unknown swarm entry member: {0}")]
    UnknownEntry(String),
}

/// Adapter standing in for a member inside sibling handoffs. Holds a
/// weak reference so member-to-member wiring cannot form strong `Arc`
/// cycles; the swarm (and each returned run stream) hold the strong ones.
struct MemberSlot<Ctx> {
    name: String,
    description: String,
    target: OnceLock<Weak<dyn Agent<Ctx>>>,
}

#[async_trait]
impl<Ctx> Agent<Ctx> for MemberSlot<Ctx>
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
        let target = self.target.get().and_then(Weak::upgrade).ok_or_else(|| {
            AgentError::Other(anyhow::anyhow!(
                "swarm member '{}' is no longer alive",
                self.name
            ))
        })?;
        // Forward ctx unchanged: the handoff machinery already derived
        // the child context, so the slot adds no depth level.
        target.run(ctx, input).await
    }
}

type MemberInjector<Ctx> = Box<dyn FnOnce(Vec<Handoff<Ctx>>) -> Arc<dyn Agent<Ctx>> + Send>;

/// Builder for [`SwarmAgent`]. Members are added pre-wired; `build()`
/// injects the full-mesh handoffs.
pub struct SwarmAgentBuilder<Ctx> {
    name: Option<String>,
    description: String,
    members: Vec<(String, String, MemberInjector<Ctx>)>,
    entry: Option<String>,
    max_handoffs: Option<u32>,
}

impl<Ctx> SwarmAgentBuilder<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Set the swarm's agent name (required).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the swarm's description.
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = d.into();
        self
    }

    /// Add a member. Only `LlmAgent`s can be members — they are the only
    /// agents that can call the injected `transfer_to_<member>` tools.
    /// Pre-existing handoffs on the member are preserved (appended to).
    pub fn member<M, T>(mut self, agent: LlmAgent<Ctx, M, T>) -> Self
    where
        LlmAgent<Ctx, M, T>: Agent<Ctx> + 'static,
    {
        let name = agent.name.clone();
        let description = agent.description.clone();
        self.members.push((
            name,
            description,
            Box::new(move |handoffs| {
                let mut agent = agent;
                agent.handoffs.extend(handoffs);
                Arc::new(agent) as Arc<dyn Agent<Ctx>>
            }),
        ));
        self
    }

    /// Choose the member that receives the initial input. Defaults to
    /// the first member added.
    pub fn entry(mut self, name: impl Into<String>) -> Self {
        self.entry = Some(name.into());
        self
    }

    /// Bound the number of handoffs before the swarm fails with
    /// [`AgentError::MaxHandoffsExceeded`]. Unset: only the run's
    /// configured maximum agent nesting depth bounds the chain.
    pub fn max_handoffs(mut self, limit: u32) -> Self {
        self.max_handoffs = Some(limit);
        self
    }

    /// Validate and wire the swarm.
    pub fn build(self) -> Result<SwarmAgent<Ctx>, SwarmBuildError> {
        let name = self.name.ok_or(SwarmBuildError::MissingName)?;
        if self.members.is_empty() {
            return Err(SwarmBuildError::Empty);
        }
        let mut seen = std::collections::HashSet::new();
        for (member_name, _, _) in &self.members {
            if !seen.insert(member_name.clone()) {
                return Err(SwarmBuildError::DuplicateMember(member_name.clone()));
            }
        }
        let entry_idx = match &self.entry {
            None => 0,
            Some(e) => self
                .members
                .iter()
                .position(|(n, _, _)| n == e)
                .ok_or_else(|| SwarmBuildError::UnknownEntry(e.clone()))?,
        };

        // 1. One weak slot per member (name/description copied now).
        let slots: Vec<Arc<MemberSlot<Ctx>>> = self
            .members
            .iter()
            .map(|(n, d, _)| {
                Arc::new(MemberSlot {
                    name: n.clone(),
                    description: d.clone(),
                    target: OnceLock::new(),
                })
            })
            .collect();

        // 2. Wire each member with handoffs to every OTHER member's slot.
        let mut members: Vec<Arc<dyn Agent<Ctx>>> = Vec::with_capacity(self.members.len());
        for (i, (_, _, injector)) in self.members.into_iter().enumerate() {
            let handoffs: Vec<Handoff<Ctx>> = slots
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, slot)| Handoff::shared(Arc::clone(slot) as Arc<dyn Agent<Ctx>>))
                .collect();
            members.push(injector(handoffs));
        }

        // 3. Point each slot at its finished member (weak).
        for (slot, member) in slots.iter().zip(&members) {
            let _ = slot.target.set(Arc::downgrade(member));
        }

        Ok(SwarmAgent {
            name,
            description: self.description,
            members,
            entry_idx,
            max_handoffs: self.max_handoffs,
        })
    }
}

/// A pool of `LlmAgent`s with auto-injected full-mesh handoff tools.
/// Execution is a sequential handoff chain; the swarm ends when the
/// active member produces a final output instead of handing off.
pub struct SwarmAgent<Ctx> {
    name: String,
    description: String,
    members: Vec<Arc<dyn Agent<Ctx>>>,
    entry_idx: usize,
    max_handoffs: Option<u32>,
}

impl<Ctx> SwarmAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Start building a swarm.
    pub fn builder() -> SwarmAgentBuilder<Ctx> {
        SwarmAgentBuilder {
            name: None,
            description: String::new(),
            members: Vec::new(),
            entry: None,
            max_handoffs: None,
        }
    }
}

// Manual `Debug`: `Arc<dyn Agent<Ctx>>` doesn't implement `Debug` (the
// `Agent` trait doesn't require it), so `#[derive(Debug)]` isn't available.
// Printing each member's name (rather than the trait object) is enough for
// diagnostics and lets `Result<SwarmAgent<Ctx>, SwarmBuildError>::unwrap_err`
// compile in tests (`unwrap_err` requires the `Ok` type to be `Debug`).
impl<Ctx> std::fmt::Debug for SwarmAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwarmAgent")
            .field("name", &self.name)
            .field("description", &self.description)
            .field(
                "members",
                &self.members.iter().map(|m| m.name()).collect::<Vec<_>>(),
            )
            .field("entry_idx", &self.entry_idx)
            .field("max_handoffs", &self.max_handoffs)
            .finish()
    }
}

#[async_trait]
impl<Ctx> Agent<Ctx> for SwarmAgent<Ctx>
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
        // Strong ownership moves into the stream: a caller may drop the
        // SwarmAgent before draining (`'static` stream contract).
        let members = self.members.clone();
        let entry = Arc::clone(&self.members[self.entry_idx]);
        let max_handoffs = self.max_handoffs;

        let stream = async_stream::stream! {
            let _members_alive = members;
            let parent_failure = ctx.failure_handle();
            let span = workflow_run_span(&name, ctx.tracer());
            yield AgentEvent::RunStarted { agent: name.clone() };

            let max = max_depth(ctx.run_config());
            if ctx.agent_depth() + 1 > max {
                let err = AgentError::MaxAgentDepthExceeded {
                    depth: ctx.agent_depth() + 1,
                    max,
                };
                let msg = err.to_string();
                parent_failure.set(err);
                span.record("otel.status_code", "ERROR");
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            let child = ctx.subagent_child();
            let child_failure = child.failure_handle();
            yield AgentEvent::AgentUpdated { agent: entry.name().to_owned() };

            let mut sub = match entry.run(child, input).instrument(span.clone()).await {
                Ok(s) => s,
                Err(e) => {
                    let msg = e.to_string();
                    parent_failure.set(e);
                    span.record("otel.status_code", "ERROR");
                    yield AgentEvent::RunFailed { error: msg };
                    return;
                }
            };

            let mut hops: u32 = 0;
            let mut failed = false;
            while let Some(ev) = sub.next().instrument(span.clone()).await {
                match ev {
                    AgentEvent::RunStarted { .. } => {}
                    AgentEvent::HandoffItem { from, to } => {
                        hops += 1;
                        if let Some(limit) = max_handoffs {
                            if hops > limit {
                                // The budget-busting handoff is not forwarded.
                                drop(sub);
                                let err = AgentError::MaxHandoffsExceeded { limit };
                                let msg = err.to_string();
                                parent_failure.set(err);
                                span.record("otel.status_code", "ERROR");
                                yield AgentEvent::RunFailed { error: msg };
                                return;
                            }
                        }
                        yield AgentEvent::HandoffItem { from, to };
                    }
                    AgentEvent::RunCompleted { usage } => {
                        span.record("gen_ai.usage.input_tokens", usage.input_tokens as i64);
                        span.record("gen_ai.usage.output_tokens", usage.output_tokens as i64);
                        yield AgentEvent::RunCompleted { usage };
                    }
                    AgentEvent::RunFailed { error } => {
                        failed = true;
                        span.record("otel.status_code", "ERROR");
                        yield AgentEvent::RunFailed { error };
                    }
                    other => yield other,
                }
            }

            if failed {
                if let Some(e) = child_failure.take() {
                    parent_failure.set(e);
                }
            }
            for hook in ctx.hooks().iter() {
                let _ = hook
                    .on_event(&ctx, &crate::HookEvent::OnSubagentStop {
                        agent: entry.name().to_owned(),
                    })
                    .await;
            }
        };

        Ok(Box::pin(stream))
    }
}
