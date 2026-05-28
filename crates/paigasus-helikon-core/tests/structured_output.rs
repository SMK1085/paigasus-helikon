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
