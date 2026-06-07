//! `Interceptors`: the run's control-layer orchestration unit.
//!
//! Borrows the stream-local Arc-snapshots of the agent's guardrails/hooks and
//! the run's [`RunContext`] (mode, policy, deny rules, approval handler). The
//! driver calls its async methods at the loop's control seams. Pure of the
//! state machine — all async control lives here, not in `transition()`.

use std::sync::Arc;

use crate::{
    ApprovalOutcome, Guardrail, Hook, PermissionDecision, PermissionMode, RunContext, ToolEffect,
};

/// Borrows everything the control seams need for one run.
// Tasks 7–8 construct this outside `#[cfg(test)]`; suppress until then.
#[allow(dead_code)]
pub(crate) struct Interceptors<'a, Ctx>
where
    Ctx: Send + Sync + 'static,
{
    pub(crate) ctx: &'a RunContext<Ctx>,
    pub(crate) input_guardrails: &'a [Arc<dyn Guardrail<Ctx>>],
    pub(crate) output_guardrails: &'a [Arc<dyn Guardrail<Ctx>>],
    pub(crate) agent_hooks: &'a [Arc<dyn Hook<Ctx>>],
}

impl<'a, Ctx> Interceptors<'a, Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Authorize one tool call on its effective args: `deny rules › mode ›
    /// policy › AskUser`. Returns the resolved decision (never `AskUser` — that
    /// is resolved here via the approval handler, default Deny).
    // Tasks 7–8 call this outside `#[cfg(test)]`; suppress until then.
    #[allow(dead_code)]
    pub(crate) async fn authorize(
        &self,
        tool: &str,
        effect: ToolEffect,
        args: &serde_json::Value,
    ) -> PermissionDecision {
        // 1. Deny rules — absolute, override even Bypass.
        if self.ctx.deny_rules().iter().any(|r| r.matches(tool, args)) {
            return PermissionDecision::Deny {
                reason: format!("denied by deny rule: {tool}"),
            };
        }
        // 2. Mode.
        match self.ctx.permission_mode() {
            PermissionMode::Bypass => return PermissionDecision::Allow,
            PermissionMode::Plan if effect != ToolEffect::ReadOnly => {
                return PermissionDecision::Deny {
                    reason: format!("Plan mode forbids the side-effecting tool `{tool}`"),
                };
            }
            PermissionMode::AcceptEdits if effect == ToolEffect::Write => {
                return PermissionDecision::Allow;
            }
            _ => {}
        }
        // 3. Policy (canUseTool). None ⇒ permissive.
        let decision = match self.ctx.permission_policy() {
            None => return PermissionDecision::Allow,
            Some(policy) => policy.check(self.ctx, tool, args).await,
        };
        // 4. AskUser ⇒ approval handler; None ⇒ Deny.
        match decision {
            PermissionDecision::AskUser { prompt } => match self.ctx.approval_handler() {
                None => PermissionDecision::Deny {
                    reason: "no approval handler installed".to_owned(),
                },
                Some(handler) => match handler.decide(tool, &prompt, args).await {
                    ApprovalOutcome::Allow => PermissionDecision::Allow,
                    ApprovalOutcome::Deny { reason } => PermissionDecision::Deny { reason },
                },
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod authorize_tests {
    use super::*;
    use crate::{
        ApprovalHandler, CancellationToken, DenyRule, HookRegistry, MemorySession,
        PermissionPolicy, Session, TracerHandle,
    };
    use async_trait::async_trait;
    use serde_json::json;

    fn ctx() -> RunContext<()> {
        RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
    }

    fn interceptors<'a>(ctx: &'a RunContext<()>) -> Interceptors<'a, ()> {
        Interceptors {
            ctx,
            input_guardrails: &[],
            output_guardrails: &[],
            agent_hooks: &[],
        }
    }

    struct AllowPolicy;
    #[async_trait]
    impl PermissionPolicy<()> for AllowPolicy {
        async fn check(
            &self,
            _: &RunContext<()>,
            _: &str,
            _: &serde_json::Value,
        ) -> PermissionDecision {
            PermissionDecision::Allow
        }
    }
    struct AskPolicy;
    #[async_trait]
    impl PermissionPolicy<()> for AskPolicy {
        async fn check(
            &self,
            _: &RunContext<()>,
            _: &str,
            _: &serde_json::Value,
        ) -> PermissionDecision {
            PermissionDecision::AskUser {
                prompt: "ok?".into(),
            }
        }
    }
    struct AllowHandler;
    #[async_trait]
    impl ApprovalHandler for AllowHandler {
        async fn decide(&self, _: &str, _: &str, _: &serde_json::Value) -> ApprovalOutcome {
            ApprovalOutcome::Allow
        }
    }

    #[tokio::test]
    async fn deny_rule_beats_bypass() {
        let c = ctx()
            .with_permission_mode(PermissionMode::Bypass)
            .with_deny_rules(vec![DenyRule::tool("rm")]);
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("rm", ToolEffect::ReadOnly, &json!({})).await,
            PermissionDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn plan_denies_non_readonly_allows_readonly() {
        let c = ctx().with_permission_mode(PermissionMode::Plan);
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("write", ToolEffect::Write, &json!({})).await,
            PermissionDecision::Deny { .. }
        ));
        assert!(matches!(
            i.authorize("read", ToolEffect::ReadOnly, &json!({})).await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn accept_edits_allows_write() {
        let c = ctx().with_permission_mode(PermissionMode::AcceptEdits);
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("edit", ToolEffect::Write, &json!({})).await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn default_mode_no_policy_allows() {
        let c = ctx();
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("any", ToolEffect::SideEffect, &json!({})).await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn ask_user_without_handler_denies() {
        let c = ctx().with_permission_policy(Arc::new(AskPolicy));
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("t", ToolEffect::SideEffect, &json!({})).await,
            PermissionDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn ask_user_with_allow_handler_allows() {
        let c = ctx()
            .with_permission_policy(Arc::new(AskPolicy))
            .with_approval_handler(Arc::new(AllowHandler));
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("t", ToolEffect::SideEffect, &json!({})).await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn policy_allow_passes_through() {
        let c = ctx().with_permission_policy(Arc::new(AllowPolicy));
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("t", ToolEffect::SideEffect, &json!({})).await,
            PermissionDecision::Allow
        ));
    }
}
