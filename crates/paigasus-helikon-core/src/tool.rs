//! The [`Tool`] trait and its carrier types.
//!
//! Tools are object-safe by design — applications hold heterogeneous
//! registries as `Vec<Arc<dyn Tool<Ctx>>>`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    ActionsHandle, AllowRule, ApprovalHandler, CancellationToken, DenyRule, HookRegistry,
    PermissionMode, PermissionPolicy, SessionState, TracerHandle,
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

/// Bundle of the run's permission/guard/redaction config, projected from
/// [`crate::RunContext`] into [`ToolContext`]. A struct (not a long positional
/// arg list) so same-typed fields like `default_guards`/`redact_output` can't be
/// transposed silently.
pub(crate) struct PermissionFields<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    pub(crate) mode: PermissionMode,
    pub(crate) policy: Option<Arc<dyn PermissionPolicy<Ctx>>>,
    pub(crate) deny_rules: Vec<DenyRule>,
    pub(crate) allow_rules: Vec<AllowRule>,
    pub(crate) approval_handler: Option<Arc<dyn ApprovalHandler>>,
    pub(crate) guard_rules: Vec<crate::GuardRule>,
    pub(crate) default_guards: bool,
    pub(crate) redact_output: bool,
    pub(crate) extra_secrets: Vec<String>,
}

/// Narrower view of [`crate::RunContext`] passed to [`Tool::invoke`].
///
/// Deliberately excludes the session handle: tools must not bypass the
/// runner's persistence by writing directly to the session log. The run-level
/// hook registry rides along as a `pub(crate)` carrier (used only by
/// `agent_as_tool` to fire `OnSubagentStop`); it is **not** exposed to `Tool`
/// impls — hooks fire *around* tool invocations, not from inside them.
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
    /// Carrier for the parent run's [`HookRegistry`], projected by
    /// [`crate::RunContext::to_tool_context`]. `pub(crate)` — read only by the
    /// `agent_as_tool` path to fire `OnSubagentStop`; not exposed to tools.
    pub(crate) hooks: HookRegistry<Ctx>,
    permission_mode: PermissionMode,
    // read by agent_as_tool in a later task
    pub(crate) permission_policy: Option<Arc<dyn PermissionPolicy<Ctx>>>,
    // read by agent_as_tool in a later task
    pub(crate) deny_rules: Vec<DenyRule>,
    /// Carrier: allow rules from the parent [`crate::RunContext`].
    pub(crate) allow_rules: Vec<AllowRule>,
    // read by agent_as_tool in a later task
    pub(crate) approval_handler: Option<Arc<dyn ApprovalHandler>>,
    /// Carrier: user guard rules from the parent [`crate::RunContext`].
    pub(crate) guard_rules: Vec<crate::GuardRule>,
    /// Carrier: whether built-in destructive guards are active.
    pub(crate) default_guards: bool,
    /// Carrier: whether tool-output redaction is active.
    pub(crate) redact_output: bool,
    /// Carrier: extra secret values to redact from tool output.
    pub(crate) extra_secrets: Vec<String>,
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
            hooks: HookRegistry::new(),
            permission_mode: PermissionMode::Default,
            permission_policy: None,
            deny_rules: Vec::new(),
            allow_rules: Vec::new(),
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

    /// Install the run-level hook registry (used by
    /// [`crate::RunContext::to_tool_context`]). `pub(crate)` — read only by the
    /// `agent_as_tool` path to fire `OnSubagentStop`; not exposed to tools.
    pub(crate) fn with_hooks(mut self, hooks: HookRegistry<Ctx>) -> Self {
        self.hooks = hooks;
        self
    }

    /// The run's permission mode. A tool may legitimately branch on this.
    pub fn permission_mode(&self) -> PermissionMode {
        self.permission_mode
    }

    /// Install the permission/guard/redaction config (used by
    /// [`crate::RunContext::to_tool_context`]). The policy/deny/guard/handler
    /// fields are `pub(crate)` carriers read only by the `agent_as_tool` rebuild
    /// path — not exposed to tools.
    pub(crate) fn with_permissions(mut self, fields: PermissionFields<Ctx>) -> Self {
        self.permission_mode = fields.mode;
        self.permission_policy = fields.policy;
        self.deny_rules = fields.deny_rules;
        self.allow_rules = fields.allow_rules;
        self.approval_handler = fields.approval_handler;
        self.guard_rules = fields.guard_rules;
        self.default_guards = fields.default_guards;
        self.redact_output = fields.redact_output;
        self.extra_secrets = fields.extra_secrets;
        self
    }

    /// The run's user guard rules (carrier for `agent_as_tool` rebuild).
    pub fn guard_rules(&self) -> &[crate::GuardRule] {
        &self.guard_rules
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

    /// The tool refused the operation: either a hard safety-boundary violation
    /// (a path outside the sandbox root, a non-UTF-8 read) or an unsatisfiable
    /// precondition (an ambiguous edit target, an allow/deny-blocked command).
    /// Distinct from a [`crate::PermissionPolicy`] denial, which the runner
    /// resolves before `invoke` is ever called. Not recoverable.
    #[error("operation denied: {reason}")]
    Denied {
        /// Human-readable denial reason, surfaced to the model.
        reason: String,
    },

    /// Escape hatch for arbitrary tool failures. See ADR-10.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[cfg(test)]
mod denied_variant_tests {
    use super::ToolError;

    #[test]
    fn denied_displays_reason() {
        let e = ToolError::Denied {
            reason: "path escapes the sandbox root".to_owned(),
        };
        assert_eq!(
            e.to_string(),
            "operation denied: path escapes the sandbox root"
        );
    }
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
