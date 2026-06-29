//! Handler for the `GET /openapi.json` endpoint.
//!
//! The entire module is compiled only when the `openapi` crate feature is active
//! (the default).  When the feature is absent the module is not included by
//! [`super`] and the route is not added to the router.

#![cfg(feature = "openapi")]

use axum::{extract::State, Json};
use utoipa::OpenApi as _;

use crate::{
    dto::{AgentInfo, AsyncAccepted, RunStatus},
    server::AppState,
};

// ── documentation stubs ───────────────────────────────────────────────────────
//
// utoipa derives path metadata from the `#[utoipa::path]` attribute placed on a
// function.  The real handlers are generic over `Ctx` and fight utoipa's proc-
// macro, so we use minimal non-generic stubs here solely to carry the attribute.
// The stubs are never called at runtime.

/// Documentation stub for `GET /agents`.
#[allow(dead_code)]
#[utoipa::path(
    get,
    path = "/agents",
    responses(
        (status = 200, description = "List of agents registered with this server",
         body = Vec<AgentInfo>)
    ),
    tag = "agents"
)]
async fn _list_agents_doc() {}

/// Documentation stub for `POST /agents/{name}/runs`.
#[allow(dead_code)]
#[utoipa::path(
    post,
    path = "/agents/{name}/runs",
    params(
        ("name" = String, Path, description = "Machine-readable name of the target agent")
    ),
    responses(
        (status = 200, description = "Completed synchronous run"),
        (status = 202, description = "Run accepted (async mode)", body = AsyncAccepted),
        (status = 404, description = "No agent with the given name is registered"),
    ),
    tag = "runs"
)]
async fn _create_run_doc() {}

/// Documentation stub for `GET /agents/{name}/runs/{id}/events`.
#[allow(dead_code)]
#[utoipa::path(
    get,
    path = "/agents/{name}/runs/{id}/events",
    params(
        ("name" = String, Path, description = "Agent name"),
        ("id" = String, Path, description = "Run identifier (UUID)")
    ),
    responses(
        (status = 200, description = "Server-Sent Events stream of AgentEvents"),
        (status = 404, description = "Run not found"),
    ),
    tag = "runs"
)]
async fn _events_doc() {}

// ── ApiDoc ────────────────────────────────────────────────────────────────────

/// OpenAPI document descriptor for the Paigasus Helikon agent server.
///
/// Aggregates the route annotations (via documentation stubs above) and DTO
/// schema definitions into a single [`utoipa::openapi::OpenApi`] base document.
/// The live handler [`openapi_json`] augments this base with the runtime agent
/// list before serving it.
#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "Paigasus Helikon Agent Server",
        description = "REST/SSE/WebSocket runtime for Paigasus Helikon agents."
    ),
    paths(_list_agents_doc, _create_run_doc, _events_doc),
    components(schemas(AgentInfo, AsyncAccepted, RunStatus))
)]
struct ApiDoc;

// ── handler ───────────────────────────────────────────────────────────────────

/// `GET /openapi.json` — serve the OpenAPI specification for this server.
///
/// Returns the static path/schema spec derived from [`ApiDoc`], augmented with
/// the runtime-mounted agent list injected into the `info.description` field.
/// Clients can therefore discover which agents are available by inspecting the
/// spec without a separate `GET /agents` call.
pub(crate) async fn openapi_json<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
) -> Json<utoipa::openapi::OpenApi> {
    let mut spec = ApiDoc::openapi();

    // Build a sorted list of mounted agent entries and append them to the
    // spec's info description so the served document reflects the live
    // server configuration.
    let mut lines: Vec<String> = state
        .agents
        .values()
        .map(|a| format!("- `{}`: {}", a.name(), a.description()))
        .collect();
    lines.sort(); // deterministic order despite HashMap iteration

    let agents_section = format!("## Mounted agents\n\n{}", lines.join("\n"));

    match spec.info.description.as_mut() {
        Some(desc) => {
            desc.push_str("\n\n");
            desc.push_str(&agents_section);
        }
        None => spec.info.description = Some(agents_section),
    }

    Json(spec)
}
