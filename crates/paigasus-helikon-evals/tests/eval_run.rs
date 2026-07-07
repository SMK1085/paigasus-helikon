//! EvalRun end-to-end tests over MockModel agents.

use std::sync::Arc;

use paigasus_helikon_core::{Agent, FinishReason, LlmAgent, ModelEvent};
use paigasus_helikon_evals::{
    EvalDataset, EvalRun, ExactMatch, MockModel, ScoreOutcome, ToolUseTrajectory,
};

const DATASET: &str = r#"
{"id":"a","input":"question a","expected":"answer a"}
{"id":"b","input":"question b","expected":"answer b"}
{"id":"c","input":"question c","expected":"answer c"}
"#;

fn agent_for(case_id: &str) -> Arc<dyn Agent<()>> {
    let text = format!("answer {case_id}");
    Arc::new(
        LlmAgent::builder::<()>()
            .name("echo")
            .description("echoes per case")
            .shared_model(MockModel::with_script(vec![
                ModelEvent::TokenDelta { text },
                ModelEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ]))
            .instructions("test")
            .build(),
    )
}

#[tokio::test]
async fn eval_run_is_deterministic_under_concurrency() {
    for _ in 0..3 {
        let report = EvalRun::builder()
            .dataset(EvalDataset::from_jsonl_str("t", DATASET).unwrap())
            .agent_factory(|case| agent_for(&case.id))
            .default_ctx()
            .evaluator(ExactMatch::new())
            .evaluator(ToolUseTrajectory::exact())
            .concurrency(4)
            .run()
            .await
            .unwrap();
        assert!(report.passed());
        assert_eq!(report.results.len(), 3);
        // report order matches dataset order regardless of concurrency
        assert_eq!(report.results[0].case_id, "a");
        assert_eq!(report.results[2].case_id, "c");
        // trajectory skipped (no expected_tools), exact_match passed
        let scores = &report.results[0].scores;
        assert!(scores.iter().any(
            |s| s.evaluator == "exact_match" && matches!(s.score.outcome, ScoreOutcome::Passed)
        ));
        assert!(scores.iter().any(|s| s.evaluator == "tool_trajectory"
            && matches!(s.score.outcome, ScoreOutcome::Skipped)));
        let summary = &report.summary;
        assert_eq!(summary.evaluators["exact_match"].passed, 3);
        assert_eq!(summary.evaluators["tool_trajectory"].skipped, 3);
        assert_eq!(summary.cases_passed, 3);
    }
}

#[tokio::test]
async fn eval_run_failure_and_agent_error_reported() {
    // agent answers wrong for b; agent for c errors (no scripts).
    let report = EvalRun::builder()
        .dataset(EvalDataset::from_jsonl_str("t", DATASET).unwrap())
        .agent_factory(|case| match case.id.as_str() {
            "c" => Arc::new(
                LlmAgent::builder::<()>()
                    .name("broken")
                    .description("no scripts")
                    .shared_model(MockModel::with_scripts(vec![]))
                    .instructions("test")
                    .build(),
            ) as Arc<dyn Agent<()>>,
            id => agent_for(if id == "b" { "WRONG" } else { id }),
        })
        .default_ctx()
        .evaluator(ExactMatch::new())
        .run()
        .await
        .unwrap();
    assert!(!report.passed());
    assert!(report.results[1]
        .scores
        .iter()
        .any(|s| matches!(s.score.outcome, ScoreOutcome::Failed)));
    assert!(report.results[2].error.is_some());
    assert_eq!(report.summary.cases_failed, 2);
    let table = report.render_table();
    assert!(table.contains("exact_match"));
}
