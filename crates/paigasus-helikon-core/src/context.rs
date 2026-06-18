//! Run-scoped context types.
//!
//! [`RunContext`] carries user data, session handle, hook registry, tracer,
//! and cancellation token across the agent loop.

use std::sync::Arc;

use crate::{
    ActionsHandle, ApprovalHandler, DenyRule, FailureSlot, Hook, PermissionMode, PermissionPolicy,
    RunConfig, Session, SessionState, ToolContext,
};

/// Carries the per-run state shared across the agent loop, tools,
/// guardrails, and hooks.
///
/// `RunContext` does **not** implement `Default` — a context without a
/// session handle is meaningless. Construct via [`RunContext::new`].
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use async_trait::async_trait;
/// use paigasus_helikon_core::{
///     CancellationToken, ConversationSnapshot, HookRegistry, RunContext,
///     SequenceId, Session, SessionError, SessionEvent, TracerHandle,
/// };
///
/// struct NoopSession;
/// #[async_trait]
/// impl Session for NoopSession {
///     async fn append(&self, _: &[SessionEvent]) -> Result<(), SessionError> { Ok(()) }
///     async fn events(&self, _: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
///         Ok(Vec::new())
///     }
///     async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
///         Ok(ConversationSnapshot::default())
///     }
/// }
///
/// let _ctx: RunContext<()> = RunContext::new(
///     Arc::new(()),
///     Arc::new(NoopSession),
///     HookRegistry::<()>::new(),
///     TracerHandle::default(),
///     CancellationToken::new(),
/// );
/// ```
pub struct RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    user_ctx: Arc<Ctx>,
    session: Arc<dyn Session>,
    hooks: HookRegistry<Ctx>,
    tracer: TracerHandle,
    cancel: CancellationToken,
    /// Per-invocation execution policy, injected by a `Runner` (e.g.
    /// `TokioRunner`). This is the runner-injection channel, **not** general
    /// context state: it is deliberately NOT surfaced into [`ToolContext`] by
    /// [`RunContext::to_tool_context`]. `None` when an agent is run directly
    /// without a runner.
    run_config: Option<RunConfig>,
    /// Out-of-band carrier for the run's terminal structured [`crate::AgentError`].
    /// Written by [`crate::Agent::run`] at the moment of failure; read at the
    /// boundary by a [`crate::Runner`] / [`crate::RunResultStreaming`]. Like
    /// `run_config`, it is **not** projected into [`ToolContext`].
    failure: FailureSlot,
    /// Agent-nesting depth: 0 for a top-level run, incremented by
    /// [`RunContext::handoff_child`] and by `AgentAsTool` for each nested
    /// agent run. Bounded by [`crate::RunConfig::max_agent_depth`].
    agent_depth: u32,
    /// Run-scoped coordination KV shared across sub-agents (SMA-325). Shared by
    /// `subagent_child` / `handoff_child`; **not** projected as isolated.
    state: SessionState,
    /// Control side-channel a tool writes (e.g. `escalate`). A **fresh** handle
    /// per `subagent_child`, so a `LoopAgent` reads only the current sub-run's signal.
    actions: ActionsHandle,
    /// How the permission layer governs tool calls for this run.
    /// Monotonic on `Bypass`: once set, it cannot be downgraded.
    permission_mode: PermissionMode,
    /// Optional `canUseTool` policy, evaluated after deny rules and mode.
    permission_policy: Option<Arc<dyn PermissionPolicy<Ctx>>>,
    /// Deny rules evaluated before mode (override even `Bypass`).
    deny_rules: Vec<DenyRule>,
    /// Resolves `AskUser` decisions; `None` → deny by default.
    approval_handler: Option<Arc<dyn ApprovalHandler>>,
    /// User-supplied guard rules, evaluated before mode (can Ask or Deny).
    guard_rules: Vec<crate::GuardRule>,
    /// Whether the always-on destructive default guards are consulted.
    default_guards: bool,
    /// Whether tool output is redacted before re-entering context.
    redact_output: bool,
    /// Extra secret values to redact, beyond the auto-sourced env set.
    extra_secrets: Vec<String>,
}

