//! [`WriteTool`] — create or overwrite a file inside the sandbox.

use std::marker::PhantomData;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::Value;

use crate::sandbox::{guard_relative, Sandbox};

/// Arguments for [`WriteTool`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WriteArgs {
    /// Path to write, relative to the sandbox root.
    path: String,
    /// Full file contents (overwrites any existing file).
    content: String,
}

/// Create or overwrite a file relative to the sandbox root, creating parent
/// directories inside the sandbox as needed.
pub struct WriteTool<Ctx = ()> {
    sandbox: Sandbox,
    schema: Value,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl<Ctx> WriteTool<Ctx> {
    /// Construct a `WriteTool` over `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            schema: serde_json::to_value(schemars::schema_for!(WriteArgs))
                .expect("WriteArgs schema serializes"),
            _ctx: PhantomData,
        }
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for WriteTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "Write"
    }

    fn description(&self) -> &str {
        "Create or overwrite a file relative to the sandbox root. Parent \
         directories inside the sandbox are created as needed. Returns \
         `{ \"path\": \"<rel>\", \"bytes_written\": <n> }`."
    }

    fn schema(&self) -> &Value {
        &self.schema
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Write
    }

    async fn invoke(&self, _ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let args: WriteArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs {
            schema_errors: vec![e.to_string()],
        })?;
        let rel = guard_relative(&args.path)
            .map_err(|reason| ToolError::Denied { reason })?
            .to_path_buf();

        let sandbox = self.sandbox.clone();
        let content = args.content;
        let bytes = content.len();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let dir = sandbox.dir();
            if let Some(parent) = rel.parent() {
                if !parent.as_os_str().is_empty() {
                    dir.create_dir_all(parent)?;
                }
            }
            dir.write(&rel, content.as_bytes())
        })
        .await
        .map_err(|e| ToolError::Other(e.into()))?
        // Deliberate: all write-side I/O errors (including PermissionDenied or
        // disk-full on an in-sandbox path) are reported as Denied rather than
        // Other. A finer split can be added if the taxonomy later warrants it.
        .map_err(|e| ToolError::Denied {
            reason: format!("cannot write {}: {e}", args.path),
        })?;

        Ok(ToolOutput::new(
            serde_json::json!({ "path": args.path, "bytes_written": bytes }),
        ))
    }
}
