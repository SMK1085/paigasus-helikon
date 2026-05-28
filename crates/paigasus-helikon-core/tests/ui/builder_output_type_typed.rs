//! `.output_type::<Answer>()` produces an `LlmAgent<MyCtx, MockModel, Answer>`.
//! Binding to the explicit type proves T flows through the builder to the
//! agent.

use paigasus_helikon_core::{
    CancellationToken, LlmAgent, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

struct MockModel;

#[async_trait::async_trait]
impl Model for MockModel {
    async fn invoke(
        &self,
        _r: ModelRequest,
        _c: CancellationToken,
    ) -> Result<
        futures_core::stream::BoxStream<'static, Result<ModelEvent, ModelError>>,
        ModelError,
    > {
        Err(ModelError::Unavailable)
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct Answer {
    #[allow(dead_code)]
    value: u32,
}

fn main() {
    let _: LlmAgent<(), MockModel, Answer> = LlmAgent::builder::<()>()
        .name("triage")
        .model(MockModel)
        .output_type::<Answer>()
        .build();
}