impl<Ctx> RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct a new [`RunContext`].
    pub fn new(
        user_ctx: Arc<Ctx>,
        session: Arc<dyn Session>,
        hooks: HookRegistry<Ctx>,
        tracer: TracerHandle,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            user_ctx,
            session,
            hooks,
            tracer,
            cancel,
            run_config: None,
            failure: FailureSlot::new(),
            agent_depth: 0,
            state: SessionState::new(),
            actions: ActionsHandle::new(),
            permission_mode: PermissionMode::Default,
            permission_policy: None,
            deny_rules: Vec::new(),
            approval_handler: None,
            guard_rules: Vec::new(),
            default_guards: true,
            redact_output: true,
            extra_secrets: Vec::new(),
        }
    }

    /// Borrow the user context.
    pub fn user_ctx(&self) -> &Arc<Ctx> {
        &self.user_ctx
    }
    /// Borrow the session handle.
    pub fn session(&self) -> &Arc<dyn Session> {
        &self.session
    }
    /// Borrow the hook registry.
    pub fn hooks(&self) -> &HookRegistry<Ctx> {
        &self.hooks
    }
    /// Borrow the tracer handle.
    pub fn tracer(&self) -> &TracerHandle {
        &self.tracer
    }
    /// Borrow the cancellation token.
    pub fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Borrow the run-scoped [`SessionState`] shared across sub-agents.
    pub fn state(&self) -> &SessionState {
        &self.state
    }
    /// Borrow the [`ActionsHandle`] for this (sub-)run.
    pub fn actions(&self) -> &ActionsHandle {
        &self.actions
    }

    /// Borrow the per-invocation [`RunConfig`], if a runner installed one.
    pub fn run_config(&self) -> Option<&RunConfig> {
        self.run_config.as_ref()
    }

    /// Clone the handle to this run's [`FailureSlot`].
    ///
    /// A [`crate::Runner`] clones this **before** moving the context into
    /// [`crate::Agent::run`] (the same way it clones `cancel` / `session`), then
    /// reads the structured error after the run's event stream drains.
    pub fn failure_handle(&self) -> FailureSlot {
        self.failure.clone()
    }

    /// Nesting depth that produced this context (0 at top level).
    pub fn agent_depth(&self) -> u32 {
        self.agent_depth
    }

    /// Stamp an explicit nesting depth. Used by `AgentAsTool` when it builds
    /// the isolated sub-context for its wrapped agent.
    pub fn with_agent_depth(mut self, depth: u32) -> Self {
        self.agent_depth = depth;
        self
    }

    /// Install the run-scoped [`SessionState`]. Used by `AgentAsTool` to build
    /// the fire-only context for `OnSubagentStop` so hooks observe the parent's
    /// shared `state` rather than a fresh one.
    pub(crate) fn with_state(mut self, state: SessionState) -> Self {
        self.state = state;
        self
    }

    /// A context for a handed-off sub-run. A handoff *continues the same
    /// logical run*, so the child **shares** session, hooks, cancel token,
    /// failure slot, and run config (including per-invocation limits like
    /// `max_turns` / `timeout`) — with `agent_depth` incremented by one.
    /// (Distinct from `AgentAsTool`, which builds an isolated context.)
    ///
    /// The increment saturates: depth is bounded far below `u32::MAX` by the
    /// driver's `max_agent_depth` guard, so saturating is purely defensive.
    pub fn handoff_child(&self) -> Self {
        Self {
            user_ctx: Arc::clone(&self.user_ctx),
            session: Arc::clone(&self.session),
            hooks: self.hooks.clone(),
            tracer: self.tracer.clone(),
            cancel: self.cancel.clone(),
            run_config: self.run_config.clone(),
            failure: self.failure.clone(),
            agent_depth: self.agent_depth.saturating_add(1),
            state: self.state.clone(),
            actions: self.actions.clone(),
            permission_mode: self.permission_mode,
            permission_policy: self.permission_policy.clone(),
            deny_rules: self.deny_rules.clone(),
            approval_handler: self.approval_handler.clone(),
            guard_rules: self.guard_rules.clone(),
            default_guards: self.default_guards,
            redact_output: self.redact_output,
            extra_secrets: self.extra_secrets.clone(),
        }
    }

    /// A context for one sub-agent of a workflow agent (`SequentialAgent`,
    /// `ParallelAgent`, `LoopAgent`). **Shares** the run-scoped `state`, session,
    /// cancel token, tracer, user context, and run config; gets a **fresh**
    /// `FailureSlot` and a **fresh** `ActionsHandle` (so the workflow agent reads
    /// only this sub-run's failure / escalate); `agent_depth` incremented by one.
    pub fn subagent_child(&self) -> Self {
        Self {
            user_ctx: Arc::clone(&self.user_ctx),
            session: Arc::clone(&self.session),
            hooks: self.hooks.clone(),
            tracer: self.tracer.clone(),
            cancel: self.cancel.clone(),
            run_config: self.run_config.clone(),
            failure: FailureSlot::new(),
            agent_depth: self.agent_depth.saturating_add(1),
            state: self.state.clone(),
            actions: ActionsHandle::new(),
            permission_mode: self.permission_mode,
            permission_policy: self.permission_policy.clone(),
            deny_rules: self.deny_rules.clone(),
            approval_handler: self.approval_handler.clone(),
            guard_rules: self.guard_rules.clone(),
            default_guards: self.default_guards,
            redact_output: self.redact_output,
            extra_secrets: self.extra_secrets.clone(),
        }
    }

    /// Install the per-invocation [`RunConfig`] (consuming builder). A
    /// [`crate::Runner`] calls this before [`crate::Agent::run`].
    pub fn with_run_config(mut self, config: RunConfig) -> Self {
        self.run_config = Some(config);
        self
    }

    /// Set the permission mode. **Monotonic on `Bypass`:** once the mode is
    /// `Bypass`, this is a no-op — `Bypass` cannot be downgraded (the safety
    /// invariant). All other transitions apply.
    pub fn with_permission_mode(mut self, mode: PermissionMode) -> Self {
        if self.permission_mode != PermissionMode::Bypass {
            self.permission_mode = mode;
        }
        self
    }

    /// Install the run's permission policy (`canUseTool`).
    pub fn with_permission_policy(mut self, policy: Arc<dyn PermissionPolicy<Ctx>>) -> Self {
        self.permission_policy = Some(policy);
        self
    }

    /// Install deny rules, evaluated before mode (override even `Bypass`).
    pub fn with_deny_rules(mut self, rules: Vec<DenyRule>) -> Self {
        self.deny_rules = rules;
        self
    }

    /// Install the approval handler that resolves `AskUser` decisions.
    pub fn with_approval_handler(mut self, handler: Arc<dyn ApprovalHandler>) -> Self {
        self.approval_handler = Some(handler);
        self
    }

    /// The current permission mode.
    pub fn permission_mode(&self) -> PermissionMode {
        self.permission_mode
    }

    /// The run's permission policy, if installed.
    pub fn permission_policy(&self) -> Option<&Arc<dyn PermissionPolicy<Ctx>>> {
        self.permission_policy.as_ref()
    }

    /// The run's deny rules.
    pub fn deny_rules(&self) -> &[DenyRule] {
        &self.deny_rules
    }

    /// The run's approval handler, if installed.
    pub fn approval_handler(&self) -> Option<&Arc<dyn ApprovalHandler>> {
        self.approval_handler.as_ref()
    }

    /// Install user guard rules (evaluated before mode; can Ask or Deny).
    pub fn with_guard_rules(mut self, rules: Vec<crate::GuardRule>) -> Self {
        self.guard_rules = rules;
        self
    }

    /// Disable the always-on built-in destructive guard set (power-user opt-out).
    pub fn without_default_guards(mut self) -> Self {
        self.default_guards = false;
        self
    }

    /// Disable automatic secret redaction of tool output.
    pub fn without_output_redaction(mut self) -> Self {
        self.redact_output = false;
        self
    }

    /// Add extra secret values to redact from tool output. Additive: chained
    /// calls accumulate (earlier secrets are never dropped).
    pub fn with_extra_secrets(mut self, secrets: Vec<String>) -> Self {
        self.extra_secrets.extend(secrets);
        self
    }

    /// The run's user guard rules.
    pub fn guard_rules(&self) -> &[crate::GuardRule] {
        &self.guard_rules
    }

    /// Whether built-in destructive guards are active.
    pub fn default_guards(&self) -> bool {
        self.default_guards
    }

    /// Whether tool-output redaction is active.
    pub fn redact_output(&self) -> bool {
        self.redact_output
    }

    /// Extra secret values to redact.
    pub fn extra_secrets(&self) -> &[String] {
        &self.extra_secrets
    }

    /// Clone the permission/guard/redaction config into a [`crate::tool::PermissionFields`]
    /// bundle for projection into a [`ToolContext`].
    pub(crate) fn clone_permission_fields(&self) -> crate::tool::PermissionFields<Ctx> {
        crate::tool::PermissionFields {
            mode: self.permission_mode,
            policy: self.permission_policy.clone(),
            deny_rules: self.deny_rules.clone(),
            approval_handler: self.approval_handler.clone(),
            guard_rules: self.guard_rules.clone(),
            default_guards: self.default_guards,
            redact_output: self.redact_output,
            extra_secrets: self.extra_secrets.clone(),
        }
    }

    /// Project the narrower [`ToolContext`] from this [`RunContext`].
    ///
    /// Tools receive `user_ctx`, `tracer`, and a **child** cancellation
    /// token. The child observes the parent's cancellation but tool-side
    /// `cancel()` calls only cancel the tool's subtree — they do not
    /// propagate back to the run.
    ///
    /// Tools do not see the session handle (the runner owns persistence). The
    /// run-level hook registry is projected as a `pub(crate)` carrier (for
    /// `agent_as_tool` to fire `OnSubagentStop`), but is not exposed to `Tool`
    /// impls — hooks fire around tool invocations, not from inside.
    pub fn to_tool_context(&self) -> ToolContext<Ctx> {
        let max_agent_depth = self
            .run_config
            .as_ref()
            .map(|c| c.max_agent_depth)
            .unwrap_or_else(|| RunConfig::default().max_agent_depth);
        ToolContext::new(
            Arc::clone(&self.user_ctx),
            self.tracer.clone(),
            self.cancel.child_token(),
            self.agent_depth,
            max_agent_depth,
        )
        .with_state(self.state.clone())
        .with_actions(self.actions.clone())
        .with_hooks(self.hooks.clone())
        .with_permissions(self.clone_permission_fields())
    }
}

