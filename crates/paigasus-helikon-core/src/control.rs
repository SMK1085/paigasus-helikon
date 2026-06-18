//! `Interceptors`: the run's control-layer orchestration unit.
//!
//! Borrows the stream-local Arc-snapshots of the agent's guardrails/hooks and
//! the run's [`RunContext`] (mode, policy, deny rules, approval handler). The
//! driver calls its async methods at the loop's control seams. Pure of the
//! state machine — all async control lives here, not in `transition()`.

use std::sync::Arc;

use crate::{
    ApprovalOutcome, Guardrail, Hook, HookDecision, HookEvent, HookRegistry, PermissionDecision,
    PermissionMode, RunContext, ToolEffect,
};

/// Borrows everything the control seams need for one run.
pub(crate) struct Interceptors<'a, Ctx>
where
    Ctx: Send + Sync + 'static,
{
    pub(crate) ctx: &'a RunContext<Ctx>,
    pub(crate) input_guardrails: &'a [Arc<dyn Guardrail<Ctx>>],
    pub(crate) output_guardrails: &'a [Arc<dyn Guardrail<Ctx>>],
    pub(crate) agent_hooks: &'a [Arc<dyn Hook<Ctx>>],
}

/// The folded outcome of firing all hooks for one event.
#[derive(Debug, Default)]
pub(crate) struct ResolvedHookDecision {
    /// `Some(reason)` if any hook denied (first wins).
    pub(crate) denied: Option<String>,
    /// The last `ReplaceInput`/`ReplaceOutput` value, if any.
    pub(crate) replacement: Option<serde_json::Value>,
    /// All injected system messages, in fire order.
    pub(crate) injections: Vec<String>,
}

