//! `POST /` — A2A's JSON-RPC 2.0 method dispatch.

use axum::{
    extract::State,
    response::{IntoResponse as _, Response},
};

use crate::server::AppState;

/// `POST /` — dispatch one JSON-RPC 2.0 request.
pub(crate) async fn dispatch<Ctx: Send + Sync + 'static>(
    State(_state): State<AppState<Ctx>>,
    _request: axum::extract::Request,
) -> Response {
    // Placeholder: the method table lands in the next commit on this branch. Answering
    // INTERNAL_ERROR keeps the route mounted and the router testable meanwhile.
    axum::Json(crate::a2a::types::JsonRpcResponse::error(
        serde_json::Value::Null,
        crate::a2a::types::rpc_error::INTERNAL_ERROR,
        "not yet implemented",
    ))
    .into_response()
}
