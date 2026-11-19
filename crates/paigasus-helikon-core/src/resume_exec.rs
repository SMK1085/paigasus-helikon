//! Execution of the resolved deferred tool calls of a resume.
//!
//! The agent runs these calls before its first turn. They take the normal
//! tool-execution path without `PreToolUse`: a `Denied` call gets the denial
//! result, and an `Approved` call runs after the permission re-check.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use tracing::Instrument as _;

use crate::context::{AskPoints, Authorization};
use crate::control::Interceptors;
use crate::deferral::PlannedCall;
use crate::{
    AgentEvent, ContentPart, HookEvent, Item, RunContext, Tool, ToolCallOutcome, ToolContext,
    ToolResolution,
};

/// The synthetic result of a resolved call that did not run, because the
/// run ended before the resume executed it.
const NOT_RUN: &str = "tool call did not run: the run ended before the resume completed";

/// Re-check an approved deferred call (spec §4.8). The ask points count as
/// answered; the deny rules, guards, allow rules, mode and policy run again.
/// Returns `Err(reason)` when the call must be denied.
pub(crate) async fn recheck_approved<Ctx: Send + Sync + 'static>(
    ctx: &RunContext<Ctx>,
    tool: Option<&Arc<dyn Tool<Ctx>>>,
    name: &str,
    recorded_args: &serde_json::Value,
) -> Result<(), String> {
    let Some(tool) = tool else {
        return Err(format!("tool no longer available: {name}"));
    };
    match ctx
        .authorize_tool_detailed(name, tool.effect(), recorded_args, AskPoints::Answered)
        .await
    {
        Authorization::Allow => Ok(()),
        Authorization::Replace { args } if args == *recorded_args => Ok(()),
        Authorization::Replace { .. } => Err("arguments changed on re-check".to_owned()),
        Authorization::Deny { reason } => Err(reason),
        // Unreachable with `AskPoints::Answered`; fail closed.
        Authorization::Defer { .. } => Err("approval deferred again on re-check".to_owned()),
    }
}

/// Run the resolved calls of a resume. Returns one outcome per call, in the
/// order of `calls`, and the `PermissionDenied` events.
pub(crate) async fn run_resolved_calls<Ctx: Send + Sync + 'static>(
    tools: &[Arc<dyn Tool<Ctx>>],
    calls: &[PlannedCall],
    interceptors: &Interceptors<'_, Ctx>,
    tool_ctx: &ToolContext<Ctx>,
    limit: Option<NonZeroUsize>,
    parent: &tracing::Span,
) -> (Vec<ToolCallOutcome>, Vec<AgentEvent>) {
    let denied_events: Mutex<Vec<AgentEvent>> = Mutex::new(Vec::new());
    let ctx = interceptors.ctx;
    let redact_output = ctx.redact_output();
    let secrets = crate::redaction::SecretSet::from_env_and_extra(ctx.extra_secrets());

    let futures = calls.iter().map(|call| {
        let name = call.pending.tool().to_owned();
        let call_id = call.pending.call_id().to_owned();
        let args = call.pending.args().clone();
        let tool = tools.iter().find(|t| t.name() == name).cloned();
        let denied_events = &denied_events;
        let secrets = &secrets;
        let span = tracing::info_span!(
            target: "paigasus::core::agent",
            parent: parent,
            "tool.execute",
            otel.name = tracing::field::Empty,
            otel.kind = "internal",
            gen_ai.operation.name = "execute_tool",
            gen_ai.tool.name = %name,
            paigasus.tool.resumed = true,
            otel.status_code = tracing::field::Empty,
        );
        span.record("otel.name", format!("execute_tool {name}").as_str());
        async move {
            let deny = |reason: String| {
                denied_events
                    .lock()
                    .unwrap()
                    .push(AgentEvent::PermissionDenied {
                        tool: name.clone(),
                        reason: reason.clone(),
                    });
                ToolCallOutcome {
                    call_id: call_id.clone(),
                    result: Err(format!("permission denied: {reason}")),
                }
            };
            let tool = match &call.resolution {
                ToolResolution::Denied { reason } => return deny(reason.clone()),
                ToolResolution::Approved => {
                    match recheck_approved(ctx, tool.as_ref(), &name, &args).await {
                        Err(reason) => return deny(reason),
                        Ok(()) => match tool {
                            Some(tool) => tool,
                            None => return deny(format!("tool no longer available: {name}")),
                        },
                    }
                }
            };

            let result = match tool.invoke(tool_ctx, args).await {
                Ok(output) => {
                    let post = interceptors
                        .fire(&HookEvent::PostToolUse {
                            tool: name.clone(),
                            output: output.content.clone(),
                        })
                        .await;
                    match post.denied {
                        Some(reason) => Err(format!("blocked by PostToolUse hook: {reason}")),
                        None => {
                            let final_json = post.replacement.unwrap_or(output.content);
                            // Redaction is the final transform, after the hooks.
                            Ok(crate::finalize_tool_output(
                                final_json,
                                redact_output,
                                secrets,
                            ))
                        }
                    }
                }
                Err(e) => {
                    tracing::Span::current().record("otel.status_code", "ERROR");
                    Err(e.to_string())
                }
            };
            ToolCallOutcome { call_id, result }
        }
        .instrument(span)
    });

    let outcomes = match limit {
        None => futures_util::future::join_all(futures).await,
        Some(n) => {
            use futures_util::stream::StreamExt as _;
            let collected: Vec<_> = futures.collect();
            futures_util::stream::iter(collected)
                .buffered(n.get())
                .collect()
                .await
        }
    };
    (outcomes, denied_events.into_inner().unwrap())
}

