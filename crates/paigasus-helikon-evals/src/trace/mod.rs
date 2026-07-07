//! Trace sinks for offline analysis.

use async_trait::async_trait;

use crate::{CaseResult, RunMeta};

#[cfg(feature = "trace-sqlite")]
mod sqlite;
#[cfg(feature = "trace-sqlite")]
pub use sqlite::SqliteTraceSink;

#[cfg(feature = "trace-parquet")]
mod parquet;
#[cfg(feature = "trace-parquet")]
pub use parquet::ParquetTraceSink;

/// Errors from trace sinks.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TraceError {
    /// Backend I/O or storage failure.
    #[error("trace backend error: {0}")]
    Backend(String),
}

/// Receives each case's result during an eval run.
#[async_trait]
pub trait TraceSink: Send + Sync {
    /// Record one case (called once per case, after its evaluators ran).
    async fn record_case(&self, run: &RunMeta, case: &CaseResult) -> Result<(), TraceError>;
    /// Flush and close the sink (called once, after all cases).
    async fn finish(&self) -> Result<(), TraceError>;
}
