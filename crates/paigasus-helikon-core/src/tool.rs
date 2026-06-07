//! The [`Tool`] trait and its carrier types.
//!
//! Tools are object-safe by design — applications hold heterogeneous
//! registries as `Vec<Arc<dyn Tool<Ctx>>>`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    ActionsHandle, ApprovalHandler, CancellationToken, DenyRule, PermissionMode, PermissionPolicy,
    SessionState, TracerHandle,
};

/// A tool's side-effect profile. Drives [`crate::PermissionMode`] decisions:
/// `Plan` allows only `ReadOnly`; `AcceptEdits` auto-approves `Write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ToolEffect {
    /// No side effects; safe to run under `Plan` mode.
    ReadOnly,
    /// Mutates local/filesystem state; auto-approved by `AcceptEdits`.
    Write,
    /// Any other side effect (network, external). Safe-by-default.
    #[default]
    SideEffect,
}

/// A tool an agent can call.
///
/// Object-safe by design — applications hold heterogeneous registries as
/// `Vec<Arc<dyn Tool<Ctx>>>`.
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use paigasus_helikon_core::{Tool, ToolContext, ToolError, ToolOutput};
/// use serde_json::{json, Value};
///
/// struct EchoTool {
///     schema: Value,
/// }
///
/// #[async_trait]
/// impl Tool<()> for EchoTool {
///     fn name(&self) -> &str { "echo" }
///     fn description(&self) -> &str { "Returns the input verbatim." }
///     fn schema(&self) -> &Value { &self.schema }
///
///     async fn invoke(
///         &self,
///         _ctx: &ToolContext<()>,
///         args: Value,
///     ) -> Result<ToolOutput, ToolError> {
///         Ok(ToolOutput::new(args))
///     }
/// }
///
/// let _tool = EchoTool {
///     schema: json!({ "type": "object" }),
/// };
/// ```
#[async_trait]
pub trait Tool<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Tool name, unique per registry. Used by the model to address calls.
    fn name(&self) -> &str;
    /// Human-readable description, shown to the model.
    fn description(&self) -> &str;
    /// JSON Schema for the argument payload.
    fn schema(&self) -> &serde_json::Value;
    /// Optional JSON Schema for the return payload. Default is `None`.
    fn output_schema(&self) -> Option<&serde_json::Value> {
        None
    }

    /// This tool's side-effect profile. Default [`ToolEffect::SideEffect`]
    /// (safe-by-default): an undeclared tool is treated as side-effecting, so
    /// `Plan` mode blocks it.
    fn effect(&self) -> ToolEffect {
        ToolEffect::SideEffect
    }

    /// Execute the tool with `args` (a JSON value matching [`Tool::schema`]).
    async fn invoke(
        &self,
        ctx: &ToolContext<Ctx>,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError>;
}

/// Narrower view of [`crate::RunContext`] passed to [`Tool::invoke`].
///
/// Deliberately excludes the session handle and hook registry: tools
/// must not bypass the runner's persistence by writing directly to the
/// session log, and hooks fire *around* tool invocations, not from
/// inside them.
pub struct ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    user_ctx: Arc<Ctx>,
    tracer: TracerHandle,
    cancel: CancellationToken,
    agent_depth: u32,
    max_agent_depth: u32,
    state: SessionState,
    actions: ActionsHandle,
    permission_mode: PermissionMode,
    // read by agent_as_tool in a later task
    pub(crate) permission_policy: Option<Arc<dyn PermissionPolicy<Ctx>>>,
    // read by agent_as_tool in a later task
    pub(crate) deny_rules: Vec<DenyRule>,
    // read by agent_as_tool in a later task
    pub(crate) approval_handler: Option<Arc<dyn ApprovalHandler>>,
}

