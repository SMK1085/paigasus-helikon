//! [`CliModel`] — the CLI's provider-dispatch enum — and the builders that
//! turn a sidecar [`ModelDef`] into a live model.
//!
//! The OpenAI and Anthropic variants resolve their API key at **build**
//! time (`OPENAI_API_KEY` / `ANTHROPIC_API_KEY` from the environment), so
//! [`build_model`] fails fast with a clear error when the key is missing.
//! The mock variant replays a recorded script file and needs no network or
//! credentials.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};
use paigasus_helikon_evals::{MockModel, ScriptFile};
use paigasus_helikon_providers_anthropic::AnthropicModel;
use paigasus_helikon_providers_openai::OpenAiModel;

use crate::sidecar::ModelDef;

/// A model resolved from a sidecar [`ModelDef`]: one enum so the rest of
/// the CLI can hold a concrete type and still dispatch across providers.
pub enum CliModel {
    /// An OpenAI model (Chat Completions backend).
    OpenAi(OpenAiModel),
    /// An Anthropic model (Messages API).
    Anthropic(AnthropicModel),
    /// A scripted mock model (deterministic replay, no network).
    Mock(Arc<MockModel>),
}

#[async_trait]
impl Model for CliModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        match self {
            CliModel::OpenAi(m) => m.invoke(request, cancel).await,
            CliModel::Anthropic(m) => m.invoke(request, cancel).await,
            CliModel::Mock(m) => m.invoke(request, cancel).await,
        }
    }

    fn capabilities(&self) -> ModelCapabilities {
        match self {
            CliModel::OpenAi(m) => m.capabilities(),
            CliModel::Anthropic(m) => m.capabilities(),
            CliModel::Mock(m) => m.capabilities(),
        }
    }

    fn provider(&self) -> &str {
        match self {
            CliModel::OpenAi(m) => m.provider(),
            CliModel::Anthropic(m) => m.provider(),
            CliModel::Mock(m) => m.provider(),
        }
    }

    fn model(&self) -> &str {
        match self {
            CliModel::OpenAi(m) => m.model(),
            CliModel::Anthropic(m) => m.model(),
            CliModel::Mock(m) => m.model(),
        }
    }
}

/// Builds a [`CliModel`] from a sidecar [`ModelDef`].
///
/// Mock script paths are resolved against `base_dir` (the sidecar's
/// directory). The mock variant replays the script file's `default`
/// scripts; use [`build_model_for_case`] for per-case selection.
pub fn build_model(def: &ModelDef, base_dir: &Path) -> anyhow::Result<CliModel> {
    match def {
        ModelDef::Openai { id } => Ok(CliModel::OpenAi(
            OpenAiModel::chat(id)
                .build()
                .with_context(|| format!("failed to build OpenAI model '{id}'"))?,
        )),
        ModelDef::Anthropic { id } => Ok(CliModel::Anthropic(
            AnthropicModel::messages(id)
                .build()
                .with_context(|| format!("failed to build Anthropic model '{id}'"))?,
        )),
        ModelDef::Mock { script } => {
            let path = base_dir.join(script);
            let model = MockModel::from_script_file(&path)
                .with_context(|| format!("failed to load mock script '{}'", path.display()))?;
            Ok(CliModel::Mock(model))
        }
    }
}

/// Builds a [`CliModel`] for one eval case.
///
/// The mock variant selects the script set recorded for `case_id`
/// (falling back to the file's `default` scripts when no per-case entry
/// exists). Non-mock providers ignore `case_id` and behave exactly like
/// [`build_model`].
pub fn build_model_for_case(
    def: &ModelDef,
    base_dir: &Path,
    case_id: &str,
) -> anyhow::Result<CliModel> {
    match def {
        ModelDef::Mock { script } => {
            let path = base_dir.join(script);
            let file = ScriptFile::load(&path)
                .with_context(|| format!("failed to load mock script '{}'", path.display()))?;
            Ok(CliModel::Mock(MockModel::with_scripts(
                file.scripts_for(case_id),
            )))
        }
        _ => build_model(def, base_dir),
    }
}
