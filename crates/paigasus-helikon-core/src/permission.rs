//! Permission layer: gate tool calls via deny rules → permission mode →
//! `canUseTool` policy. See the *Permissions, Guardrails & Hooks* concept page.

use async_trait::async_trait;

use crate::RunContext;

/// How permission mode governs tool calls.
///
/// `Bypass` propagates to subagents and **cannot be overridden** — a typed
/// enum, not a string. The non-override property is enforced by
/// [`RunContext::with_permission_mode`], which refuses to downgrade `Bypass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum PermissionMode {
    /// Defer to the policy (ask for unfamiliar tools); permissive when no policy.
    #[default]
    Default,
    /// Auto-approve tools with a [`crate::ToolEffect::Write`] effect; all other
    /// tools still reach the policy.
    AcceptEdits,
    /// Read-only: deny any tool whose [`crate::ToolEffect`] is not `ReadOnly`.
    Plan,
    /// Dangerous: allow all (deny rules still apply). Propagates; sticky.
    Bypass,
}

/// The outcome of a [`PermissionPolicy::check`] (or the resolved decision).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PermissionDecision {
    /// Run the call unchanged.
    Allow,
    /// Block the call; the reason is surfaced to the model as a tool result.
    Deny {
        /// Human-readable denial reason.
        reason: String,
    },
    /// Ask a human (resolved via [`ApprovalHandler`]; default Deny).
    AskUser {
        /// Prompt shown to the approver.
        prompt: String,
    },
    /// Replace the call's arguments before execution (sanitize).
    Replace {
        /// Replacement JSON arguments.
        args: serde_json::Value,
    },
}

/// Authorizes a tool call. The decision pipeline runs
/// `deny rules › mode › this policy` (see `control.rs`).
#[async_trait]
pub trait PermissionPolicy<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Decide whether `tool` may run with `args`.
    async fn check(
        &self,
        ctx: &RunContext<Ctx>,
        tool: &str,
        args: &serde_json::Value,
    ) -> PermissionDecision;
}

/// How a [`DenyRule`] matches a call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Matcher {
    /// Exact tool name.
    Tool(String),
    /// Any Bash sub-command whose resolved program equals this. Tool-scoped to
    /// the `Bash` tool.
    BashProgram(String),
}

/// A first-class deny rule, evaluated **before** mode — so it overrides even
/// [`PermissionMode::Bypass`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyRule {
    matcher: Matcher,
}

impl DenyRule {
    /// Deny a tool by its exact name.
    pub fn tool(name: impl Into<String>) -> Self {
        Self {
            matcher: Matcher::Tool(name.into()),
        }
    }

    /// Deny a Bash call whose compound command contains a sub-command whose
    /// resolved program equals `program` (operator-, wrapper-, and
    /// `bash -c`-aware). Only matches the `Bash` tool.
    pub fn bash_command(program: impl Into<String>) -> Self {
        Self {
            matcher: Matcher::BashProgram(program.into()),
        }
    }

    /// `true` if this rule denies `tool` invoked with `args`.
    pub fn matches(&self, tool: &str, args: &serde_json::Value) -> bool {
        match &self.matcher {
            Matcher::Tool(name) => name == tool,
            Matcher::BashProgram(program) => {
                if tool != "Bash" {
                    return false;
                }
                let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
                    return false;
                };
                crate::command_match::resolve_all(command)
                    .iter()
                    .any(|c| &c.program == program)
            }
        }
    }
}

/// Resolves a [`PermissionDecision::AskUser`] when the driver cannot decide
/// inline. Non-generic — it needs no `Ctx`.
#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    /// Decide an `AskUser` prompt. Returns a narrowed [`ApprovalOutcome`]
    /// (cannot recursively ask).
    async fn decide(&self, tool: &str, prompt: &str, args: &serde_json::Value) -> ApprovalOutcome;
}

/// The narrowed decision an [`ApprovalHandler`] may return.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ApprovalOutcome {
    /// Allow the call.
    Allow,
    /// Deny the call with a reason.
    Deny {
        /// Human-readable denial reason.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn permission_mode_default_is_default_variant() {
        assert_eq!(PermissionMode::default(), PermissionMode::Default);
    }

    #[test]
    fn deny_rule_matches_exact_tool_name_only() {
        let rule = DenyRule::tool("rm");
        assert!(rule.matches("rm", &json!({})));
        assert!(!rule.matches("ls", &json!({})));
        assert!(rule.matches("rm", &json!({"path": "/etc/passwd"})));
    }

    #[test]
    fn bash_command_matches_any_subcommand_program() {
        let rule = DenyRule::bash_command("rm");
        let args = json!({ "command": "echo ok && rm -rf ." });
        assert!(rule.matches("Bash", &args));
        let safe = json!({ "command": "echo ok && ls" });
        assert!(!rule.matches("Bash", &safe));
    }

    #[test]
    fn bash_command_is_tool_scoped() {
        let rule = DenyRule::bash_command("rm");
        // A non-Bash tool carrying a `command` field must not trip it.
        assert!(!rule.matches("Other", &json!({ "command": "rm -rf ." })));
    }

    #[test]
    fn bash_command_sees_through_sudo_and_bash_c() {
        let rule = DenyRule::bash_command("rm");
        assert!(rule.matches("Bash", &json!({ "command": "sudo rm -rf /" })));
        assert!(rule.matches("Bash", &json!({ "command": "bash -c 'rm -rf /'" })));
    }
}
