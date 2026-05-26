//! Responses API backend. Populated by SMA-316 Tasks F1+F2.

use futures_core::stream::BoxStream;
use paigasus_helikon_core::{CancellationToken, ModelError, ModelEvent, ModelRequest};

use crate::model::OpenAiModel;

/// Entry point for the Responses API. Returns Unavailable until F1+F2 land.
pub(crate) async fn invoke(
    _model: &OpenAiModel,
    _request: ModelRequest,
    _cancel: CancellationToken,
) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
    Err(ModelError::Unavailable)
}
