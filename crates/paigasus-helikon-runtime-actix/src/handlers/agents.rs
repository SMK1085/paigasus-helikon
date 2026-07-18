//! `GET /agents` — list mounted agents.

use actix_web::web::{Data, Json};

use crate::{dto::AgentInfo, server::AppState};

/// List all mounted agents.
///
/// Order matches the underlying `HashMap` iteration order and is unspecified;
/// callers must not depend on it.
pub(crate) async fn list<Ctx: Send + Sync + 'static>(
    state: Data<AppState<Ctx>>,
) -> Json<Vec<AgentInfo>> {
    Json(
        state
            .agents
            .values()
            .map(|a| AgentInfo {
                name: a.name().to_owned(),
                description: a.description().to_owned(),
            })
            .collect(),
    )
}
