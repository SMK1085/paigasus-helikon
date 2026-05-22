//! Shared test fixtures for SMA-314 integration tests. Compiled once
//! per test binary via `#[path = "common/mod.rs"] mod common;` at the
//! top of each integration test file.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::collections::VecDeque;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;

use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

/// A scripted [`Model`] that emits a pre-recorded sequence of
/// [`ModelEvent`]s per call to [`Model::invoke`]. Pop one script per
/// invocation; running out of scripts yields a `ModelError`.
pub struct MockModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
}

impl MockModel {
    pub fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(VecDeque::from(scripts)),
        })
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
            .unwrap()
            .pop_front()
            .ok_or_else(|| ModelError::Other(anyhow::anyhow!("no more scripted responses")))?;
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            tools: true,
            parallel_tool_calls: true,
            ..Default::default()
        }
    }
}