#[cfg(test)]
mod runcontext_tests {
    use super::*;
    use crate::{MemorySession, RunConfig};
    use std::sync::Arc;

    #[test]
    fn failure_handle_shares_the_context_slot() {
        use crate::AgentError;
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            crate::HookRegistry::new(),
            crate::TracerHandle::default(),
            crate::CancellationToken::new(),
        );
        let handle = ctx.failure_handle();
        handle.set(AgentError::MaxTurnsExceeded(2));
        // A second handle from the same ctx observes the write.
        assert!(matches!(
            ctx.failure_handle().take(),
            Some(AgentError::MaxTurnsExceeded(2))
        ));
    }

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
        assert_eq!(h.tags(), &["prod", "beta"]);
    }

    #[test]
    fn with_run_config_round_trips_and_defaults_none() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        );
        assert!(ctx.run_config().is_none());

        let ctx =
            ctx.with_run_config(RunConfig::new().with_timeout(std::time::Duration::from_secs(1)));
        assert_eq!(
            ctx.run_config().unwrap().timeout,
            Some(std::time::Duration::from_secs(1))
        );
    }

    #[test]
    fn handoff_child_increments_depth_and_shares_failure_slot() {
        use crate::AgentError;
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        );
        assert_eq!(ctx.agent_depth(), 0);

        let child = ctx.handoff_child();
        assert_eq!(child.agent_depth(), 1);
        assert_eq!(child.handoff_child().agent_depth(), 2);

        // The child shares the parent's failure slot (so a failing target
        // reaches the parent's boundary).
        child.failure_handle().set(AgentError::MaxTurnsExceeded(2));
        assert!(matches!(
            ctx.failure_handle().take(),
            Some(AgentError::MaxTurnsExceeded(2))
        ));
    }

    #[test]
    fn with_agent_depth_sets_depth() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .with_agent_depth(5);
        assert_eq!(ctx.agent_depth(), 5);
    }

    #[test]
    fn to_tool_context_projects_depth_and_max() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .with_agent_depth(2)
        .with_run_config(RunConfig::new().with_max_agent_depth(5));

        let tc = ctx.to_tool_context();
        assert_eq!(tc.agent_depth(), 2);
        assert_eq!(tc.max_agent_depth(), 5);
    }

    #[test]
    fn to_tool_context_projects_state_and_actions() {
        use serde_json::json;
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        );
        ctx.state().set("k", "v");
        let tc = ctx.to_tool_context();
        assert_eq!(tc.state().get("k"), Some(json!("v")));
        tc.actions().escalate();
        assert!(
            ctx.actions().is_escalated(),
            "tool escalate reaches the run"
        );
    }

    #[test]
    fn permission_mode_defaults_to_default_and_setter_round_trips() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        );
        assert_eq!(ctx.permission_mode(), crate::PermissionMode::Default);
        let ctx = ctx.with_permission_mode(crate::PermissionMode::Plan);
        assert_eq!(ctx.permission_mode(), crate::PermissionMode::Plan);
    }

    #[test]
    fn bypass_cannot_be_downgraded() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .with_permission_mode(crate::PermissionMode::Bypass)
        .with_permission_mode(crate::PermissionMode::Plan); // no-op
        assert_eq!(ctx.permission_mode(), crate::PermissionMode::Bypass);
    }

    #[test]
    fn handoff_child_inherits_mode_and_keeps_bypass_sticky() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .with_permission_mode(crate::PermissionMode::Bypass);
        assert_eq!(
            ctx.handoff_child().permission_mode(),
            crate::PermissionMode::Bypass
        );
        assert_eq!(
            ctx.subagent_child().permission_mode(),
            crate::PermissionMode::Bypass
        );
    }

    #[test]
    fn to_tool_context_projects_permission_mode() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .with_permission_mode(crate::PermissionMode::Bypass);
        assert_eq!(
            ctx.to_tool_context().permission_mode(),
            crate::PermissionMode::Bypass
        );
    }

    #[test]
    fn guard_rules_default_on_and_inherit_through_children() {
        use crate::{GuardRule, PermissionMode};
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .with_permission_mode(PermissionMode::Bypass)
        .with_guard_rules(vec![GuardRule::destructive_defaults().remove(0)]);

        assert!(ctx.default_guards());
        assert_eq!(ctx.guard_rules().len(), 1);
        assert_eq!(ctx.handoff_child().guard_rules().len(), 1);
        assert_eq!(ctx.subagent_child().guard_rules().len(), 1);
        assert_eq!(ctx.to_tool_context().guard_rules().len(), 1);
        assert!(ctx.handoff_child().default_guards());
    }

    #[test]
    fn without_default_guards_disables_builtins() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .without_default_guards();
        assert!(!ctx.default_guards());
        assert!(!ctx.subagent_child().default_guards());
    }

    #[test]
    fn subagent_child_shares_state_fresh_actions_increments_depth() {
        use serde_json::json;
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        );
        ctx.state().set("k", "v");
        ctx.actions().escalate();

        let child = ctx.subagent_child();
        assert_eq!(child.agent_depth(), 1);
        assert_eq!(child.state().get("k"), Some(json!("v")), "state is shared");
        assert!(!child.actions().is_escalated(), "actions slot is fresh");

        child.state().set("k2", "v2");
        assert_eq!(ctx.state().get("k2"), Some(json!("v2")), "shared store");

        use crate::AgentError;
        child.failure_handle().set(AgentError::MaxTurnsExceeded(1));
        assert!(ctx.failure_handle().take().is_none(), "fresh failure slot");
    }
}

/// Re-export of [`tokio_util::sync::CancellationToken`] so downstream
/// crates need not depend on `tokio-util` directly.
pub use tokio_util::sync::CancellationToken;

/// Registry of hooks active for one run.
///
/// Today the surface is intentionally minimal — just `new`, `push`,
/// `iter`, and `is_empty`. The agent-loop ticket grows this when it
/// needs per-event filtering.
pub struct HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    hooks: Vec<Arc<dyn Hook<Ctx>>>,
}

impl<Ctx> HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Register a hook.
    pub fn push(&mut self, hook: Arc<dyn Hook<Ctx>>) {
        self.hooks.push(hook);
    }

    /// Iterate over registered hooks in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Hook<Ctx>>> {
        self.hooks.iter()
    }

    /// `true` if no hooks are registered.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

impl<Ctx> Default for HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Ctx> Clone for HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            hooks: self.hooks.clone(),
        }
    }
}

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