impl<'a, Ctx> Interceptors<'a, Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Fire `event` to agent-level hooks first, then the run-level
    /// [`HookRegistry`]. Folds outcomes: first `Deny` short-circuits;
    /// `Replace*` last-writer-wins; `InjectSystemMessage` accumulates.
    pub(crate) async fn fire(&self, event: &HookEvent) -> ResolvedHookDecision {
        let mut out = ResolvedHookDecision::default();
        let registry: &HookRegistry<Ctx> = self.ctx.hooks();
        let all = self.agent_hooks.iter().chain(registry.iter());
        for hook in all {
            match hook.on_event(self.ctx, event).await {
                HookDecision::Allow => {}
                HookDecision::Deny { reason } => {
                    out.denied = Some(reason);
                    return out; // short-circuit
                }
                HookDecision::ReplaceInput { value } | HookDecision::ReplaceOutput { value } => {
                    out.replacement = Some(value);
                }
                HookDecision::InjectSystemMessage { text } => {
                    out.injections.push(text);
                }
            }
        }
        out
    }

    /// Run input guardrails as a blocking gate. Returns `Some((kind, info))`
    /// on the first tripwire, else `None`. A guardrail's own error is treated
    /// as a tripwire with [`crate::GuardrailKind::Other`].
    pub(crate) async fn run_input_guardrails(
        &self,
        text: &str,
    ) -> Option<(crate::GuardrailKind, serde_json::Value)> {
        self.run_guardrails(self.input_guardrails, crate::GuardrailInput::UserText(text))
            .await
    }

    /// Run output guardrails as a blocking gate on the final text.
    pub(crate) async fn run_output_guardrails(
        &self,
        text: &str,
    ) -> Option<(crate::GuardrailKind, serde_json::Value)> {
        self.run_guardrails(
            self.output_guardrails,
            crate::GuardrailInput::ModelOutput(text),
        )
        .await
    }

    async fn run_guardrails(
        &self,
        guardrails: &[Arc<dyn Guardrail<Ctx>>],
        input: crate::GuardrailInput<'_>,
    ) -> Option<(crate::GuardrailKind, serde_json::Value)> {
        for g in guardrails {
            match g.check(self.ctx, input.clone()).await {
                Ok(crate::GuardrailVerdict::Pass) => {}
                Ok(crate::GuardrailVerdict::Tripwire { kind, info }) => return Some((kind, info)),
                Err(e) => {
                    return Some((
                        crate::GuardrailKind::Other {
                            reason: e.to_string(),
                        },
                        serde_json::Value::Null,
                    ))
                }
            }
        }
        None
    }

    /// Authorize one tool call on its effective args: `deny rules › guard rules ›
    /// allow rules › mode › policy › AskUser`. Returns the resolved decision
    /// (never `AskUser` — that is resolved here via the approval handler, default
    /// Deny).
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
        // 1a/1b. Guard rules — built-in destructive defaults (unless opted out)
        // then user guard rules. Run before mode, so they beat Bypass; may Ask.
        let builtin = if self.ctx.default_guards() {
            crate::GuardRule::destructive_defaults()
        } else {
            Vec::new()
        };
        for guard in builtin.iter().chain(self.ctx.guard_rules()) {
            if guard.matches(tool, args) {
                match guard.action() {
                    crate::GuardAction::Deny { reason } => {
                        return PermissionDecision::Deny {
                            reason: reason.clone(),
                        };
                    }
                    crate::GuardAction::Ask { prompt } => {
                        let Some(handler) = self.ctx.approval_handler() else {
                            return PermissionDecision::Deny {
                                reason: format!("destructive command requires approval: {prompt}"),
                            };
                        };
                        // Approval clears THIS guard only — continue the pipeline
                        // so a later guard, mode (e.g. `Plan`), and the policy
                        // still apply. Approval is not a blanket authorization.
                        match handler.decide(tool, prompt, args).await {
                            ApprovalOutcome::Allow => continue,
                            ApprovalOutcome::Deny { reason } => {
                                return PermissionDecision::Deny { reason };
                            }
                        }
                    }
                }
            }
        }
        // 3. Allow rules — positive short-circuit in ANY mode (after deny+guard,
        // before mode). A global per-tool/per-command pre-approval that skips
        // the policy. Deny and guard already ran, so this cannot resurrect a
        // denied/guarded call.
        if self.ctx.allow_rules().iter().any(|r| r.matches(tool, args)) {
            return PermissionDecision::Allow;
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
            PermissionMode::DontAsk => {
                return PermissionDecision::Deny {
                    reason: format!("DontAsk mode: no allow rule matched `{tool}`"),
                };
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
mod guardrail_tests {
    use super::*;
    use crate::{
        CancellationToken, GuardrailError, GuardrailInput, GuardrailKind, GuardrailVerdict,
        HookRegistry, MemorySession, RunContext, Session, TracerHandle,
    };
    use async_trait::async_trait;

    struct TripOnInput;
    #[async_trait]
    impl Guardrail<()> for TripOnInput {
        async fn check(
            &self,
            _: &RunContext<()>,
            _: GuardrailInput<'_>,
        ) -> Result<GuardrailVerdict, GuardrailError> {
            Ok(GuardrailVerdict::Tripwire {
                kind: GuardrailKind::InputPolicy,
                info: serde_json::Value::Null,
            })
        }
    }

    fn ctx() -> RunContext<()> {
        RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn input_guardrail_passes_when_empty() {
        let c = ctx();
        let i = Interceptors {
            ctx: &c,
            input_guardrails: &[],
            output_guardrails: &[],
            agent_hooks: &[],
        };
        assert!(i.run_input_guardrails("hello").await.is_none());
    }

    #[tokio::test]
    async fn input_guardrail_trips() {
        let gs: Vec<Arc<dyn Guardrail<()>>> = vec![Arc::new(TripOnInput)];
        let c = ctx();
        let i = Interceptors {
            ctx: &c,
            input_guardrails: &gs,
            output_guardrails: &[],
            agent_hooks: &[],
        };
        let trip = i.run_input_guardrails("hello").await;
        assert!(matches!(trip, Some((GuardrailKind::InputPolicy, _))));
    }
}

#[cfg(test)]
mod fire_tests {
    use super::*;
    use crate::{
        CancellationToken, HookDecision, HookEvent, HookRegistry, MemorySession, RunContext,
        Session, TracerHandle,
    };
    use async_trait::async_trait;
    use serde_json::json;

    struct FixedHook(HookDecision);
    #[async_trait]
    impl Hook<()> for FixedHook {
        async fn on_event(&self, _: &RunContext<()>, _: &HookEvent) -> HookDecision {
            self.0.clone()
        }
    }

    fn ctx() -> RunContext<()> {
        RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
    }

    fn with_hooks<'a>(
        ctx: &'a RunContext<()>,
        hooks: &'a [Arc<dyn Hook<()>>],
    ) -> Interceptors<'a, ()> {
        Interceptors {
            ctx,
            input_guardrails: &[],
            output_guardrails: &[],
            agent_hooks: hooks,
        }
    }

    #[tokio::test]
    async fn first_deny_short_circuits() {
        let hooks: Vec<Arc<dyn Hook<()>>> = vec![
            Arc::new(FixedHook(HookDecision::Deny {
                reason: "no".into(),
            })),
            Arc::new(FixedHook(HookDecision::ReplaceInput { value: json!(1) })),
        ];
        let c = ctx();
        let i = with_hooks(&c, &hooks);
        let r = i
            .fire(&HookEvent::PreToolUse {
                tool: "t".into(),
                args: json!({}),
            })
            .await;
        assert_eq!(r.denied.as_deref(), Some("no"));
        assert!(
            r.replacement.is_none(),
            "replace hook after deny must not run"
        );
    }

    #[tokio::test]
    async fn last_replace_wins_and_injects_accumulate() {
        let hooks: Vec<Arc<dyn Hook<()>>> = vec![
            Arc::new(FixedHook(HookDecision::ReplaceInput { value: json!(1) })),
            Arc::new(FixedHook(HookDecision::InjectSystemMessage {
                text: "a".into(),
            })),
            Arc::new(FixedHook(HookDecision::ReplaceInput { value: json!(2) })),
            Arc::new(FixedHook(HookDecision::InjectSystemMessage {
                text: "b".into(),
            })),
        ];
        let c = ctx();
        let i = with_hooks(&c, &hooks);
        let r = i
            .fire(&HookEvent::PreToolUse {
                tool: "t".into(),
                args: json!({}),
            })
            .await;
        assert!(r.denied.is_none());
        assert_eq!(r.replacement, Some(json!(2)));
        assert_eq!(r.injections, vec!["a".to_owned(), "b".to_owned()]);
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

    #[tokio::test]
    async fn destructive_guard_denies_under_bypass_without_handler() {
        let c = ctx().with_permission_mode(PermissionMode::Bypass);
        let i = interceptors(&c);
        let args = json!({ "command": "rm -rf /" });
        assert!(matches!(
            i.authorize("Bash", ToolEffect::SideEffect, &args).await,
            PermissionDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn destructive_guard_asks_under_bypass_with_handler() {
        let c = ctx()
            .with_permission_mode(PermissionMode::Bypass)
            .with_approval_handler(Arc::new(AllowHandler));
        let i = interceptors(&c);
        let args = json!({ "command": "rm -rf /" });
        assert!(matches!(
            i.authorize("Bash", ToolEffect::SideEffect, &args).await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn without_default_guards_lets_bypass_allow_destructive() {
        let c = ctx()
            .with_permission_mode(PermissionMode::Bypass)
            .without_default_guards();
        let i = interceptors(&c);
        let args = json!({ "command": "rm -rf /" });
        assert!(matches!(
            i.authorize("Bash", ToolEffect::SideEffect, &args).await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn guard_approval_does_not_bypass_mode() {
        // An approving handler clears the destructive guard, but the rest of the
        // pipeline still runs — `Plan` mode denies the side-effecting tool.
        let c = ctx()
            .with_permission_mode(PermissionMode::Plan)
            .with_approval_handler(Arc::new(AllowHandler));
        let i = interceptors(&c);
        let args = json!({ "command": "rm -rf /" });
        assert!(matches!(
            i.authorize("Bash", ToolEffect::SideEffect, &args).await,
            PermissionDecision::Deny { .. }
        ));
    }

    struct PanicPolicy;
    #[async_trait]
    impl PermissionPolicy<()> for PanicPolicy {
        async fn check(
            &self,
            _: &RunContext<()>,
            _: &str,
            _: &serde_json::Value,
        ) -> PermissionDecision {
            panic!("policy must not be consulted under DontAsk");
        }
    }

    #[tokio::test]
    async fn dont_ask_denies_without_invoking_policy() {
        use crate::AllowRule;
        let c = ctx()
            .with_permission_mode(PermissionMode::DontAsk)
            .with_permission_policy(Arc::new(PanicPolicy))
            .with_allow_rules(vec![AllowRule::tool("Read")]);
        let i = interceptors(&c);
        // allowed tool → Allow (policy never called)
        assert!(matches!(
            i.authorize("Read", ToolEffect::ReadOnly, &json!({"path": "a"}))
                .await,
            PermissionDecision::Allow
        ));
        // unlisted tool → Deny (policy never called → no panic)
        assert!(matches!(
            i.authorize("Bash", ToolEffect::SideEffect, &json!({"command": "ls"}))
                .await,
            PermissionDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn allow_rule_short_circuits_in_default_mode() {
        use crate::AllowRule;
        let c = ctx()
            .with_permission_policy(Arc::new(AskPolicy)) // would otherwise Ask→Deny
            .with_allow_rules(vec![AllowRule::tool("WebSearch")]);
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("WebSearch", ToolEffect::SideEffect, &json!({}))
                .await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn deny_path_beats_bypass() {
        use crate::DenyRule;
        let c = ctx()
            .with_permission_mode(PermissionMode::Bypass)
            .with_deny_rules(vec![DenyRule::read(".env")]);
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("Read", ToolEffect::ReadOnly, &json!({"path": "cfg/.env"}))
                .await,
            PermissionDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn breaker_beats_accept_edits_and_allow_rule() {
        use crate::AllowRule;
        let c = ctx()
            .with_permission_mode(PermissionMode::AcceptEdits)
            .with_allow_rules(vec![AllowRule::edit(".git/**")]); // must NOT override breaker
        let i = interceptors(&c);
        // no approval handler installed → Ask resolves to Deny
        assert!(matches!(
            i.authorize(
                "Write",
                ToolEffect::Write,
                &json!({"path": ".git/config", "content": "x"})
            )
            .await,
            PermissionDecision::Deny { .. }
        ));
    }
}
