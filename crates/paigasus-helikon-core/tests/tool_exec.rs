//! SMA-332: the public tool-call authorize/redact pipeline durable runners
//! reuse (`execute_tool_call` / `finalize_tool_output`). This is the
//! regression net for the primitives a Temporal activity will call directly,
//! outside the ephemeral `LlmAgent` loop.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use paigasus_helikon_core::redaction::SecretSet;
use paigasus_helikon_core::{
    execute_tool_call, finalize_tool_output, AgentEvent, ContentPart, DenyRule, PermissionDecision,
    PermissionPolicy, RunContext, Tool, ToolCallRequest,
};

use common::{noop_run_context, MockTool};

fn call(name: &str) -> ToolCallRequest {
    ToolCallRequest {
        call_id: "c1".into(),
        name: name.into(),
        args: serde_json::json!({}),
    }
}

#[tokio::test]
async fn allow_path_invokes_and_redacts_extra_secret() {
    let ctx = noop_run_context::<()>().with_extra_secrets(vec!["supersecretvalue".into()]);
    let tool_ctx = ctx.to_tool_context();
    let tool = MockTool::new(
        "secret",
        serde_json::json!({ "stdout": "FOO_API_KEY=supersecretvalue" }),
    );
    let tools: Vec<Arc<dyn Tool<()>>> = vec![tool as Arc<dyn Tool<()>>];

    let (outcome, event) = execute_tool_call(&tools, &ctx, &tool_ctx, &call("secret")).await;

    assert!(event.is_none(), "no permission event on the allow path");
    let parts = outcome.result.expect("tool ran ok");
    let rendered: String = parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !rendered.contains("supersecretvalue"),
        "secret leaked into tool output: {rendered}"
    );
}

#[tokio::test]
async fn deny_rule_denies_and_surfaces_permission_event() {
    let ctx = noop_run_context::<()>().with_deny_rules(vec![DenyRule::tool("secret")]);
    let tool_ctx = ctx.to_tool_context();
    let tool = MockTool::new("secret", serde_json::json!({ "ok": true }));
    let tools: Vec<Arc<dyn Tool<()>>> = vec![tool as Arc<dyn Tool<()>>];

    let (outcome, event) = execute_tool_call(&tools, &ctx, &tool_ctx, &call("secret")).await;

    let err = outcome.result.expect_err("deny rule must block the call");
    assert!(
        err.starts_with("permission denied: "),
        "unexpected error: {err}"
    );
    assert!(matches!(
        event,
        Some(AgentEvent::PermissionDenied { tool, .. }) if tool == "secret"
    ));
}

#[tokio::test]
async fn unknown_tool_errors_without_an_event() {
    let ctx = noop_run_context::<()>();
    let tool_ctx = ctx.to_tool_context();
    let tools: Vec<Arc<dyn Tool<()>>> = Vec::new();

    let (outcome, event) = execute_tool_call(&tools, &ctx, &tool_ctx, &call("nope")).await;

    assert_eq!(outcome.result, Err("unknown tool: nope".to_owned()));
    assert!(event.is_none());
}

struct DeferPolicy;
#[async_trait::async_trait]
impl PermissionPolicy<()> for DeferPolicy {
    async fn check(
        &self,
        _: &RunContext<()>,
        _: &str,
        _: &serde_json::Value,
    ) -> PermissionDecision {
        PermissionDecision::Defer {
            prompt: "policy says wait".into(),
        }
    }
}