/// One `ToolOutputItem` per call with the synthetic "did not run" result.
pub(crate) fn not_run_results(calls: &[PlannedCall]) -> Vec<AgentEvent> {
    calls
        .iter()
        .map(|call| AgentEvent::ToolOutputItem {
            item: Item::ToolResult {
                call_id: call.pending.call_id().to_owned(),
                content: vec![ContentPart::Text {
                    text: NOT_RUN.to_owned(),
                }],
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AllowRule, ApprovalHandler, ApprovalOutcome, PermissionDecision, PermissionPolicy,
        ToolError, ToolOutput,
    };
    use async_trait::async_trait;
    use serde_json::json;

    struct Pay;

    #[async_trait]
    impl Tool<()> for Pay {
        fn name(&self) -> &str {
            "pay"
        }
        fn description(&self) -> &str {
            "pays"
        }
        fn schema(&self) -> &serde_json::Value {
            use std::sync::OnceLock;
            static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
            SCHEMA.get_or_init(|| json!({ "type": "object" }))
        }
        async fn invoke(
            &self,
            _ctx: &ToolContext<()>,
            _args: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::new(json!({ "paid": true })))
        }
    }

    /// Replaces the args with a fixed value.
    struct ReplaceWith(serde_json::Value);

    #[async_trait]
    impl PermissionPolicy<()> for ReplaceWith {
        async fn check(
            &self,
            _: &RunContext<()>,
            _: &str,
            _: &serde_json::Value,
        ) -> PermissionDecision {
            PermissionDecision::Replace {
                args: self.0.clone(),
            }
        }
    }

    struct DeferPolicy;

    #[async_trait]
    impl PermissionPolicy<()> for DeferPolicy {
        async fn check(
            &self,
            _: &RunContext<()>,
            _: &str,
            _: &serde_json::Value,
        ) -> PermissionDecision {
            PermissionDecision::Defer {
                prompt: "wait".to_owned(),
            }
        }
    }

    struct PanicHandler;

    #[async_trait]
    impl ApprovalHandler for PanicHandler {
        async fn decide(&self, _: &str, _: &str, _: &serde_json::Value) -> ApprovalOutcome {
            panic!("the re-check must not call the approval handler")
        }
    }

    fn pay() -> Arc<dyn Tool<()>> {
        Arc::new(Pay)
    }

    fn args() -> serde_json::Value {
        json!({ "amount": 5 })
    }

    #[tokio::test]
    async fn recheck_denies_a_missing_tool() {
        let ctx = RunContext::ephemeral(());
        assert_eq!(
            recheck_approved(&ctx, None, "pay", &args()).await,
            Err("tool no longer available: pay".to_owned())
        );
    }

    #[tokio::test]
    async fn recheck_passes_replace_with_equal_args() {
        let ctx = RunContext::ephemeral(()).with_permission_policy(Arc::new(ReplaceWith(args())));
        assert_eq!(
            recheck_approved(&ctx, Some(&pay()), "pay", &args()).await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn recheck_denies_replace_with_different_args() {
        let ctx = RunContext::ephemeral(())
            .with_permission_policy(Arc::new(ReplaceWith(json!({ "amount": 500 }))));
        assert_eq!(
            recheck_approved(&ctx, Some(&pay()), "pay", &args()).await,
            Err("arguments changed on re-check".to_owned())
        );
    }

    #[tokio::test]
    async fn recheck_passes_an_allow_rule_without_calling_the_handler() {
        // The built-in guard asks for `rm -rf /`; the re-check answers it
        // without the handler, and the allow rule passes the call.
        let tool: Arc<dyn Tool<()>> = Arc::new(Pay);
        let ctx = RunContext::ephemeral(())
            .with_allow_rules(vec![AllowRule::tool("Bash")])
            .with_approval_handler(Arc::new(PanicHandler));
        let rm_root = json!({ "command": "rm -rf /" });
        assert_eq!(
            recheck_approved(&ctx, Some(&tool), "Bash", &rm_root).await,
            Ok(())
        );
    }

    #[tokio::test]
    async fn recheck_passes_with_the_flag_off() {
        let ctx = RunContext::ephemeral(()).with_permission_policy(Arc::new(DeferPolicy));
        assert!(!ctx.deferred_tool_calls_enabled());
        assert_eq!(
            recheck_approved(&ctx, Some(&pay()), "pay", &args()).await,
            Ok(())
        );
    }
}
