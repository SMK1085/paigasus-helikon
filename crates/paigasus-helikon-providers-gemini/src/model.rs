//! `GeminiModel` — public [`paigasus_helikon_core::Model`] implementation.

use std::sync::Arc;

use crate::builder::{BuildError, Config, GeminiModelBuilder, Transport};

/// Google Gemini provider (Developer API + Vertex).
#[derive(Debug, Clone)]
pub struct GeminiModel(pub(crate) Arc<Config>);

impl GeminiModel {
    /// Developer-API builder (API key).
    pub fn developer(model_id: impl Into<String>) -> GeminiModelBuilder {
        GeminiModelBuilder::new(model_id, Transport::Developer)
    }
    /// Vertex-AI builder (project + location + bearer/token-provider).
    pub fn vertex(
        model_id: impl Into<String>,
        project: impl Into<String>,
        location: impl Into<String>,
    ) -> GeminiModelBuilder {
        GeminiModelBuilder::new(
            model_id,
            Transport::Vertex {
                project: project.into(),
                location: location.into(),
            },
        )
    }
    /// Developer API from `GEMINI_API_KEY` (fallback `GOOGLE_API_KEY`).
    pub fn from_env(model_id: impl Into<String>) -> Result<Self, BuildError> {
        let key = std::env::var("GEMINI_API_KEY")
            .or_else(|_| std::env::var("GOOGLE_API_KEY"))
            .map_err(|_| BuildError::MissingApiKey)?;
        Self::developer(model_id).api_key(key).build()
    }

    pub(crate) fn from_config(cfg: Config) -> Self {
        Self(Arc::new(cfg))
    }
    /// Provider id.
    pub fn provider(&self) -> &str {
        "gemini"
    }
    /// Model id.
    pub fn model(&self) -> &str {
        &self.0.model_id
    }
}

impl GeminiModelBuilder {
    /// Validate inputs and materialize the [`GeminiModel`].
    pub fn build(self) -> Result<GeminiModel, BuildError> {
        Ok(GeminiModel::from_config(self.build_config()?))
    }
}
