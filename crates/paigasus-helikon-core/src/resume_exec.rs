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
    AgentEvent, ContentPart, Item, RunContext, Tool, ToolCallOutcome, ToolContext, ToolResolution,
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
        let span = crate::agent::tool_execute_span(parent, &name, true);
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
                    if let Err(reason) = recheck_approved(ctx, tool.as_ref(), &name, &args).await {
                        return deny(reason);
                    }
                    // The re-check denies a missing tool, so `tool` is set
                    // here. Fail closed all the same.
                    let Some(tool) = tool else {
                        return deny("the re-check passed without a tool".to_owned());
                    };
                    tool
                }
            };

            let result = crate::agent::invoke_and_finalize(
                &tool,
                tool_ctx,
                args,
                interceptors,
                redact_output,
                secrets,
            )
            .await;
            ToolCallOutcome { call_id, result }
        }
        .instrument(span)
    });

    let outcomes = crate::agent::run_with_limit(futures, limit).await;
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

/// Close the open handoff transfer calls of `plan` from its level to the end
/// of the path (spec §4.7). The agent that denies the calls of a resume uses
/// this, because the target of each such call does not run. A routing exit
/// that fails before the target runs uses it too.
///
/// Every transfer call of each handoff response on the path is closed: a
/// handoff response can hold more than one transfer call. Each transfer call
/// is found by the position that its own handoff recorded, not by its call
/// id alone, because two transfer calls on the path can share a call id.
///
/// For each transfer call in `conversation` that has no result, this puts
/// the synthetic result `agent no longer available: {name}` directly after
/// the call (`name` is the agent that the call led to). It returns the
/// inserted results in path order, for the agent to emit before the denial
/// results.
///
/// A transfer call that already has a result keeps that result, but this
/// moves it to directly after the call. The move changes only the model
/// request, so it is not returned and the session gets no second result. A
/// result belongs to the earliest open call with its call id, as in the
/// session recorder. The parent emits the transfer result directly after the
/// handoff. Only a session stored by core 0.6.0 or earlier can hold a late
/// result.
pub(crate) fn close_open_transfers(
    plan: &crate::ResumePlan,
    conversation: &mut Vec<Item>,
) -> Vec<Item> {
    let mut transfers: Vec<(crate::deferral::TransferCall, String)> = Vec::new();
    let mut level = plan.clone();
    while let Some(next) = level.next_agent() {
        for call in level.transfer_calls_to_next() {
            transfers.push((call.clone(), next.to_owned()));
        }
        level = level.descend();
    }

    let mut closed = Vec::new();
    for (call, name) in transfers {
        let Some(at) = conversation
            .iter()
            .enumerate()
            .rev()
            .filter(
                |(_, i)| matches!(i, Item::ToolCall { call_id, .. } if *call_id == call.call_id),
            )
            .nth(call.later_calls)
            .map(|(at, _)| at)
        else {
            continue;
        };
        if let Some(result_at) = paired_result(conversation, at) {
            let result = conversation.remove(result_at);
            conversation.insert(at + 1, result);
            continue;
        }
        let result = Item::ToolResult {
            call_id: call.call_id,
            content: vec![ContentPart::Text {
                text: format!("agent no longer available: {name}"),
            }],
        };
        conversation.insert(at + 1, result.clone());
        closed.push(result);
    }
    closed
}

/// The index of the result in `conversation` that closes the `ToolCall` at
/// `call_at`, if any. Each result closes the earliest open call with its
/// call id.
fn paired_result(conversation: &[Item], call_at: usize) -> Option<usize> {
    // The open calls, as (call id, index), in conversation order.
    let mut open: Vec<(&str, usize)> = Vec::new();
    for (at, item) in conversation.iter().enumerate() {
        match item {
            Item::ToolCall { call_id, .. } => open.push((call_id, at)),
            Item::ToolResult { call_id, .. } => {
                if let Some(p) = open.iter().position(|(id, _)| id == call_id) {
                    if open.remove(p).1 == call_at {
                        return Some(at);
                    }
                }
            }
            _ => {}
        }
    }
    None
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
