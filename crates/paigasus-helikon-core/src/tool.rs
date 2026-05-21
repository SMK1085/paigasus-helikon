//! The [`Tool`] trait and its carrier types.
//!
//! Tools are object-safe by design — applications hold heterogeneous
//! registries as `Vec<Arc<dyn Tool<Ctx>>>`.

use std::marker::PhantomData;

use async_trait::async_trait;

/// A tool an agent can call.
///
/// Object-safe by design — applications hold heterogeneous registries as
/// `Vec<Arc<dyn Tool<Ctx>>>`.
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use paigasus_helikon_core::{Tool, ToolContext, ToolError, ToolOutput};
/// use serde_json::{json, Value};
///
/// struct EchoTool {
///     schema: Value,
/// }
///
/// #[async_trait]
/// impl Tool<()> for EchoTool {
///     fn name(&self) -> &str { "echo" }
///     fn description(&self) -> &str { "Returns the input verbatim." }
///     fn schema(&self) -> &Value { &self.schema }
///
///     async fn invoke(
///         &self,
///         _ctx: &ToolContext<()>,
///         args: Value,
///     ) -> Result<ToolOutput, ToolError> {
///         Ok(ToolOutput::new(args))
///     }
/// }
///
/// let _tool = EchoTool {
///     schema: json!({ "type": "object" }),
/// };
/// ```
#[async_trait]
pub trait Tool<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Tool name, unique per registry. Used by the model to address calls.
    fn name(&self) -> &str;
    /// Human-readable description, shown to the model.
    fn description(&self) -> &str;
    /// JSON Schema for the argument payload.
    fn schema(&self) -> &serde_json::Value;
    /// Optional JSON Schema for the return payload. Default is `None`.
    fn output_schema(&self) -> Option<&serde_json::Value> {
        None
    }

    /// Execute the tool with `args` (a JSON value matching [`Tool::schema`]).
    async fn invoke(
        &self,
        ctx: &ToolContext<Ctx>,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError>;
}

/// A narrower view of [`crate::RunContext`] passed to [`Tool::invoke`].
///
/// Field shape lands with the agent-loop ticket.
pub struct ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    _ctx: PhantomData<fn() -> Ctx>,
}

impl<Ctx> ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct a bare [`ToolContext`].
    pub fn new() -> Self {
        Self { _ctx: PhantomData }
    }
}

impl<Ctx> Default for ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// The result of a successful [`Tool::invoke`] call.
///
/// Field shape (multi-modal content, metadata) lands with later tickets.
/// Today `content` is the raw JSON value the tool returned.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ToolOutput {
    /// The tool's return payload, as JSON.
    pub content: serde_json::Value,
}

impl ToolOutput {
    /// Construct a [`ToolOutput`] with the given JSON content.
    pub fn new(content: serde_json::Value) -> Self {
        Self { content }
    }
}

/// Errors raised by [`Tool::invoke`].
///
/// `InvalidArgs` is the single recoverable variant per ADR-10: the runner
/// is permitted to feed the schema errors back to the model once before
/// surfacing [`crate::AgentError::InvalidStructuredOutput`]. No other
/// variant is recoverable.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    /// Arguments did not match [`Tool::schema`].
    ///
    /// Recoverable per ADR-10 — the runner may feed `schema_errors` back to
    /// the model once before surfacing
    /// [`crate::AgentError::InvalidStructuredOutput`].
    #[error("invalid tool arguments: {schema_errors:?}")]
    InvalidArgs {
        /// Human-readable schema-validation errors.
        schema_errors: Vec<String>,
    },

    /// Escape hatch for arbitrary tool failures. See ADR-10.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
