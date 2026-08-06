//! End-to-end matrix for the principal↔session binding (CWE-639).
//!
//! Every row is driven over real HTTP against a real server. Isolation is not
//! asserted by status code — a `200` proves nothing about *which* session was
//! resolved. Instead the mounted agent echoes the conversation the runner loaded
//! for the request, so one principal's text appearing in another principal's
//! response body is a direct, observable session collision.

use std::{net::SocketAddr, sync::Arc};

use async_trait::async_trait;
use axum::http::request::Parts;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
};
use paigasus_helikon_runtime_axum::{
    AgentServer, AgentServerBuilder, AuthLayer, AuthRejection, Principal,
};

/// The exact body of the fail-closed 403.
///
/// Pinned here (and identically in the actix crate and the cross-runtime parity
/// suite) so the two runtimes cannot drift. It renders in full because 4xx
/// bodies are deliberately not redacted — the caller already knows what it sent.
const UNBOUND_403_BODY: &str =
    r#"{"error":"unauthorized: session id requires an authenticated principal (403 Forbidden)"}"#;

// ── auth layer ────────────────────────────────────────────────────────────────

/// Admits every request, and establishes a [`Principal`] only when the
/// `X-Test-Principal` header is present.
///
/// The "admitted but principal-less" case is the whole point: it is what a real
/// deployment produces when its auth layer authenticates a shared API key, or a
/// service account, without resolving it to a per-caller identity.
struct HeaderPrincipalAuth;

#[async_trait]
impl AuthLayer for HeaderPrincipalAuth {
    async fn authenticate(&self, parts: &mut Parts) -> Result<(), AuthRejection> {
        if let Some(value) = parts.headers.get("x-test-principal") {
            if let Ok(s) = value.to_str() {
                parts.extensions.insert(Principal(s.to_owned()));
            }
        }
        Ok(())
    }
}

// ── history-echo agent ────────────────────────────────────────────────────────

/// Concatenate every text block of `content` into `out`, `|`-separated.
fn push_text(out: &mut Vec<String>, content: &[ContentPart]) {
    for part in content {
        if let ContentPart::Text { text } = part {
            out.push(text.clone());
        }
    }
}

/// An agent that echoes the **merged conversation** — the session history the
/// runner loaded plus this turn's input — back as its assistant message.
///
/// This is what makes session isolation observable over HTTP: the run's
/// `output` field carries the loaded history verbatim, so if two callers
/// collided on one session id the second caller's response would contain the
/// first caller's text.
struct HistoryEchoAgent;

#[async_trait]
impl Agent<()> for HistoryEchoAgent {
    fn name(&self) -> &str {
        "history"
    }

    fn description(&self) -> &str {
        "echoes the merged conversation (session history + this turn)"
    }

    async fn run(
        &self,
        _ctx: RunContext<()>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let mut parts = Vec::new();
        for item in &input.messages {
            match item {
                Item::UserMessage { content } => push_text(&mut parts, content),
                Item::AssistantMessage { content, .. } => push_text(&mut parts, content),
                _ => {}
            }
        }
        Ok(stream::iter(vec![
            AgentEvent::MessageOutput {
                item: Item::AssistantMessage {
                    content: vec![ContentPart::Text {
                        text: parts.join("|"),
                    }],
                    agent: None,
                },
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ])
        .boxed())
    }
}

// ── harness ───────────────────────────────────────────────────────────────────

/// Boot a server mounting the `history` agent, applying `configure` to the
/// builder first, and return its bound address.
async fn spawn_history_server(
    configure: impl FnOnce(AgentServerBuilder<()>) -> AgentServerBuilder<()>,
) -> SocketAddr {
    let builder = AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(HistoryEchoAgent));
    let server = configure(builder).build().expect("server builds");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        server
            .serve_with_listener(listener)
            .await
            .expect("serve loop");
    });
    addr
}

