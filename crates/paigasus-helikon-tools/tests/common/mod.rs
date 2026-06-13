//! Shared test helpers: a deterministic `Model` that replays scripted events.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

/// A `Model` that returns one pre-scripted `Vec<ModelEvent>` per `invoke` call,
/// in order. Ignores the request — deterministic, no network.
pub struct ScriptedModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
}

impl ScriptedModel {
    /// Construct from one script (event vec) per expected turn.
    pub fn new(scripts: Vec<Vec<ModelEvent>>) -> Self {
        Self {
            scripts: Mutex::new(VecDeque::from(scripts)),
        }
    }
}

#[async_trait]
impl Model for ScriptedModel {
    async fn invoke(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ModelError::Other(anyhow::anyhow!("no more scripted responses")))?;
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}
