//! A scripted `Model` for deterministic replay.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::{stream, StreamExt as _};
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

use crate::{EvalError, ScriptFile};

/// A scripted [`Model`] that replays pre-recorded `ModelEvent`s: one
/// script per `invoke` call, in order. Running out of scripts yields a
/// `ModelError` — deterministic by construction.
///
/// Honors cancellation as [`Model::invoke`] requires: the stream observes
/// the token at each poll and ends on the first fired observation, without
/// emitting `Finish`. The token is *observed*, not awaited — a consumer
/// that stops polling never learns the stream has ended, which is all a
/// synchronous scripted stream can offer.
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

    /// Load the `default` scripts from a JSON script file. Files with
    /// only per-case entries yield an exhausted mock — use
    /// [`ScriptFile::load`] + [`ScriptFile::scripts_for`] for per-case
    /// selection.
    pub fn from_script_file(path: &Path) -> Result<Arc<Self>, EvalError> {
        let file = ScriptFile::load(path)?;
        Ok(Self::with_scripts(file.scripts_for("")))
    }
}

#[async_trait]
impl Model for MockModel {
    /// Pops one script and replays it.
    ///
    /// The script is popped unconditionally — a pre-cancelled `invoke`
    /// consumes its script and returns an empty stream rather than an error,
    /// so "one script per `invoke`" holds regardless of cancellation timing
    /// and exhaustion stays deterministic. This matches `RetryingModel`,
    /// which deliberately does not race `invoke` with cancellation.
    async fn invoke(
        &self,
        _request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let script = self
            .scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .ok_or_else(|| {
                ModelError::Other(anyhow::anyhow!("MockModel: no more scripted responses"))
            })?;
        // `take_while` pulls the item before testing the predicate and drops
        // it when false. Unobservable here: the stream owns the script
        // exclusively, so a dropped `ModelEvent` has no side effect.
        Ok(Box::pin(
            stream::iter(script.into_iter().map(Ok))
                .take_while(move |_| std::future::ready(!cancel.is_cancelled())),
        ))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    fn provider(&self) -> &str {
        "mock"
    }
}
