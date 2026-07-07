//! [`RhaiTool`] — a [`Tool`] whose body is a sandboxed Rhai script.
//!
//! The script is compiled once at construction (compile errors surface
//! from [`RhaiTool::new`], not at call time) and must define a
//! `fn run(args)` entry point. Each invocation runs on a blocking thread
//! with a hard operation budget, so a runaway script terminates with an
//! error instead of wedging the runtime. Script failures are returned as
//! [`ToolError::Other`] — never a panic.

use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolError, ToolOutput};

/// Hard cap on Rhai operations per invocation; a script that exceeds it
/// is terminated with an error.
const MAX_OPERATIONS: u64 = 1_000_000;

/// A tool implemented by a Rhai script exposing `fn run(args)`.
pub struct RhaiTool {
    name: String,
    description: String,
    schema: serde_json::Value,
    engine: Arc<rhai::Engine>,
    ast: Arc<rhai::AST>,
}

impl RhaiTool {
    /// Compiles `source` and returns the tool.
    ///
    /// Fails if the script does not parse; the `run` function's presence
    /// is only checked at invoke time.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        params_schema: serde_json::Value,
        source: &str,
    ) -> anyhow::Result<Self> {
        let name = name.into();
        let mut engine = rhai::Engine::new();
        engine.set_max_operations(MAX_OPERATIONS);
        let ast = engine
            .compile(source)
            .with_context(|| format!("failed to compile rhai tool '{name}'"))?;
        Ok(Self {
            name,
            description: description.into(),
            schema: params_schema,
            engine: Arc::new(engine),
            ast: Arc::new(ast),
        })
    }
}

#[async_trait]
impl Tool<()> for RhaiTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }

    async fn invoke(
        &self,
        _ctx: &ToolContext<()>,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let engine = Arc::clone(&self.engine);
        let ast = Arc::clone(&self.ast);
        let result = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
            let dyn_args = rhai::serde::to_dynamic(&args).map_err(|e| e.to_string())?;
            let mut scope = rhai::Scope::new();
            let out: rhai::Dynamic = engine
                .call_fn(&mut scope, &ast, "run", (dyn_args,))
                .map_err(|e| e.to_string())?;
            rhai::serde::from_dynamic(&out).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| ToolError::Other(anyhow::anyhow!("rhai task join error: {e}")))?
        .map_err(|e| ToolError::Other(anyhow::anyhow!("rhai tool '{}' failed: {e}", self.name)))?;
        Ok(ToolOutput::new(result))
    }
}
