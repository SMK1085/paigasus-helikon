//! Built-in evaluator tests.

use paigasus_helikon_core::TokenUsage;
use paigasus_helikon_evals::{
    CaseOutcome, EvalCase, Evaluator, ExactMatch, JsonSchemaConformance, ScoreOutcome,
};

fn case(expected: Option<serde_json::Value>) -> EvalCase {
    EvalCase {
        id: "c1".into(),
        input: "q".into(),
        expected,
        expected_tools: None,
        metadata: serde_json::Map::new(),
    }
}

fn outcome(text: &str) -> CaseOutcome {
    CaseOutcome {
        final_output: text.into(),
        events: vec![],
        usage: TokenUsage::default(),
    }
}

#[tokio::test]
async fn exact_match_string_and_json_and_skip() {
    let e = ExactMatch::new();
    let s = e
        .evaluate(&case(Some("Hello".into())), &outcome("  Hello "))
        .await
        .unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));
    let s = e
        .evaluate(&case(Some("Hello".into())), &outcome("nope"))
        .await
        .unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Failed));
    // JSON expected → structural comparison
    let s = e
        .evaluate(
            &case(Some(serde_json::json!({"a": 1}))),
            &outcome("{ \"a\": 1 }"),
        )
        .await
        .unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));
    // absent expected → skipped
    let s = e.evaluate(&case(None), &outcome("x")).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Skipped));
    // case-insensitive option
    let s = ExactMatch::new()
        .case_insensitive()
        .evaluate(&case(Some("HELLO".into())), &outcome("hello"))
        .await
        .unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));
}

#[tokio::test]
async fn json_schema_validates() {
    let schema = serde_json::json!({"type":"object","required":["month"],"properties":{"month":{"type":"string"}}});
    let e = JsonSchemaConformance::new(schema).unwrap();
    let s = e
        .evaluate(&case(None), &outcome(r#"{"month":"June"}"#))
        .await
        .unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));
    let s = e
        .evaluate(&case(None), &outcome(r#"{"day": 3}"#))
        .await
        .unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Failed));
    assert!(s.detail.unwrap().contains("month"));
    let s = e.evaluate(&case(None), &outcome("not json")).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Failed));
}
