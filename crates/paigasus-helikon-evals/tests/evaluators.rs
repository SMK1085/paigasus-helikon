//! Built-in evaluator tests.

use std::sync::Arc;

use paigasus_helikon_core::{AgentEvent, FinishReason, Item, ModelEvent, TokenUsage};
use paigasus_helikon_evals::{
    CaseOutcome, EvalCase, Evaluator, ExactMatch, JsonSchemaConformance, LlmJudge, MockModel,
    ScoreOutcome, ToolUseTrajectory,
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

fn tool_call_event(name: &str) -> AgentEvent {
    AgentEvent::ToolCallItem {
        item: Item::ToolCall {
            call_id: "c".into(),
            name: name.into(),
            args: serde_json::json!({}),
        },
    }
}

#[tokio::test]
async fn llm_judge_parses_score_and_thresholds() {
    let judge_reply = r#"{"score": 0.9, "reasoning": "solid"}"#;
    let model = MockModel::with_script(vec![
        ModelEvent::TokenDelta {
            text: judge_reply.into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]);
    let judge = LlmJudge::new(model.clone() as Arc<dyn paigasus_helikon_core::Model>)
        .rubric("Is the answer helpful?");
    let s = judge
        .evaluate(&case(Some("ref".into())), &outcome("answer"))
        .await
        .unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));
    assert!((s.value - 0.9).abs() < 1e-9);

    let model = MockModel::with_script(vec![
        ModelEvent::TokenDelta {
            text: r#"{"score": 0.2, "reasoning": "weak"}"#.into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]);
    let judge = LlmJudge::new(model as Arc<dyn paigasus_helikon_core::Model>).threshold(0.5);
    let s = judge
        .evaluate(&case(None), &outcome("answer"))
        .await
        .unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Failed));
}

#[tokio::test]
async fn trajectory_modes_and_handoff_filter() {
    let mut c = case(None);
    c.expected_tools = Some(vec!["lookup_spending".into(), "send_report".into()]);

    let mut o = outcome("x");
    o.events = vec![
        tool_call_event("transfer_to_budgeting"), // filtered by default
        tool_call_event("lookup_spending"),
        tool_call_event("send_report"),
    ];
    let s = ToolUseTrajectory::exact().evaluate(&c, &o).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));

    // in_order: extra tool between expected ones still passes
    let mut o2 = outcome("x");
    o2.events = vec![
        tool_call_event("lookup_spending"),
        tool_call_event("noise"),
        tool_call_event("send_report"),
    ];
    assert!(matches!(
        ToolUseTrajectory::exact()
            .evaluate(&c, &o2)
            .await
            .unwrap()
            .outcome,
        ScoreOutcome::Failed
    ));
    assert!(matches!(
        ToolUseTrajectory::in_order()
            .evaluate(&c, &o2)
            .await
            .unwrap()
            .outcome,
        ScoreOutcome::Passed
    ));

    // skip without expected_tools
    let s = ToolUseTrajectory::exact()
        .evaluate(&case(None), &o)
        .await
        .unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Skipped));

    // include_handoffs keeps transfer tools
    let mut c2 = case(None);
    c2.expected_tools = Some(vec![
        "transfer_to_budgeting".into(),
        "lookup_spending".into(),
        "send_report".into(),
    ]);
    let s = ToolUseTrajectory::exact()
        .include_handoffs()
        .evaluate(&c2, &o)
        .await
        .unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));

    // empty expected_tools means "no tools expected"
    let mut c3 = case(None);
    c3.expected_tools = Some(vec![]);
    let s = ToolUseTrajectory::exact()
        .evaluate(&c3, &outcome("x"))
        .await
        .unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));
}
