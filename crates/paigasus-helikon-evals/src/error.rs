//! Error types for the evals crate.

/// Errors produced by dataset loading, evaluation, and eval runs.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EvalError {
    /// Reading a dataset or script file failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// A JSONL line failed to parse.
    #[error("parse error on line {line}: {source}")]
    Parse {
        /// 1-based line number in the JSONL file.
        line: usize,
        /// The underlying serde error.
        source: serde_json::Error,
    },
    /// A JSON Schema failed to compile.
    #[error("invalid json schema: {0}")]
    InvalidSchema(String),
    /// `EvalRun` was started without a context factory.
    #[error("EvalRun requires a ctx_factory (or default_ctx)")]
    MissingCtxFactory,
    /// `EvalRun` was started without an agent or agent factory.
    #[error("EvalRun requires an agent or agent_factory")]
    MissingAgent,
    /// `EvalRun` was started without a dataset.
    #[error("EvalRun requires a dataset")]
    MissingDataset,
    /// An agent run failed during evaluation.
    #[error("agent run failed: {0}")]
    Run(String),
    /// Any other error.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
