//! `.model(m).build()` without `.name(…)` first — `.build` is not
//! reachable on `LlmAgentBuilder<…, NoName, HasModel>`.

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

fn main() {
    let _ = LlmAgent::builder::<()>()
        .model(MockModel)
        .build();
}
