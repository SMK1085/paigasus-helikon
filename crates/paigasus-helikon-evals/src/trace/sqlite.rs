//! SQLite-backed [`TraceSink`].

use std::path::Path;

use async_trait::async_trait;
use paigasus_helikon_core::SessionRecorder;
use sqlx::SqlitePool;

use super::{TraceError, TraceSink};
use crate::{CaseResult, RunMeta};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Records eval traces into a SQLite database (`eval_runs` / `eval_cases` /
/// `eval_events` tables — see the crate's `migrations/0001_eval_traces.sql`).
///
/// One instance is one pool; every [`TraceSink::record_case`] call for a
/// given run should share the same [`RunMeta::run_id`], since `eval_runs`
/// is keyed on it.
#[derive(Debug, Clone)]
pub struct SqliteTraceSink {
    pool: SqlitePool,
}

impl SqliteTraceSink {
    /// Open (or create) the trace database at `path`, running the embedded
    /// migration. `path`'s parent directory must already exist.
    ///
    /// # Errors
    ///
    /// Returns [`TraceError::Backend`] if the connection or migration fails.
    pub async fn open(path: &Path) -> Result<Self, TraceError> {
        let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .map_err(|e| TraceError::Backend(e.to_string()))?;
        MIGRATOR
            .run(&pool)
            .await
            .map_err(|e| TraceError::Backend(e.to_string()))?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl TraceSink for SqliteTraceSink {
    async fn record_case(&self, run: &RunMeta, case: &CaseResult) -> Result<(), TraceError> {
        sqlx::query(
            "INSERT OR IGNORE INTO eval_runs (run_id, dataset, started_ts_nanos) \
             VALUES (?, ?, ?)",
        )
        .bind(&run.run_id)
        .bind(&run.dataset)
        .bind(run.started_ts_nanos)
        .execute(&self.pool)
        .await
        .map_err(|e| TraceError::Backend(e.to_string()))?;

        let final_output = case
            .outcome
            .as_ref()
            .map(|o| o.final_output.clone())
            .unwrap_or_default();
        let scores =
            serde_json::to_string(&case.scores).map_err(|e| TraceError::Backend(e.to_string()))?;

        sqlx::query(
            "INSERT INTO eval_cases (run_id, case_id, final_output, error, scores) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&run.run_id)
        .bind(&case.case_id)
        .bind(&final_output)
        .bind(&case.error)
        .bind(&scores)
        .execute(&self.pool)
        .await
        .map_err(|e| TraceError::Backend(e.to_string()))?;

        if let Some(outcome) = &case.outcome {
            let mut rec = SessionRecorder::new("eval");
            for ev in &outcome.events {
                rec.observe(ev);
            }
            for (seq, ev) in rec.drain().iter().enumerate() {
                let payload =
                    serde_json::to_string(ev).map_err(|e| TraceError::Backend(e.to_string()))?;
                sqlx::query(
                    "INSERT INTO eval_events (run_id, case_id, seq, kind, ts_nanos, payload) \
                     VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(&run.run_id)
                .bind(&case.case_id)
                .bind(seq as i64)
                .bind(ev.kind())
                .bind(ev.ts_nanos_saturating())
                .bind(&payload)
                .execute(&self.pool)
                .await
                .map_err(|e| TraceError::Backend(e.to_string()))?;
            }
        }

        Ok(())
    }

    async fn finish(&self) -> Result<(), TraceError> {
        Ok(())
    }
}
