//! SMA-320 structured output: AC#1 (typed struct returned), the tools
//! two-phase path, and AC#2 (one repair then error). Tasks 6-8 add tests here.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use paigasus_helikon_core::{
    Agent, AgentInput, FinishReason, LlmAgent, ModelEvent, RunResultStreaming,
};

use common::{noop_run_context, MockModel};

#[derive(Debug, PartialEq, serde::Deserialize, schemars::JsonSchema)]
struct LeukemiaSubtypeAnalysis {
    subtype: String,
    confidence: u32,
}

fn agent_with_output<M>(model: Arc<M>) -> LlmAgent<(), M, LeukemiaSubtypeAnalysis>
where
    M: paigasus_helikon_core::Model + 'static,
{
    LlmAgent::builder::<()>()
        .name("classifier")
        .shared_model(model)
        .instructions("Classify the sample.")
        .output_type::<LeukemiaSubtypeAnalysis>()
        .build()
}

#[tokio::test]
async fn tools_two_phase_structured_output() {
    use common::MockTool;

    let tool = MockTool::new("fetch_panel", serde_json::json!({"blasts": 80}));
    let model = MockModel::with_scripts(vec![
        // turn 0: call the tool
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "c1".into(),
                name: Some("fetch_panel".into()),
                args_delta: "{}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        // turn 1: unconstrained free-text answer, no tool call
        vec![
            ModelEvent::TokenDelta {
                text: "Based on the panel, AML.".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
        // turn 2: constrained finalizing turn → structured JSON
        vec![
            ModelEvent::TokenDelta {
                text: "{\"subtype\":\"AML\",\"confidence\":88}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]);

    let agent = LlmAgent::builder::<()>()
        .name("classifier")
        .shared_model(model)
        .instructions("Classify the sample.")
        .shared_tool(tool.clone())
        .output_type::<LeukemiaSubtypeAnalysis>()
        .build();

    let stream = agent
        .run(
            noop_run_context::<()>(),
            AgentInput::from_user_text("sample"),
        )
        .await
        .expect("run starts");
    let result = RunResultStreaming::new(stream)
        .collect_typed::<LeukemiaSubtypeAnalysis>()
        .await
        .expect("collect_typed succeeds");

    assert_eq!(
        result.final_output,
        LeukemiaSubtypeAnalysis {
            subtype: "AML".into(),
            confidence: 88
        }
    );
    assert_eq!(
        tool.invocations().len(),
        1,
        "the real tool must run in phase 1"
    );
}

#[tokio::test]
async fn no_tools_structured_output_returns_struct() {
    let model = MockModel::with_scripts(vec![vec![
        ModelEvent::TokenDelta {
            text: "{\"subtype\":\"AML\",\"confidence\":92}".into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]]);
    let agent = agent_with_output(model);
    let stream = agent
        .run(
            noop_run_context::<()>(),
            AgentInput::from_user_text("sample data"),
        )
        .await
        .expect("run starts");
    let result = RunResultStreaming::new(stream)
        .collect_typed::<LeukemiaSubtypeAnalysis>()
        .await
        .expect("collect_typed succeeds");
    assert_eq!(
        result.final_output,
        LeukemiaSubtypeAnalysis {
            subtype: "AML".into(),
            confidence: 92
        }
    );
}

#[tokio::test]
async fn invalid_output_repairs_once_then_errors() {
    use paigasus_helikon_core::{AgentError, AgentEvent};

    let model = MockModel::with_scripts(vec![
        // finalizing turn: invalid (missing `confidence`)
        vec![
            ModelEvent::TokenDelta {
                text: "{\"subtype\":\"AML\"}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
        // repair turn: still invalid (not even JSON)
        vec![
            ModelEvent::TokenDelta {
                text: "sorry, I cannot".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]);
    let agent = agent_with_output(model);
    let stream = agent
        .run(
            noop_run_context::<()>(),
            AgentInput::from_user_text("sample"),
        )
        .await
        .expect("run starts");

    // Collect raw events first to assert the repair count.
    let events: Vec<AgentEvent> = {
        use futures_util::stream::StreamExt;
        stream.collect::<Vec<_>>().await
    };
    let repair_count = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::RepairStarted { .. }))
        .count();
    assert_eq!(repair_count, 1, "exactly one repair turn");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::StructuredOutputFailed { .. })),
        "a StructuredOutputFailed event must be emitted"
    );

    // Re-run to assert the typed error surface (fresh scripts).
    let model2 = MockModel::with_scripts(vec![
        vec![
            ModelEvent::TokenDelta {
                text: "{\"subtype\":\"AML\"}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
        vec![
            ModelEvent::TokenDelta {
                text: "still wrong".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]);
    let agent2 = agent_with_output(model2);
    let stream2 = agent2
        .run(
            noop_run_context::<()>(),
            AgentInput::from_user_text("sample"),
        )
        .await
        .expect("run starts");
    let err = RunResultStreaming::new(stream2)
        .collect_typed::<LeukemiaSubtypeAnalysis>()
        .await
        .expect_err("must error");
    match err {
        AgentError::InvalidStructuredOutput {
            schema_errors,
            final_text,
        } => {
            assert!(!schema_errors.is_empty());
            assert_eq!(final_text, "still wrong");
        }
        other => panic!("expected InvalidStructuredOutput, got {other:?}"),
    }
}

#[tokio::test]
async fn tool_call_on_finalizing_turn_is_a_violation() {
    use paigasus_helikon_core::AgentError;

    // No tools on the agent, so turn 0 is the finalizing turn. The model
    // (mis)behaves by emitting a tool call on both the finalizing and repair turns.
    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "x".into(),
                name: Some("nope".into()),
                args_delta: "{}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "y".into(),
                name: Some("nope".into()),
                args_delta: "{}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
    ]);
    let agent = agent_with_output(model);
    let stream = agent
        .run(
            noop_run_context::<()>(),
            AgentInput::from_user_text("sample"),
        )
        .await
        .expect("run starts");
    let err = RunResultStreaming::new(stream)
        .collect_typed::<LeukemiaSubtypeAnalysis>()
        .await
        .expect_err("must error");
    assert!(matches!(err, AgentError::InvalidStructuredOutput { .. }));
}

/// SMA-402: a structured run spans the unconstrained turn(s) + the constrained
/// finalizing turn; usage must sum across all of them, including the finalizing
/// turn. (Three turns here: tool call → unconstrained text → finalizing JSON.)
#[tokio::test]
async fn structured_run_usage_is_cumulative() {
    use common::MockTool;

    let tool = MockTool::new("fetch_panel", serde_json::json!({"blasts": 80}));
    let model = MockModel::with_scripts(vec![
        // turn 0: call the tool (usage)
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "c1".into(),
                name: Some("fetch_panel".into()),
                args_delta: "{}".into(),
            },
            ModelEvent::Usage {
                input_tokens: 50,
                output_tokens: 10,
                cached_input_tokens: Some(5),
                reasoning_tokens: Some(2),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        // turn 1: unconstrained free-text answer (usage)
        vec![
            ModelEvent::TokenDelta {
                text: "Based on the panel, AML.".into(),
            },
            ModelEvent::Usage {
                input_tokens: 60,
                output_tokens: 12,
                cached_input_tokens: Some(0),
                reasoning_tokens: Some(4),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
        // turn 2: constrained finalizing turn → structured JSON (usage)
        vec![
            ModelEvent::TokenDelta {
                text: "{\"subtype\":\"AML\",\"confidence\":88}".into(),
            },
            ModelEvent::Usage {
                input_tokens: 70,
                output_tokens: 6,
                cached_input_tokens: Some(0),
                reasoning_tokens: Some(0),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]);

    let agent = LlmAgent::builder::<()>()
        .name("classifier")
        .shared_model(model)
        .instructions("Classify the sample.")
        .shared_tool(tool.clone())
        .output_type::<LeukemiaSubtypeAnalysis>()
        .build();

    let stream = agent
        .run(
            noop_run_context::<()>(),
            AgentInput::from_user_text("sample"),
        )
        .await
        .expect("run starts");
    let result = RunResultStreaming::new(stream)
        .collect_typed::<LeukemiaSubtypeAnalysis>()
        .await
        .expect("collect_typed succeeds");

    // Sums: input 50+60+70=180, output 10+12+6=28, cached 5+0+0=5,
    // reasoning 2+4+0=6, total 60+72+76=208.
    assert_eq!(result.usage.input_tokens, 180);
    assert_eq!(result.usage.output_tokens, 28);
    assert_eq!(result.usage.cached_input_tokens, 5);
    assert_eq!(result.usage.reasoning_tokens, 6);
    assert_eq!(result.usage.total_tokens, 208);
}
