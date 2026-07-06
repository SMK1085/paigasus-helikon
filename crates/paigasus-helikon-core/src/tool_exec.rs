//! The public per-tool-call execution pipeline: resolve → authorize →
//! invoke → redact → convert.
//!
//! This is the primitive a durable runner (e.g. a Temporal activity) calls
//! directly to execute one tool call with exactly the same authorization and
//! redaction the ephemeral `LlmAgent` loop applies inline — so a durable
//! runner never writes unauthorized or unredacted tool output into its
//! history. The ephemeral loop driver composes the same building blocks
//! ([`crate::RunContext::authorize_tool`] and [`finalize_tool_output`])
//! around its own hook interleave; [`execute_tool_call`] is the hook-free
//! version for callers with no hook registry to fire.

/// Render a tool's raw JSON output to content parts, applying redaction last.
///
/// Redaction is deliberately the final transform: whatever a caller did to
/// the JSON beforehand (e.g. a `PostToolUse` hook rewriting it), this
/// function's redaction pass runs after, so nothing downstream can
/// reintroduce a secret past it.
pub fn finalize_tool_output(
    output: serde_json::Value,
    redact_output: bool,
    secrets: &crate::redaction::SecretSet,
) -> Vec<crate::ContentPart> {
    let output = if redact_output {
        crate::redaction::redact(&output, secrets)
    } else {
        output
    };
    tool_output_to_content_parts(&crate::ToolOutput::new(output))
}

/// Conversion convention: `ToolOutput.content` (SMA-313's
/// `serde_json::Value`) becomes one `ContentPart::Text`.
/// `Value::String(s) -> ContentPart::Text { text: s }`; other JSON
/// values are stringified via `Value::to_string()`.
pub(crate) fn tool_output_to_content_parts(output: &crate::ToolOutput) -> Vec<crate::ContentPart> {
    let text = match &output.content {
        serde_json::Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    vec![crate::ContentPart::Text { text }]
}

/// The hook-free single-call pipeline durable runners execute:
/// resolve → authorize → invoke → redact → convert. Returns the outcome plus
/// an optional `AgentEvent::PermissionDenied` to surface.
///
/// Tool resolution mirrors the ephemeral loop driver: an unknown tool name
/// still runs the authorize step first (using [`crate::ToolEffect::SideEffect`]
/// as its effective side-effect profile), and only fails with `unknown tool:
/// {name}` once authorization allows the call through.
pub async fn execute_tool_call<Ctx>(
    tools: &[std::sync::Arc<dyn crate::Tool<Ctx>>],
    run_ctx: &crate::RunContext<Ctx>,
    tool_ctx: &crate::ToolContext<Ctx>,
    call: &crate::ToolCallRequest,
) -> (crate::ToolCallOutcome, Option<crate::AgentEvent>)
where
    Ctx: Send + Sync + 'static,
{
    let tool = tools.iter().find(|t| t.name() == call.name).cloned();
    let effect = tool
        .as_ref()
        .map(|t| t.effect())
        .unwrap_or(crate::ToolEffect::SideEffect);

    let mut args = call.args.clone();
    match run_ctx.authorize_tool(&call.name, effect, &args).await {
        crate::PermissionDecision::Allow => {}
        crate::PermissionDecision::Replace { args: sanitized } => {
            args = sanitized;
        }
        crate::PermissionDecision::Deny { reason }
        | crate::PermissionDecision::AskUser { prompt: reason } => {
            let outcome = crate::ToolCallOutcome {
                call_id: call.call_id.clone(),
                result: Err(format!("permission denied: {reason}")),
            };
            let event = crate::AgentEvent::PermissionDenied {
                tool: call.name.clone(),
                reason,
            };
            return (outcome, Some(event));
        }
    }

    let Some(tool) = tool else {
        return (
            crate::ToolCallOutcome {
                call_id: call.call_id.clone(),
                result: Err(format!("unknown tool: {}", call.name)),
            },
            None,
        );
    };

    let result = match tool.invoke(tool_ctx, args).await {
        Ok(output) => {
            let secrets = crate::redaction::SecretSet::from_env_and_extra(run_ctx.extra_secrets());
            Ok(finalize_tool_output(
                output.content,
                run_ctx.redact_output(),
                &secrets,
            ))
        }
        Err(e) => Err(e.to_string()),
    };

    (
        crate::ToolCallOutcome {
            call_id: call.call_id.clone(),
            result,
        },
        None,
    )
}
