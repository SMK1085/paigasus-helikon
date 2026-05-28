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
