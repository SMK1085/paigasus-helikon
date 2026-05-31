//! `OpenAiModel` — the public [`paigasus_helikon_core::Model`]
//! implementation. Internally dispatches via a [`Backend`] enum to the
//! Chat-Completions or Responses-API code paths.

use async_openai::config::OpenAIConfig;
use async_openai::Client;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

use crate::builder::OpenAiModelBuilder;
use crate::capabilities::Backend;

/// OpenAI provider — supports both Chat Completions and the Responses API.
///
/// Construct via [`Self::chat`] or [`Self::responses`].
#[derive(Debug)]
pub struct OpenAiModel {
    pub(crate) model_id: String,
    pub(crate) backend: Backend,
    pub(crate) client: Client<OpenAIConfig>,
    pub(crate) capabilities: ModelCapabilities,
}

impl OpenAiModel {
    /// Construct a Chat Completions model builder.
    pub fn chat(model_id: impl Into<String>) -> OpenAiModelBuilder {
        OpenAiModelBuilder::new(model_id, Backend::Chat)
    }

    /// Construct a Responses API model builder.
    pub fn responses(model_id: impl Into<String>) -> OpenAiModelBuilder {
        OpenAiModelBuilder::new(model_id, Backend::Responses)
    }

    pub(crate) fn new(
        model_id: String,
        backend: Backend,
        client: Client<OpenAIConfig>,
        capabilities: ModelCapabilities,
    ) -> Self {
        Self {
            model_id,
            backend,
            client,
            capabilities,
        }
    }
}

#[async_trait]
impl Model for OpenAiModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        match self.backend {
            Backend::Chat => crate::backend::chat::invoke(self, request, cancel).await,
            Backend::Responses => crate::backend::responses::invoke(self, request, cancel).await,
        }
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.capabilities
    }

    fn provider(&self) -> &str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.model_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_and_model_getters() {
        let m = OpenAiModel::chat("gpt-4o")
            .api_key("sk-test")
            .build()
            .unwrap();
        assert_eq!(m.provider(), "openai");
        assert_eq!(m.model(), "gpt-4o");
    }
}
