//! [`EditTool`] — exact string replacement inside a sandbox file.

use std::marker::PhantomData;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::Value;

use crate::sandbox::{guard_relative, Sandbox};

/// Arguments for [`EditTool`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EditArgs {
    /// Path to edit, relative to the sandbox root.
    path: String,
    /// The exact text to replace. Must occur in the file.
    old_string: String,
    /// The replacement text.
    new_string: String,
    /// Replace every occurrence. When false (default), `old_string` must be
    /// unique or the edit is refused.
    #[serde(default)]
    replace_all: bool,
}

/// Replace an exact string in a sandbox file. Refuses (does not guess) when
/// `old_string` is missing or ambiguous.
pub struct EditTool<Ctx = ()> {
    sandbox: Sandbox,
    schema: Value,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl<Ctx> EditTool<Ctx> {
    /// Construct an `EditTool` over `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            schema: serde_json::to_value(schemars::schema_for!(EditArgs))
                .expect("EditArgs schema serializes"),
            _ctx: PhantomData,
        }
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for EditTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        "Replace an exact string in a sandbox file. `old_string` must occur in \
         the file, and must be unique unless `replace_all` is true. Returns \
         `{ \"path\": \"<rel>\", \"replacements\": <n> }`."
    }

    fn schema(&self) -> &Value {
        &self.schema
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Write
    }

    async fn invoke(&self, _ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let args: EditArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs {
            schema_errors: vec![e.to_string()],
        })?;
        let rel = guard_relative(&args.path)
            .map_err(|reason| ToolError::Denied { reason })?
            .to_path_buf();

        let EditArgs {
            path,
            old_string,
            new_string,
            replace_all,
        } = args;

        if old_string.is_empty() {
            return Err(ToolError::Denied {
                reason: "old_string must not be empty".to_owned(),
            });
        }

        let sandbox = self.sandbox.clone();

        let (path, count) =
            tokio::task::spawn_blocking(move || -> Result<(String, usize), ToolError> {
                let dir = sandbox.dir();
                let original = dir.read_to_string(&rel).map_err(|e| match e.kind() {
                    std::io::ErrorKind::NotFound => {
                        ToolError::Other(anyhow::anyhow!("no such file: {path}"))
                    }
                    _ => ToolError::Denied {
                        reason: format!("cannot read {path} for edit: {e}"),
                    },
                })?;
                let count = original.matches(&old_string).count();
                if count == 0 {
                    return Err(ToolError::Denied {
                        reason: format!("old_string not found in {path}"),
                    });
                }
                if count > 1 && !replace_all {
                    return Err(ToolError::Denied {
                        reason: "old_string is not unique; pass replace_all or add context"
                            .to_owned(),
                    });
                }
                let updated = if replace_all {
                    original.replace(&old_string, &new_string)
                } else {
                    original.replacen(&old_string, &new_string, 1)
                };
                dir.write(&rel, updated.as_bytes())
                    .map_err(|e| ToolError::Denied {
                        reason: format!("cannot write {path}: {e}"),
                    })?;
                // For replace_all=false, count == 1 here (the >1 case was refused above).
                Ok((path, count))
            })
            .await
            .map_err(|e| ToolError::Other(e.into()))??;

        Ok(ToolOutput::new(
            serde_json::json!({ "path": path, "replacements": count }),
        ))
    }
}
