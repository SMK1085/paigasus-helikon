//! SMA-346: TokioRunner surfaces the structured AgentError as RunError::Agent.

#[path = "common/mod.rs"]
mod common;

use std::time::Duration;

use common::{noop_run_context, run_context_with_cancel, text_agent, MockModel, PendingModel};
use paigasus_helikon_core::{
    AgentError, AgentInput, CancellationToken, RunConfig, RunError, Runner,
};
use paigasus_helikon_runtime_tokio::TokioRunner;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_surfaces_model_error_as_run_error_agent() {
    // Empty scripts => model.invoke errors => AgentError::Model recorded.
    let agent = text_agent(MockModel::with_scripts(vec![]), Vec::new());
    let err = TokioRunner
        .run(
            &agent,
            noop_run_context(),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect_err("run should fail");
    assert!(
        matches!(err, RunError::Agent(AgentError::Model(_))),
        "expected RunError::Agent(AgentError::Model(..)), got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_still_maps_to_run_error_cancelled() {
    // Cancel/timeout stay runner-level (sourced from Outcome, not the slot).
    let cancel = CancellationToken::new();
    let ctx = run_context_with_cancel(cancel.clone());
    let agent = text_agent(std::sync::Arc::new(PendingModel), Vec::new());
    let res = tokio::time::timeout(Duration::from_secs(5), async {
        let run = TokioRunner.run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        );
        let killer = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        let (r, _) = tokio::join!(run, killer);
        r
    })
    .await
    .expect("within 5s");
    assert!(
        matches!(res, Err(RunError::Cancelled)),
        "cancel must remain RunError::Cancelled, got {res:?}"
    );
}
