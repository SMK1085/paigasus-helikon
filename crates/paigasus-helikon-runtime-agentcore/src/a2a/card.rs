//! `GET /.well-known/agent-card.json` — A2A's discovery document.
//!
//! # How the card is derived
//!
//! When no card is installed via
//! [`AgentCoreServerBuilder::agent_card`](crate::AgentCoreServerBuilder::agent_card),
//! one is derived from the configured agent:
//!
//! | Card field | Source |
//! | --- | --- |
//! | `name` | the agent's `name()` |
//! | `description` | the agent's `description()` |
//! | `version` | **this crate's** version |
//! | `url` | `AGENTCORE_RUNTIME_URL`, else the builder's `agent_card_url`, else omitted |
//! | `protocolVersion` | `0.3.0` |
//! | `preferredTransport` | `JSONRPC` |
//! | `capabilities.streaming` | `true` — `message/stream` is always mounted |
//! | `defaultInputModes` / `defaultOutputModes` | `["text"]`; this runtime handles text |
//! | `skills` | one skill mirroring the agent's name and description |
//!
//! `version` reports the version of `paigasus-helikon-runtime-agentcore`, not of the
//! agent: a library cannot read its host binary's version, and inventing one would be
//! worse than reporting a true-but-narrow fact. A deployment that needs to publish its
//! own version supplies a complete card through `agent_card`.
//!
//! `url` is omitted rather than guessed. The server binds `0.0.0.0`, which is a bind
//! address and not somewhere a client can connect, so publishing it on a *discovery*
//! document would send callers to an unroutable address.

use axum::{extract::State, Json};

use crate::{
    a2a::types::{AgentCapabilities, AgentCard, AgentSkill},
    server::AppState,
};

/// A2A protocol version this card advertises.
const PROTOCOL_VERSION: &str = "0.3.0";

/// Transport clients should prefer. This container speaks JSON-RPC 2.0 only.
const PREFERRED_TRANSPORT: &str = "JSONRPC";

/// Environment variable AgentCore sets to the runtime's public URL, when it sets one.
const RUNTIME_URL_ENV: &str = "AGENTCORE_RUNTIME_URL";

/// `GET /.well-known/agent-card.json` — see the [module docs](self) for the derivation.
pub(crate) async fn agent_card<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
) -> Json<AgentCard> {
    if let Some(card) = state.card.clone() {
        return Json(card);
    }

    let url = std::env::var(RUNTIME_URL_ENV)
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(|| state.card_url.clone());

    let name = state.agent.name().to_owned();
    let description = state.agent.description().to_owned();

    Json(AgentCard {
        skills: vec![AgentSkill {
            id: name.clone(),
            name: name.clone(),
            description: description.clone(),
            tags: vec![],
        }],
        name,
        description,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        url,
        protocol_version: PROTOCOL_VERSION.to_owned(),
        preferred_transport: PREFERRED_TRANSPORT.to_owned(),
        capabilities: AgentCapabilities { streaming: true },
        default_input_modes: vec!["text".to_owned()],
        default_output_modes: vec!["text".to_owned()],
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use futures_util::stream::{self, BoxStream, StreamExt as _};
    use paigasus_helikon_core::{
        Agent, AgentError, AgentEvent, AgentInput, RunContext, TokenUsage,
    };
    use tower::ServiceExt as _;

    use crate::AgentCoreServer;

    struct NamedAgent;

    #[async_trait]
    impl Agent<()> for NamedAgent {
        fn name(&self) -> &str {
            "invoice-reconciler"
        }
        fn description(&self) -> &str {
            "reconciles invoices against statements"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            Ok(stream::iter(vec![AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            }])
            .boxed())
        }
    }

    async fn fetch_card(server: &AgentCoreServer<()>) -> serde_json::Value {
        let resp = server
            .a2a_router()
            .oneshot(
                Request::builder()
                    .uri("/.well-known/agent-card.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn card_is_derived_from_the_configured_agent() {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(NamedAgent))
            .with_default_context()
            .build()
            .unwrap();
        let card = fetch_card(&server).await;
        assert_eq!(card["name"], "invoice-reconciler");
        assert_eq!(
            card["description"],
            "reconciles invoices against statements"
        );
        assert_eq!(card["protocolVersion"], "0.3.0");
        assert_eq!(card["preferredTransport"], "JSONRPC");
        assert_eq!(card["capabilities"]["streaming"], true);
        assert_eq!(
            card["skills"][0]["id"], "invoice-reconciler",
            "an empty skills array is valid but useless for discovery"
        );
    }

    /// `0.0.0.0` is a bind address, not a routable URL; publishing it on a discovery
    /// card would be actively misleading, so an unknown url is omitted instead.
    #[tokio::test]
    async fn url_is_omitted_when_nothing_authoritative_is_known() {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(NamedAgent))
            .with_default_context()
            .build()
            .unwrap();
        let card = fetch_card(&server).await;
        assert!(card.get("url").is_none(), "card: {card}");
    }

    #[tokio::test]
    async fn explicit_card_url_is_published() {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(NamedAgent))
            .with_default_context()
            .agent_card_url("https://example.invalid/runtimes/x/invocations/")
            .build()
            .unwrap();
        let card = fetch_card(&server).await;
        assert_eq!(
            card["url"],
            "https://example.invalid/runtimes/x/invocations/"
        );
    }

    #[tokio::test]
    async fn an_explicit_card_replaces_the_derived_one() {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(NamedAgent))
            .with_default_context()
            .agent_card(crate::AgentCard {
                name: "custom".to_owned(),
                description: "hand-written".to_owned(),
                version: "9.9.9".to_owned(),
                url: None,
                protocol_version: "0.3.0".to_owned(),
                preferred_transport: "JSONRPC".to_owned(),
                capabilities: crate::AgentCapabilities { streaming: true },
                default_input_modes: vec!["text".to_owned()],
                default_output_modes: vec!["text".to_owned()],
                skills: vec![],
            })
            .build()
            .unwrap();
        let card = fetch_card(&server).await;
        assert_eq!(card["name"], "custom");
        assert_eq!(card["version"], "9.9.9");
    }

    #[tokio::test]
    async fn ping_is_reachable_on_the_a2a_router() {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(NamedAgent))
            .with_default_context()
            .build()
            .unwrap();
        let resp = server
            .a2a_router()
            .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
