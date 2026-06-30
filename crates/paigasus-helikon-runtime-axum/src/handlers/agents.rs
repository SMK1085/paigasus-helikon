//! Handlers for the `/agents` resource.

use axum::{extract::State, Json};

use crate::{dto::AgentInfo, server::AppState};

/// `GET /agents` — list all agents registered with this server.
///
/// Returns a JSON array of [`AgentInfo`] objects, one per mounted agent.
/// The order is unspecified (HashMap iteration order).
pub(crate) async fn list<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
) -> Json<Vec<AgentInfo>> {
    let agents: Vec<AgentInfo> = state
        .agents
        .values()
        .map(|a| AgentInfo {
            name: a.name().to_owned(),
            description: a.description().to_owned(),
        })
        .collect();
    Json(agents)
}
