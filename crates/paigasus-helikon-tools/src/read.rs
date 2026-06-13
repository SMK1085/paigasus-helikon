//! [`ReadTool`] — read a UTF-8 text file from the sandbox.

use std::io;
use std::marker::PhantomData;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::Value;

use crate::sandbox::{guard_relative, Sandbox};

/// Arguments for [`ReadTool`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadArgs {
    /// Path to read, relative to the sandbox root.
    path: String,
    /// 1-based first line to return (inclusive). Omit to start at the top.
    /// Passing `0` is treated as `1`.
    offset: Option<u64>,
    /// Maximum number of lines to return. Omit for the whole file.
    limit: Option<u64>,
}

/// Read a UTF-8 text file relative to the sandbox root, optionally windowed by
/// line. Read-only; allowed under `Plan` mode.
pub struct ReadTool<Ctx = ()> {
    sandbox: Sandbox,
    schema: Value,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl<Ctx> ReadTool<Ctx> {
    /// Construct a `ReadTool` over `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            schema: serde_json::to_value(schemars::schema_for!(ReadArgs))
                .expect("ReadArgs schema serializes"),
            _ctx: PhantomData,
        }
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for ReadTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "Read a UTF-8 text file relative to the sandbox root. Optional `offset` \
         and `limit` select a 1-based line window. Returns `{ \"content\": \"<text>\" }`."
    }

    fn schema(&self) -> &Value {
        &self.schema
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn invoke(&self, _ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let args: ReadArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs {
            schema_errors: vec![e.to_string()],
        })?;
        let rel = guard_relative(&args.path)
            .map_err(|reason| ToolError::Denied { reason })?
            .to_path_buf();

        let sandbox = self.sandbox.clone();
        let path_for_msg = args.path.clone();
        let text = tokio::task::spawn_blocking(move || sandbox.dir().read_to_string(rel))
            .await
            .map_err(|e| ToolError::Other(e.into()))?
            .map_err(|e| map_read_error(&path_for_msg, e))?;

        let content = window(&text, args.offset, args.limit);
        Ok(ToolOutput::new(serde_json::json!({ "content": content })))
    }
}

/// Map a `cap-std` read error to the right `ToolError` variant.
fn map_read_error(path: &str, e: io::Error) -> ToolError {
    match e.kind() {
        io::ErrorKind::NotFound => ToolError::Other(anyhow::anyhow!("no such file: {path}")),
        io::ErrorKind::InvalidData => ToolError::Denied {
            reason: format!("file is not valid UTF-8: {path}"),
        },
        // Deliberate: all other I/O errors (including PermissionDenied on an
        // in-sandbox file) are reported as Denied rather than Other. A finer
        // split can be added if the taxonomy later warrants it.
        _ => ToolError::Denied {
            reason: format!("cannot read {path}: {e}"),
        },
    }
}

/// Apply the 1-based `offset`/`limit` line window. The full-file path (no
/// offset/limit) returns the original bytes verbatim; the windowed path
/// normalises line endings via `lines()` + `join("\n")`, so a trailing
/// newline is not preserved in a windowed result.
fn window(text: &str, offset: Option<u64>, limit: Option<u64>) -> String {
    if offset.is_none() && limit.is_none() {
        return text.to_owned();
    }
    let start = offset.unwrap_or(1).saturating_sub(1) as usize;
    let lines: Vec<&str> = text.lines().collect();
    let slice: Vec<&str> = match limit {
        Some(n) => lines.into_iter().skip(start).take(n as usize).collect(),
        None => lines.into_iter().skip(start).collect(),
    };
    slice.join("\n")
}
