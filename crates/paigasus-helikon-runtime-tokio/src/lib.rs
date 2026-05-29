//! `paigasus-helikon-runtime-tokio` — the default ephemeral Tokio runner.
//!
//! [`TokioRunner`] implements [`paigasus_helikon_core::Runner`] by consuming
//! the agent's [`paigasus_helikon_core::AgentEvent`] stream and adding
//! run-level control (cancellation, timeout, aggregation) at the boundary. It
//! does not own the loop driver — the agent does (see ADR-13).

use async_trait::async_trait;
use paigasus_helikon_core::{
    Agent, AgentInput, RunConfig, RunContext, RunError, RunResult, RunResultStreaming, Runner,
};

/// The default ephemeral execution backend. Stateless.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioRunner;

#[async_trait]
impl<Ctx> Runner<Ctx> for TokioRunner
where
    Ctx: Send + Sync + 'static,
{
    async fn run(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResult, RunError> {
        let ctx = ctx.with_run_config(config);
        let stream = agent.run(ctx, input).await?;
        RunResultStreaming::new(stream).collect().await
    }

    async fn run_streamed(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResultStreaming, RunError> {
        let ctx = ctx.with_run_config(config);
        let stream = agent.run(ctx, input).await?;
        Ok(RunResultStreaming::new(stream))
    }
}
