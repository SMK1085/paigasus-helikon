//! `.build()` must be a compile error once a `ToolSource` is registered —
//! the user must call `.build_resolved().await` instead.

use std::sync::Arc;

use paigasus_helikon_core::{
    CancellationToken, LlmAgent, ModelCapabilities, ModelError, ModelEvent, ModelRequest, Tool,
    ToolSource, ToolSourceError,
};

struct MockModel;

#[async_trait::async_trait]
impl paigasus_helikon_core::Model for MockModel {
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

struct S;

#[async_trait::async_trait]
impl<Ctx: Send + Sync + 'static> ToolSource<Ctx> for S {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool<Ctx>>>, ToolSourceError> {
        Ok(vec![])
    }
}

fn main() {
    let _agent = LlmAgent::builder::<()>()
        .name("x")
        .model(MockModel)
        .tool_source(S)
        .build();
}
