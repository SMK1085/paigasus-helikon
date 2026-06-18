//! [`BashTool`] — a shell tool that runs commands through a pluggable
//! [`ExecutionBackend`]. The tool itself only parses arguments and applies the
//! command allow/deny lists; *containment* is the backend's job (and is reported
//! in this tool's description via the backend's `guarantees()`).

use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::Value;

use crate::exec::{ExecRequest, ExecutionBackend};

/// Arguments for [`BashTool`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BashArgs {
    /// The shell command to run (`sh -c <command>` on unix, `cmd /C` on Windows).
    command: String,
}

/// Builder for [`BashTool`]. Obtain one via [`BashTool::builder`].
pub struct BashToolBuilder {
    backend: Arc<dyn ExecutionBackend>,
    deny_commands: Vec<String>,
    allow_commands: Option<Vec<String>>,
}

impl BashToolBuilder {
    /// Refuse a command if ANY sub-command's resolved program is in this list.
    ///
    /// Compound commands are split operator-aware: a deny rule blocks the call
    /// if ANY sub-command's program matches (so `echo ok && rm -rf .` is refused
    /// under `deny=["rm"]`).
    pub fn deny_commands<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.deny_commands = names.into_iter().map(Into::into).collect();
        self
    }

    /// If set, allow ONLY commands where EVERY sub-command's program is in this list.
    ///
    /// Compound commands are split operator-aware: an allow list permits the call
    /// only if EVERY sub-command's program is listed (so `git status && rm -rf .`
    /// is refused under `allow=["git"]`).
    pub fn allow_commands<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allow_commands = Some(names.into_iter().map(Into::into).collect());
        self
    }

    /// Finish building.
    pub fn build<Ctx>(self) -> BashTool<Ctx> {
        let label = self.backend.guarantees().label;
        let description = format!(
            "Run a shell command. Containment tier: {label}. The working directory \
             is pinned to the sandbox root. The actual enforcement depends on the \
             configured backend (see the containment tier above); gate access with \
             a PermissionPolicy or DenyRule(\"Bash\") as needed."
        );
        BashTool {
            backend: self.backend,
            deny_commands: self.deny_commands,
            allow_commands: self.allow_commands,
            description,
            schema: serde_json::to_value(schemars::schema_for!(BashArgs))
                .expect("BashArgs schema serializes"),
            _ctx: PhantomData,
        }
    }
}

/// A shell tool backed by a pluggable [`ExecutionBackend`].
///
/// Command allow/deny lists are evaluated operator-aware: compound commands
/// (e.g. `echo ok && rm -rf .`) are split into sub-commands before matching.
/// A deny rule blocks the call if ANY sub-command's resolved program is denied;
/// an allow list permits the call only if EVERY sub-command's program is listed.
pub struct BashTool<Ctx = ()> {
    backend: Arc<dyn ExecutionBackend>,
    deny_commands: Vec<String>,
    allow_commands: Option<Vec<String>>,
    description: String,
    schema: Value,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl BashTool<()> {
    /// Start building a `BashTool` over `backend`.
    pub fn builder(backend: Arc<dyn ExecutionBackend>) -> BashToolBuilder {
        BashToolBuilder {
            backend,
            deny_commands: Vec::new(),
            allow_commands: None,
        }
    }

    /// Build a `BashTool` over `backend` with no command allow/deny lists.
    pub fn new(backend: Arc<dyn ExecutionBackend>) -> BashTool<()> {
        Self::builder(backend).build()
    }
}

impl<Ctx> BashTool<Ctx> {
    fn check_command_allowed(&self, command: &str) -> Result<(), ToolError> {
        let resolved = paigasus_helikon_core::command_match::resolve_all(command);
        let programs: Vec<&str> = resolved.iter().map(|c| c.program.as_str()).collect();

        // Deny if ANY sub-command's program is denied.
        if let Some(bad) = programs
            .iter()
            .find(|p| self.deny_commands.iter().any(|d| d == *p))
        {
            return Err(ToolError::Denied {
                reason: format!("command `{bad}` is blocked by the deny list"),
            });
        }
        // With an allowlist, ALL sub-command programs must be allowed.
        if let Some(allow) = &self.allow_commands {
            if let Some(bad) = programs.iter().find(|p| !allow.iter().any(|a| a == *p)) {
                return Err(ToolError::Denied {
                    reason: format!("command `{bad}` is not in the allow list"),
                });
            }
        }
        Ok(())
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for BashTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> &Value {
        &self.schema
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::SideEffect
    }

    async fn invoke(&self, _ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let args: BashArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs {
            schema_errors: vec![e.to_string()],
        })?;
        self.check_command_allowed(&args.command)?;
        let out = self.backend.run(ExecRequest::new(args.command)).await?;
        Ok(ToolOutput::new(serde_json::json!({
            "stdout": out.stdout,
            "stderr": out.stderr,
            "exit_code": out.exit_code,
            "timed_out": out.timed_out,
            "truncated": out.truncated,
        })))
    }
}
