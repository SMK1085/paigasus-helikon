//! Parquet-backed [`TraceSink`].

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use arrow::array::{Float64Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use arrow::record_batch::RecordBatch;
use async_trait::async_trait;
use paigasus_helikon_core::SessionRecorder;
use parquet::arrow::ArrowWriter;

use super::{TraceError, TraceSink};
use crate::{CaseResult, RunMeta, ScoreOutcome};

/// One buffered row of the `<run_id>-events.parquet` table.
struct EventRow {
    run_id: String,
    case_id: String,
    seq: i64,
    kind: &'static str,
    ts_nanos: i64,
    payload: String,
}

/// One buffered row of the `<run_id>-scores.parquet` table.
struct ScoreRow {
    run_id: String,
    case_id: String,
    evaluator: String,
    value: f64,
    outcome: &'static str,
    detail: Option<String>,
}

/// Records eval traces as two Parquet files per run, written on
/// [`TraceSink::finish`]: `<run_id>-events.parquet` (one row per derived
/// [`paigasus_helikon_core::SessionEvent`]) and `<run_id>-scores.parquet`
/// (one row per [`crate::EvaluatorScore`]).
///
/// Rows are buffered in memory for the lifetime of the sink — suitable for
/// eval runs whose full trace fits in memory, not for unbounded streaming.
pub struct ParquetTraceSink {
    dir: PathBuf,
    run_id: Mutex<Option<String>>,
    events: Mutex<Vec<EventRow>>,
    scores: Mutex<Vec<ScoreRow>>,
}

impl ParquetTraceSink {
    /// Create a sink that writes into `dir` (which must already exist) on
    /// [`TraceSink::finish`].
    ///
    /// # Errors
    ///
    /// Currently infallible (`Result` is reserved for future validation,
    /// e.g. checking `dir` is writable up front).
    pub fn new(dir: &Path) -> Result<Self, TraceError> {
        Ok(Self {
            dir: dir.to_path_buf(),
            run_id: Mutex::new(None),
            events: Mutex::new(Vec::new()),
            scores: Mutex::new(Vec::new()),
        })
    }
}

/// Map a [`ScoreOutcome`] to its lowercase name. No wildcard arm: a new
/// variant must fail to compile here rather than being silently written as
/// an unrecognized string.
fn outcome_str(outcome: ScoreOutcome) -> &'static str {
    match outcome {
        ScoreOutcome::Passed => "passed",
        ScoreOutcome::Failed => "failed",
        ScoreOutcome::Skipped => "skipped",
    }
}

#[async_trait]
impl TraceSink for ParquetTraceSink {
    async fn record_case(&self, run: &RunMeta, case: &CaseResult) -> Result<(), TraceError> {
        *self.run_id.lock().expect("run_id mutex poisoned") = Some(run.run_id.clone());

        {
            let mut scores = self.scores.lock().expect("scores mutex poisoned");
            for score in &case.scores {
                scores.push(ScoreRow {
                    run_id: run.run_id.clone(),
                    case_id: case.case_id.clone(),
                    evaluator: score.evaluator.clone(),
                    value: score.score.value,
                    outcome: outcome_str(score.score.outcome),
                    detail: score.score.detail.clone(),
                });
            }
        }

        if let Some(outcome) = &case.outcome {
            let mut rec = SessionRecorder::new("eval");
            for ev in &outcome.events {
                rec.observe(ev);
            }
            let mut events = self.events.lock().expect("events mutex poisoned");
            for (seq, ev) in rec.drain().iter().enumerate() {
                let payload =
                    serde_json::to_string(ev).map_err(|e| TraceError::Backend(e.to_string()))?;
                events.push(EventRow {
                    run_id: run.run_id.clone(),
                    case_id: case.case_id.clone(),
                    seq: seq as i64,
                    kind: ev.kind(),
                    ts_nanos: ev.ts_nanos_saturating(),
                    payload,
                });
            }
        }

        Ok(())
    }

    async fn finish(&self) -> Result<(), TraceError> {
        let run_id = self.run_id.lock().expect("run_id mutex poisoned").clone();
        let Some(run_id) = run_id else {
            return Ok(());
        };

        let events = self.events.lock().expect("events mutex poisoned");
        write_events(&self.dir, &run_id, &events)?;

        let scores = self.scores.lock().expect("scores mutex poisoned");
        write_scores(&self.dir, &run_id, &scores)?;

        Ok(())
    }
}

/// Build the `events` table's schema.
fn events_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("run_id", DataType::Utf8, false),
        Field::new("case_id", DataType::Utf8, false),
        Field::new("seq", DataType::Int64, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("ts_nanos", DataType::Int64, false),
        Field::new("payload", DataType::Utf8, false),
    ]))
}

/// Build the `scores` table's schema.
fn scores_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("run_id", DataType::Utf8, false),
        Field::new("case_id", DataType::Utf8, false),
        Field::new("evaluator", DataType::Utf8, false),
        Field::new("value", DataType::Float64, false),
        Field::new("outcome", DataType::Utf8, false),
        Field::new("detail", DataType::Utf8, true),
    ]))
}

/// Write `rows` to `<dir>/<run_id>-events.parquet`.
fn write_events(dir: &Path, run_id: &str, rows: &[EventRow]) -> Result<(), TraceError> {
    let schema = events_schema();
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.run_id.as_str()),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.case_id.as_str()),
            )),
            Arc::new(Int64Array::from_iter_values(rows.iter().map(|r| r.seq))),
            Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.kind))),
            Arc::new(Int64Array::from_iter_values(
                rows.iter().map(|r| r.ts_nanos),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.payload.as_str()),
            )),
        ],
    )
    .map_err(|e| TraceError::Backend(e.to_string()))?;

    write_batch(
        &dir.join(format!("{run_id}-events.parquet")),
        schema,
        &batch,
    )
}

/// Write `rows` to `<dir>/<run_id>-scores.parquet`.
fn write_scores(dir: &Path, run_id: &str, rows: &[ScoreRow]) -> Result<(), TraceError> {
    let schema = scores_schema();
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.run_id.as_str()),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.case_id.as_str()),
            )),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.evaluator.as_str()),
            )),
            Arc::new(Float64Array::from_iter_values(rows.iter().map(|r| r.value))),
            Arc::new(StringArray::from_iter_values(
                rows.iter().map(|r| r.outcome),
            )),
            Arc::new(StringArray::from(
                rows.iter().map(|r| r.detail.as_deref()).collect::<Vec<_>>(),
            )),
        ],
    )
    .map_err(|e| TraceError::Backend(e.to_string()))?;

    write_batch(
        &dir.join(format!("{run_id}-scores.parquet")),
        schema,
        &batch,
    )
}

/// Write one [`RecordBatch`] to `path` as a single-row-group Parquet file.
fn write_batch(path: &Path, schema: SchemaRef, batch: &RecordBatch) -> Result<(), TraceError> {
    let file = File::create(path).map_err(|e| TraceError::Backend(e.to_string()))?;
    let mut writer =
        ArrowWriter::try_new(file, schema, None).map_err(|e| TraceError::Backend(e.to_string()))?;
    writer
        .write(batch)
        .map_err(|e| TraceError::Backend(e.to_string()))?;
    writer
        .close()
        .map_err(|e| TraceError::Backend(e.to_string()))?;
    Ok(())
}
