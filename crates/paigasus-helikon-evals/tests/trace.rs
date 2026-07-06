#![cfg(feature = "trace-sqlite")]
//! Trace sink round-trip tests.

use paigasus_helikon_core::{AgentEvent, TokenUsage};
use paigasus_helikon_evals::{
    CaseOutcome, CaseResult, EvaluatorScore, RunMeta, Score, SqliteTraceSink, TraceSink,
};

fn meta() -> RunMeta {
    RunMeta {
        run_id: "r1".into(),
        dataset: "d".into(),
        started_ts_nanos: 42,
    }
}

fn case_result() -> CaseResult {
    // A `MessageOutput` event built via serde, matching core's `Item` /
    // `AgentEvent` serde tagging (`#[serde(tag = "type", rename_all =
    // "snake_case")]`), so `eval_events` gets at least one row.
    let event: AgentEvent = serde_json::from_value(serde_json::json!({
        "type": "message_output",
        "item": {
            "type": "assistant_message",
            "content": [{"type": "text", "text": "hi"}],
            "agent": "a"
        }
    }))
    .unwrap();

    CaseResult {
        case_id: "c1".into(),
        outcome: Some(CaseOutcome {
            final_output: "hi".into(),
            events: vec![event],
            usage: TokenUsage::default(),
        }),
        error: None,
        scores: vec![EvaluatorScore {
            evaluator: "exact_match".into(),
            score: Score::passed(1.0),
        }],
    }
}

#[tokio::test]
async fn sqlite_sink_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("trace.db");
    let sink = SqliteTraceSink::open(&db).await.unwrap();
    sink.record_case(&meta(), &case_result()).await.unwrap();
    sink.finish().await.unwrap();

    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", db.display()))
        .await
        .unwrap();
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM eval_cases")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM eval_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(n >= 1);
}

#[cfg(feature = "trace-parquet")]
mod parquet_sink {
    use super::*;
    use paigasus_helikon_evals::ParquetTraceSink;

    #[tokio::test]
    async fn parquet_sink_writes_readable_files() {
        let dir = tempfile::tempdir().unwrap();
        let sink = ParquetTraceSink::new(dir.path()).unwrap();
        sink.record_case(&meta(), &case_result()).await.unwrap();
        sink.finish().await.unwrap();
        let events = dir.path().join("r1-events.parquet");
        let scores = dir.path().join("r1-scores.parquet");
        assert!(events.exists() && scores.exists());
        // read back with parquet's arrow reader; assert >=1 row each
        let file = std::fs::File::open(&events).unwrap();
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();
        let rows: usize = reader.map(|b| b.unwrap().num_rows()).sum();
        assert!(rows >= 1);
    }
}
