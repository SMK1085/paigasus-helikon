//! SMA-332: the public tool-call authorize/redact pipeline durable runners
//! reuse (`execute_tool_call` / `finalize_tool_output`). This is the
//! regression net for the primitives a Temporal activity will call directly,
//! outside the ephemeral `LlmAgent` loop.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use paigasus_helikon_core::redaction::SecretSet;
use paigasus_helikon_core::{
    execute_tool_call, finalize_tool_output, AgentEvent, ContentPart, DenyRule, Tool,
    ToolCallRequest,
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