impl<Ctx> ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct a new [`ToolContext`].
    pub fn new(
        user_ctx: Arc<Ctx>,
        tracer: TracerHandle,
        cancel: CancellationToken,
        agent_depth: u32,
        max_agent_depth: u32,
    ) -> Self {
        Self {
            user_ctx,
            tracer,
            cancel,
            agent_depth,
            max_agent_depth,
            state: SessionState::new(),
            actions: ActionsHandle::new(),
            permission_mode: PermissionMode::Default,
            permission_policy: None,
            deny_rules: Vec::new(),
            approval_handler: None,
        }
    }

    /// Borrow the user context.
    pub fn user_ctx(&self) -> &Arc<Ctx> {
        &self.user_ctx
    }
    /// Borrow the tracer handle.
    pub fn tracer(&self) -> &TracerHandle {
        &self.tracer
    }
    /// Borrow the cancellation token.
    pub fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }
    /// Current agent-nesting depth (handoff + agent-as-tool). `AgentAsTool`
    /// reads this to bound recursion.
    pub fn agent_depth(&self) -> u32 {
        self.agent_depth
    }
    /// The configured maximum agent-nesting depth (from `RunConfig`, or the
    /// default when no runner installed a config).
    pub fn max_agent_depth(&self) -> u32 {
        self.max_agent_depth
    }
    /// Borrow the run-scoped [`SessionState`] shared across sub-agents.
    pub fn state(&self) -> &SessionState {
        &self.state
    }
    /// Borrow the [`ActionsHandle`]. A tool calls `ctx.actions().escalate()`
    /// to stop an enclosing [`crate::LoopAgent`].
    pub fn actions(&self) -> &ActionsHandle {
        &self.actions
    }
    /// Install the shared [`SessionState`] (used by
    /// [`crate::RunContext::to_tool_context`]).
    pub fn with_state(mut self, state: SessionState) -> Self {
        self.state = state;
        self
    }
    /// Install the [`ActionsHandle`] (used by
    /// [`crate::RunContext::to_tool_context`]).
    pub fn with_actions(mut self, actions: ActionsHandle) -> Self {
        self.actions = actions;
        self
    }

    /// The run's permission mode. A tool may legitimately branch on this.
    pub fn permission_mode(&self) -> PermissionMode {
        self.permission_mode
    }

    /// Install the permission config (used by [`crate::RunContext::to_tool_context`]).
    /// `policy`/`deny_rules`/`handler` are `pub(crate)` carriers read only by
    /// the `agent_as_tool` rebuild path — not exposed to tools.
    pub(crate) fn with_permissions(
        mut self,
        mode: PermissionMode,
        policy: Option<Arc<dyn PermissionPolicy<Ctx>>>,
        deny_rules: Vec<DenyRule>,
        handler: Option<Arc<dyn ApprovalHandler>>,
    ) -> Self {
        self.permission_mode = mode;
        self.permission_policy = policy;
        self.deny_rules = deny_rules;
        self.approval_handler = handler;
        self
    }
}

/// The result of a successful [`Tool::invoke`] call.
///
/// Field shape (multi-modal content, metadata) lands with later tickets.
/// Today `content` is the raw JSON value the tool returned.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ToolOutput {
    /// The tool's return payload, as JSON.
    pub content: serde_json::Value,
}

impl ToolOutput {
    /// Construct a [`ToolOutput`] with the given JSON content.
    pub fn new(content: serde_json::Value) -> Self {
        Self { content }
    }
}

/// Errors raised by [`Tool::invoke`].
///
/// `InvalidArgs` is the single recoverable variant per ADR-10: the runner
/// is permitted to feed the schema errors back to the model once before
/// surfacing [`crate::AgentError::InvalidStructuredOutput`]. No other
/// variant is recoverable.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    /// Arguments did not match [`Tool::schema`].
    ///
    /// Recoverable per ADR-10 — the runner may feed `schema_errors` back to
    /// the model once before surfacing
    /// [`crate::AgentError::InvalidStructuredOutput`].
    #[error("invalid tool arguments: {schema_errors:?}")]
    InvalidArgs {
        /// Human-readable schema-validation errors.
        schema_errors: Vec<String>,
    },

    /// Escape hatch for arbitrary tool failures. See ADR-10.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[cfg(test)]
mod effect_tests {
    use crate::ToolEffect;

    #[test]
    fn tool_effect_default_is_side_effect() {
        assert_eq!(ToolEffect::default(), ToolEffect::SideEffect);
    }
}

#[cfg(test)]
mod tool_context_tests {
    use super::ToolContext;
    use crate::{ActionsHandle, CancellationToken, SessionState, TracerHandle};
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn state_and_actions_default_empty() {
        let tc: ToolContext<()> = ToolContext::new(
            Arc::new(()),
            TracerHandle::default(),
            CancellationToken::new(),
            0,
            8,
        );
        assert!(tc.state().get("x").is_none());
        assert!(!tc.actions().is_escalated());
    }

    #[test]
    fn with_state_and_with_actions_project_handles() {
        let state = SessionState::new();
        state.set("k", "v");
        let actions = ActionsHandle::new();
        let tc: ToolContext<()> = ToolContext::new(
            Arc::new(()),
            TracerHandle::default(),
            CancellationToken::new(),
            0,
            8,
        )
        .with_state(state.clone())
        .with_actions(actions.clone());

        assert_eq!(tc.state().get("k"), Some(json!("v")));
        tc.actions().escalate();
        assert!(
            actions.is_escalated(),
            "escalate flows to the shared handle"
        );
    }
}
