//! Chat Completions backend. Populated by SMA-316 Tasks E1+E2.

use futures_core::stream::BoxStream;
use paigasus_helikon_core::{CancellationToken, ModelError, ModelEvent, ModelRequest};

use crate::model::OpenAiModel;

/// Entry point for Chat Completions. Returns Unavailable until E1+E2 land.
pub(crate) async fn invoke(
    _model: &OpenAiModel,
    _request: ModelRequest,
    _cancel: CancellationToken,
) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
    Err(ModelError::Unavailable)
}
