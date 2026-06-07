//! [`AgentAsTool`] — expose any [`crate::Agent`] as a [`crate::Tool`].
//!
//! The parent agent calls the wrapped agent like any tool, gets its
//! `final_output` back as a [`crate::ToolOutput`], and keeps reasoning. The
//! sub-run is **isolated**: a fresh in-memory session and empty hooks, so its
//! internal turns never touch the parent's session log.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::{
    Agent, AgentInput, HookRegistry, MemorySession, RunContext, RunError, RunResultStreaming, Tool,
    ToolContext, ToolError, ToolOutput,
};

/// Adapter exposing an [`Agent`] as a [`Tool`].
///
/// The sub-run is **isolated**: a fresh in-memory session and empty hooks, so
/// its internal turns never touch the parent's session log. The wrapped agent
/// runs under its **own** [`crate::RunConfig`] — the parent's `run_config`
/// (`max_turns`, `timeout`, `max_agent_depth`) does **not** cross this boundary.
/// Only the nesting depth crosses: the parent-side guard caps entry at the
/// parent's `max_agent_depth`, and the wrapped agent's own config bounds any
/// further nesting it performs, so recursion stays bounded along the whole chain.
pub struct AgentAsTool<Ctx> {
    agent: Arc<dyn Agent<Ctx>>,
    name: String,
    description: String,
    schema: Value,
}

impl<Ctx> AgentAsTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Wrap an owned agent. The tool name and description default to the
    /// agent's own; the argument schema is a single string field `input`.
    pub fn new(agent: impl Agent<Ctx> + 'static) -> Self {
        Self::shared(Arc::new(agent))
    }

    /// Wrap a pre-wrapped agent.
    pub fn shared(agent: Arc<dyn Agent<Ctx>>) -> Self {
        let name = agent.name().to_owned();
        let description = agent.description().to_owned();
        Self {
            agent,
            name,
            description,
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "The request to pass to the wrapped agent."
                    }
                },
                "required": ["input"],
                "additionalProperties": false
            }),
        }
    }

    /// Override the tool name (default: the agent's name).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Override the tool description (default: the agent's description).
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for AgentAsTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> &Value {
        &self.schema
    }

    async fn invoke(&self, ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let input_text =
            args.get("input")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::InvalidArgs {
                    schema_errors: vec!["expected a string field `input`".to_owned()],
                })?;

        // Bound nesting with the same counter the handoff path uses.
        let depth = ctx.agent_depth();
        let max = ctx.max_agent_depth();
        if depth + 1 > max {
            return Err(ToolError::Other(anyhow::Error::from(
                crate::AgentError::MaxAgentDepthExceeded {
                    depth: depth + 1,
                    max,
                },
            )));
        }

        // Isolated sub-context: fresh session + empty hooks; inherit user_ctx,
        // tracer, and the child cancel token; stamp the incremented depth.
        // Security-critical: the parent's permission config (mode, policy,
        // deny rules, approval handler) MUST cross into the sub-run so that a
        // `Plan`/`Bypass`/policy decision applies to the wrapped agent's tools.
        let mut sub_ctx = RunContext::new(
            Arc::clone(ctx.user_ctx()),
            Arc::new(MemorySession::new()),
            HookRegistry::new(),
            ctx.tracer().clone(),
            ctx.cancel().clone(),
        )
        .with_agent_depth(depth + 1)
        .with_permission_mode(ctx.permission_mode())
        .with_deny_rules(ctx.deny_rules.clone());
        if let Some(p) = ctx.permission_policy.clone() {
            sub_ctx = sub_ctx.with_permission_policy(p);
        }
        if let Some(h) = ctx.approval_handler.clone() {
            sub_ctx = sub_ctx.with_approval_handler(h);
        }

        let failure = sub_ctx.failure_handle();
        let stream = self
            .agent
            .run(sub_ctx, AgentInput::from_user_text(input_text))
            .await
            .map_err(|e| ToolError::Other(anyhow::Error::from(e)))?;

        let result = RunResultStreaming::with_failure(stream, failure)
            .collect()
            .await
            .map_err(|e| match e {
                RunError::Agent(a) => ToolError::Other(anyhow::Error::from(a)),
                other => ToolError::Other(anyhow::Error::from(other)),
            })?;

        // Fire OnSubagentStop against the parent's run-level hooks. The sub-run
        // used an isolated (empty) registry; this fires the PARENT's hooks so a
        // run-level OnSubagentStop consumer sees the agent-as-tool sub-run stop.
        let fire_ctx = RunContext::new(
            Arc::clone(ctx.user_ctx()),
            Arc::new(MemorySession::new()),
            HookRegistry::new(),
            ctx.tracer().clone(),
            ctx.cancel().clone(),
        );
        for hook in ctx.hooks.iter() {
            let _ = hook
                .on_event(
                    &fire_ctx,
                    &crate::HookEvent::OnSubagentStop {
                        agent: self.agent.name().to_owned(),
                    },
                )
                .await;
        }

        Ok(ToolOutput::new(Value::String(result.final_output)))
    }
}