/// `POST /agents/history/runs` with the optional principal and session headers.
async fn post_run(
    addr: SocketAddr,
    principal: Option<&str>,
    session_id: Option<&str>,
    input: &str,
) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .post(format!("http://{addr}/agents/history/runs"))
        .header("content-type", "application/json")
        .body(format!(r#"{{"input":"{input}"}}"#));
    if let Some(p) = principal {
        request = request.header("x-test-principal", p);
    }
    if let Some(s) = session_id {
        request = request.header("x-session-id", s);
    }
    request.send().await.expect("run request sent")
}

/// The `output` field of a 200 run response — the conversation the runner loaded
/// for that request.
async fn output_of(response: reqwest::Response) -> String {
    let body: serde_json::Value = response.json().await.expect("run response is JSON");
    body["output"]
        .as_str()
        .unwrap_or_else(|| panic!("run response carries a string output: {body}"))
        .to_owned()
}

// ── row: no auth layer ────────────────────────────────────────────────────────

/// **No `AuthLayer` configured** — the gate is off by default, so a bare
/// `X-Session-Id` is honoured and every principal-less caller shares one
/// namespace. This is the pre-existing single-tenant behaviour, and it must not
/// regress into a 403 for servers that never authenticate.
#[tokio::test]
async fn without_auth_layer_session_ids_are_a_shared_namespace() {
    let addr = spawn_history_server(|b| b).await;

    let first = post_run(addr, None, Some("shared"), "first-turn").await;
    assert_eq!(first.status(), 200, "unauthenticated server must not 403");
    let _ = output_of(first).await;

    let second = post_run(addr, None, Some("shared"), "second-turn").await;
    assert_eq!(second.status(), 200);
    let history = output_of(second).await;
    assert!(
        history.contains("first-turn"),
        "the shared namespace must still resume the same conversation; got {history:?}"
    );
}

// ── row: auth + principal + id ────────────────────────────────────────────────

/// **The IDOR itself.** Two principals presenting the SAME `X-Session-Id` must
/// not reach the same conversation. Proven by content, not status: mallory's
/// response must not contain alice's text, while alice's own second request
/// must (the positive control that rules out "isolation" by way of no session
/// affinity at all).
#[tokio::test]
async fn same_session_id_different_principals_are_isolated() {
    let addr = spawn_history_server(|b| b.auth(Arc::new(HeaderPrincipalAuth))).await;

    let alice = post_run(addr, Some("alice"), Some("shared"), "alice-secret").await;
    assert_eq!(alice.status(), 200, "alice run status");
    let _ = output_of(alice).await;

    let mallory = post_run(addr, Some("mallory"), Some("shared"), "mallory-probe").await;
    assert_eq!(mallory.status(), 200, "mallory run status");
    let mallory_history = output_of(mallory).await;
    assert!(
        !mallory_history.contains("alice-secret"),
        "mallory read alice's conversation — sessions collided; got {mallory_history:?}"
    );

    // Positive control: affinity still holds WITHIN a principal.
    let alice_again = post_run(addr, Some("alice"), Some("shared"), "alice-again").await;
    assert_eq!(alice_again.status(), 200);
    let alice_history = output_of(alice_again).await;
    assert!(
        alice_history.contains("alice-secret"),
        "alice lost her own conversation; got {alice_history:?}"
    );
    assert!(
        !alice_history.contains("mallory-probe"),
        "alice read mallory's conversation; got {alice_history:?}"
    );
}

// ── row: auth + no principal + id ─────────────────────────────────────────────

/// **Fail closed.** An admitted caller with no established principal that names
/// a session is refused, because it would otherwise join the namespace shared by
/// every other principal-less caller.
#[tokio::test]
async fn named_session_without_principal_is_403() {
    let addr = spawn_history_server(|b| b.auth(Arc::new(HeaderPrincipalAuth))).await;

    let resp = post_run(addr, None, Some("victim-session"), "hi").await;
    assert_eq!(resp.status(), 403, "unbound named session must be 403");
    assert_eq!(
        resp.text().await.expect("403 body"),
        UNBOUND_403_BODY,
        "the 403 body is pinned so the two runtimes cannot drift"
    );
}

// ── row: auth + no principal + no id ──────────────────────────────────────────

/// **Anonymous stays anonymous.** With no `X-Session-Id` there is nothing to
/// bind, so a principal-less caller is served normally — with a *fresh* session
/// each time, never a shared one.
#[tokio::test]
async fn no_principal_and_no_session_id_gets_a_fresh_session() {
    let addr = spawn_history_server(|b| b.auth(Arc::new(HeaderPrincipalAuth))).await;

    let first = post_run(addr, None, None, "anon-one").await;
    assert_eq!(first.status(), 200, "anonymous request must be admitted");
    assert_eq!(output_of(first).await, "anon-one");

    let second = post_run(addr, None, None, "anon-two").await;
    assert_eq!(second.status(), 200);
    let history = output_of(second).await;
    assert_eq!(
        history, "anon-two",
        "anonymous sessions must never be shared or stored"
    );
}

// ── row: allow_unbound_sessions() ─────────────────────────────────────────────

/// **Opt-out.** `allow_unbound_sessions()` turns the 403 back into the
/// pre-existing shared-namespace behaviour, and `require_principal(false)` is
/// documented as equivalent — both are asserted so the two setters cannot drift.
#[tokio::test]
async fn allow_unbound_sessions_permits_the_otherwise_403_request() {
    for (label, addr) in [
        (
            "allow_unbound_sessions()",
            spawn_history_server(|b| {
                b.auth(Arc::new(HeaderPrincipalAuth))
                    .allow_unbound_sessions()
            })
            .await,
        ),
        (
            "require_principal(false)",
            spawn_history_server(|b| {
                b.auth(Arc::new(HeaderPrincipalAuth))
                    .require_principal(false)
            })
            .await,
        ),
    ] {
        let first = post_run(addr, None, Some("shared"), "unbound-one").await;
        assert_eq!(first.status(), 200, "{label} must suppress the 403");
        let _ = output_of(first).await;

        let second = post_run(addr, None, Some("shared"), "unbound-two").await;
        assert_eq!(second.status(), 200, "{label} second request");
        let history = output_of(second).await;
        assert!(
            history.contains("unbound-one"),
            "{label} must restore the shared namespace; got {history:?}"
        );
    }
}

/// **Opting out of the 403 does not opt out of the keying.** With
/// `allow_unbound_sessions()` a caller that *does* carry a `Principal` is still
/// isolated to it — including from the principal-less namespace using the same
/// id.
#[tokio::test]
async fn allow_unbound_sessions_still_isolates_principals() {
    let addr = spawn_history_server(|b| {
        b.auth(Arc::new(HeaderPrincipalAuth))
            .allow_unbound_sessions()
    })
    .await;

    let alice = post_run(addr, Some("alice"), Some("shared"), "alice-secret").await;
    assert_eq!(alice.status(), 200);
    let _ = output_of(alice).await;

    let mallory = post_run(addr, Some("mallory"), Some("shared"), "mallory-probe").await;
    assert_eq!(mallory.status(), 200);
    let mallory_history = output_of(mallory).await;
    assert!(
        !mallory_history.contains("alice-secret"),
        "allow_unbound_sessions() must not collapse the compound key; got {mallory_history:?}"
    );

    // The principal-less caller is its own namespace too — not a wildcard into
    // alice's.
    let unbound = post_run(addr, None, Some("shared"), "unbound-probe").await;
    assert_eq!(unbound.status(), 200);
    let unbound_history = output_of(unbound).await;
    assert!(
        !unbound_history.contains("alice-secret"),
        "a principal-less caller reached alice's session; got {unbound_history:?}"
    );
}

// ── row: require_principal(true) with no auth layer ───────────────────────────

/// **Embedded topology.** A host application that authenticates for the server
/// configures no `AuthLayer` here, so the default gate would be off. Setting
/// `require_principal(true)` explicitly must enforce the 403 anyway.
#[tokio::test]
async fn require_principal_without_an_auth_layer_still_403s() {
    let addr = spawn_history_server(|b| b.require_principal(true)).await;

    let resp = post_run(addr, None, Some("victim-session"), "hi").await;
    assert_eq!(
        resp.status(),
        403,
        "require_principal(true) must hold without an AuthLayer"
    );
    assert_eq!(resp.text().await.expect("403 body"), UNBOUND_403_BODY);

    // A request with no session id is still fine.
    let anon = post_run(addr, None, None, "anon").await;
    assert_eq!(anon.status(), 200, "no session id, nothing to bind");
}

// ── row: non-UTF-8 X-Session-Id ───────────────────────────────────────────────

/// A present-but-non-UTF-8 `X-Session-Id` is a `400`, not a silent `None`.
///
/// The server is deliberately the authenticated, gated one: an implementation
/// that coerced the unreadable header to `None` would *skip the fail-closed
/// gate* and answer `200`, so this row distinguishes the correct 400 from that
/// specific bug rather than merely from a crash.
#[tokio::test]
async fn non_utf8_session_id_is_400() {
    let addr = spawn_history_server(|b| b.auth(Arc::new(HeaderPrincipalAuth))).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/history/runs"))
        .header("content-type", "application/json")
        .header(
            "x-session-id",
            reqwest::header::HeaderValue::from_bytes(b"\xff\xfe").expect("opaque header bytes"),
        )
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .expect("run request sent");

    assert_eq!(
        resp.status(),
        400,
        "a non-UTF-8 session id must be rejected, never coerced to `None`"
    );
    let body = resp.text().await.expect("400 body");
    assert!(
        body.contains("not valid UTF-8"),
        "the 400 must name the malformed header; got {body}"
    );
}
