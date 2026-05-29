//! Smoke tests for [`paigasus_helikon_runtime_tokio::TokioRunner`].

#[path = "common/mod.rs"]
mod common;

use paigasus_helikon_core::{AgentInput, RunConfig, Runner};
use paigasus_helikon_runtime_tokio::TokioRunner;

use common::{noop_run_context, text_agent, MockModel};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_returns_final_output() {
    let agent = text_agent(MockModel::quick_hi(), Vec::new());
    let result = TokioRunner
        .run(
            &agent,
            noop_run_context(),
            AgentInput::from_user_text("yo"),
            RunConfig::default(),
        )
        .await
        .expect("run ok");
    assert_eq!(result.final_output, "hi");
}
