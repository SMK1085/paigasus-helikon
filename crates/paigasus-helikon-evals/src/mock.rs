//! A scripted `Model` for deterministic replay.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

use crate::{EvalError, ScriptFile};

/// A scripted [`Model`] that replays pre-recorded `ModelEvent`s: one
/// script per `invoke` call, in order. Running out of scripts yields a
/// `ModelError` — deterministic by construction.
pub struct MockModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
}

impl MockModel {
    /// A mock that answers exactly one `invoke` with `script`.
    pub fn with_script(script: Vec<ModelEvent>) -> Arc<Self> {
        Self::with_scripts(vec![script])
    }

    /// A mock that answers successive `invoke`s with successive scripts.
    pub fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(VecDeque::from(scripts)),
        })
    }

    /// Load the `default` scripts from a JSON script file.
    pub fn from_script_file(path: &Path) -> Result<Arc<Self>, EvalError> {
        let file = ScriptFile::load(path)?;
        Ok(Self::with_scripts(file.scripts_for("")))
    }
}

#[async_trait]
impl Model for MockModel {
    async fn invoke(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let script = self
            .scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .ok_or_else(|| {
                ModelError::Other(anyhow::anyhow!("MockModel: no more scripted responses"))
            })?;
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    fn provider(&self) -> &str {
        "mock"
    }
}