#[tokio::test]
async fn defer_is_a_deny_on_the_durable_path_even_with_the_flag_on() {
    let ctx = noop_run_context::<()>()
        .with_deferred_tool_calls(true)
        .with_permission_policy(Arc::new(DeferPolicy));
    let tool_ctx = ctx.to_tool_context();
    let tool = MockTool::new("secret", serde_json::json!({ "ok": true }));
    let tools: Vec<Arc<dyn Tool<()>>> = vec![tool.clone() as Arc<dyn Tool<()>>];

    let (outcome, event) = execute_tool_call(&tools, &ctx, &tool_ctx, &call("secret")).await;

    assert_eq!(
        outcome.result,
        Err(
            "permission denied: approval deferred, but this run cannot defer: policy says wait"
                .to_owned()
        )
    );
    assert!(matches!(
        event,
        Some(AgentEvent::PermissionDenied { tool, .. }) if tool == "secret"
    ));
    assert!(tool.invocations().is_empty(), "the tool must not run");
}

#[test]
fn finalize_tool_output_passes_plain_strings_through_unredacted() {
    let parts = finalize_tool_output(
        serde_json::json!("plain"),
        false,
        &SecretSet::from_env_and_extra(&[]),
    );
    assert_eq!(
        parts,
        vec![ContentPart::Text {
            text: "plain".to_owned()
        }]
    );
}

const PROMPT_SECRET: &str = "sk-test-PROMPT-SECRET-5678";

/// Defers every call with a prompt that holds a known secret.
struct DeferWithSecret;
#[async_trait::async_trait]
impl PermissionPolicy<()> for DeferWithSecret {
    async fn check(
        &self,
        _: &RunContext<()>,
        _: &str,
        _: &serde_json::Value,
    ) -> PermissionDecision {
        PermissionDecision::Defer {
            prompt: format!("pay with key {PROMPT_SECRET}?"),
        }
    }
}

/// The denial of a deferred call carries the redacted prompt, in the
/// `PermissionDenied` event and in the tool result.
async fn assert_deferred_denial_is_redacted(ctx: RunContext<()>) {
    let ctx = ctx
        .with_extra_secrets(vec![PROMPT_SECRET.into()])
        .with_permission_policy(Arc::new(DeferWithSecret));
    let tool_ctx = ctx.to_tool_context();
    let tool = MockTool::new("pay", serde_json::json!({ "ok": true }));
    let tools: Vec<Arc<dyn Tool<()>>> = vec![tool.clone() as Arc<dyn Tool<()>>];

    let (outcome, event) = execute_tool_call(&tools, &ctx, &tool_ctx, &call("pay")).await;

    let err = outcome.result.expect_err("a deferral is a deny here");
    assert!(
        err.starts_with("permission denied: approval deferred, but this run cannot defer: "),
        "{err}"
    );
    assert!(!err.contains(PROMPT_SECRET), "the tool result leaks: {err}");
    let Some(AgentEvent::PermissionDenied { reason, .. }) = event else {
        panic!("no PermissionDenied event: {event:?}");
    };
    assert!(reason.starts_with("approval deferred"), "{reason}");
    assert!(!reason.contains(PROMPT_SECRET), "the event leaks: {reason}");
    assert!(tool.invocations().is_empty(), "the tool must not run");
}

#[tokio::test]
async fn deferred_denial_redacts_the_prompt_with_the_flag_off() {
    assert_deferred_denial_is_redacted(noop_run_context::<()>()).await;
}

#[tokio::test]
async fn deferred_denial_redacts_the_prompt_on_the_durable_path() {
    assert_deferred_denial_is_redacted(noop_run_context::<()>().with_deferred_tool_calls(true))
        .await;
}

#[tokio::test]
async fn deferred_denial_keeps_the_prompt_without_redaction() {
    let ctx = noop_run_context::<()>()
        .without_output_redaction()
        .with_extra_secrets(vec![PROMPT_SECRET.into()])
        .with_permission_policy(Arc::new(DeferWithSecret));
    let tool_ctx = ctx.to_tool_context();
    let tools: Vec<Arc<dyn Tool<()>>> = Vec::new();

    let (outcome, _) = execute_tool_call(&tools, &ctx, &tool_ctx, &call("pay")).await;

    let err = outcome.result.expect_err("a deferral is a deny here");
    assert!(err.contains(PROMPT_SECRET), "{err}");
}
