//! `GET /ws` — the optional WebSocket endpoint on AgentCore's HTTP-protocol contract.

use std::sync::Arc;

use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::request::Parts,
    response::Response,
};
use futures_util::{SinkExt as _, StreamExt as _};
use paigasus_helikon_core::{AgentEvent, CancellationToken};
use paigasus_helikon_runtime_axum::SessionKey;

use crate::{
    error::AgentCoreError, frame::FrameBudget, invoke::InvocationRequest, server::AppState,
    session::extract_session_id,
};

/// Maximum bytes accepted in one inbound frame, matching `/invocations`' body cap.
const MAX_INBOUND_BYTES: usize = 2 * 1024 * 1024;

/// `GET /ws` — upgrade to a WebSocket carrying the same request vocabulary as
/// `POST /invocations`.
///
/// The session id is read from the upgrade request's headers; validation is identical
/// to `/invocations`. A rejected upgrade returns the usual contract-shaped error.
///
/// Extractor order: [`WebSocketUpgrade`] and [`Parts`] both implement
/// axum's `FromRequestParts` (the latter via a blanket impl that clones the
/// in-flight request parts), so both can sit ahead of a body-consuming
/// extractor with no ordering conflict — there is no body-consuming extractor
/// here at all, since this handler never reads the request body.
/// [`extract_session_id`] borrows from `parts.headers` and is converted to an
/// owned `String` immediately, before `parts` is moved into the upgrade
/// callback below.
pub(crate) async fn ws_upgrade<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
    upgrade: WebSocketUpgrade,
    parts: Parts,
) -> Result<Response, AgentCoreError> {
    let session_id = extract_session_id(&parts.headers)?.map(str::to_owned);
    Ok(upgrade
        .max_message_size(MAX_INBOUND_BYTES)
        .on_upgrade(move |socket| connection(socket, state, parts, session_id)))
}

/// The in-flight run's cancel token, its detached driver task, and the receiving end
/// of the channel that driver forwards events through.
struct InFlight {
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
    rx: tokio::sync::mpsc::Receiver<AgentEvent>,
}

/// `select!` helper: await the in-flight run's next event, or never resolve if no run
/// is active — so `select!` simply never picks this branch while `in_flight` is `None`.
async fn recv_in_flight(in_flight: &mut Option<InFlight>) -> Option<AgentEvent> {
    match in_flight {
        Some(f) => f.rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Drive one upgraded connection: read requests, run them, stream events back.
///
/// **One run at a time, with a genuine mid-run interrupt.** Reading the next inbound
/// message and draining the in-flight run's events race via `select!` rather than
/// running one after the other: a client's next message must be observable *while* the
/// current run is still streaming, or "send a new message to interrupt the agent" (the
/// AgentCore-documented "interactive sessions with user interrupts" case) could never
/// actually happen — a connection handler that only calls `stream.next()` again once
/// the current run's channel has already closed will never notice a message that
/// arrives mid-run; it just sits unread in the socket buffer until the run ends on its
/// own. A new message arriving while a run is in flight cancels that run and *awaits
/// its task* before starting the successor — the run's finalize (and therefore its
/// session write) happens inside that task, so starting the next run first would let
/// it load history without the interrupted turn.
async fn connection<Ctx: Send + Sync + 'static>(
    socket: WebSocket,
    state: AppState<Ctx>,
    parts: Parts,
    session_id: Option<String>,
) {
    let (mut sink, mut stream) = socket.split();
    let mut budget = FrameBudget::new();
    let mut in_flight: Option<InFlight> = None;

    loop {
        tokio::select! {
            maybe_msg = stream.next() => {
                let Some(Ok(msg)) = maybe_msg else { break; };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Binary(_) => {
                        let _ = close_unsupported(&mut sink).await;
                        return;
                    }
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => continue,
                };

                // Interrupt: cancel the previous run (if any) and await its task —
                // its finalize step must land before the next run's context is built.
                if let Some(prev) = in_flight.take() {
                    prev.cancel.cancel();
                    let _ = prev.handle.await;
                }

                let request: InvocationRequest = match serde_json::from_str(&text) {
                    Ok(r) => r,
                    Err(e) => {
                        send_event(
                            &mut sink,
                            &mut budget,
                            AgentEvent::RunFailed {
                                error: format!("invalid invocation request: {e}"),
                            },
                        )
                        .await;
                        continue;
                    }
                };

                // A fresh token and a fresh RunContext per run: CancellationToken is
                // one-shot.
                let cancel = CancellationToken::new();
                let session = match state
                    .sessions
                    .session(SessionKey::new(None, session_id.as_deref()))
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        send_event(
                            &mut sink,
                            &mut budget,
                            AgentEvent::RunFailed {
                                error: e.to_string(),
                            },
                        )
                        .await;
                        continue;
                    }
                };
                let ctx = match state.context.build(&parts, session, cancel.clone()).await {
                    Ok(c) => c,
                    Err(e) => {
                        send_event(
                            &mut sink,
                            &mut budget,
                            AgentEvent::RunFailed {
                                error: e.to_string(),
                            },
                        )
                        .await;
                        continue;
                    }
                };

                let (tx, rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
                let runner = Arc::clone(&state.runner);
                let agent = Arc::clone(&state.agent);
                let run_config = state.run_config.clone();
                let input = request.into_agent_input();

                // Detached driver, exactly as `invoke.rs` does: the runner's finalize
                // step only runs when its stream is driven to termination, so drain
                // unconditionally. Dropping `rx` (e.g. when this run is itself
                // interrupted before its channel is drained here) unblocks any send
                // this loop is parked on rather than deadlocking it.
                let handle = tokio::spawn(async move {
                    let mut events = match runner
                        .run_streamed(agent.as_ref(), ctx, input, run_config)
                        .await
                    {
                        Ok(streaming) => streaming.events,
                        Err(e) => futures_util::stream::iter(vec![AgentEvent::RunFailed {
                            error: e.to_string(),
                        }])
                        .boxed(),
                    };
                    while let Some(ev) = events.next().await {
                        let _ = tx.send(ev).await;
                    }
                });

                in_flight = Some(InFlight { cancel, handle, rx });
            }
            Some(ev) = recv_in_flight(&mut in_flight) => {
                send_event(&mut sink, &mut budget, ev).await;
            }
        }
    }

    if let Some(prev) = in_flight {
        prev.cancel.cancel();
        let _ = prev.handle.await;
    }
}

/// Serialize one event through the frame budget and write every resulting frame.
async fn send_event<S>(sink: &mut S, budget: &mut FrameBudget, event: AgentEvent)
where
    S: futures_util::Sink<Message> + Unpin,
{
    let Ok(value) = serde_json::to_value(&event) else {
        return;
    };
    for frame in budget.admit(value).await {
        if sink.send(Message::text(frame)).await.is_err() {
            return;
        }
    }
}

/// Close with 1003 Unsupported Data — this endpoint has no binary input model in v0.
async fn close_unsupported<S>(sink: &mut S) -> Result<(), S::Error>
where
    S: futures_util::Sink<Message> + Unpin,
{
    sink.send(Message::Close(Some(CloseFrame {
        code: 1003,
        reason: "binary frames are not supported".into(),
    })))
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures_util::{
        stream::{self, BoxStream, StreamExt as _},
        SinkExt as _,
    };
    use paigasus_helikon_core::{
        Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
    };
    use tokio_tungstenite::tungstenite::Message;

    use crate::AgentCoreServer;

    /// Echoes the last user message back as an assistant message, so a test can prove
    /// the second request on a connection saw the first turn.
    struct EchoAgent;

    #[async_trait]
    impl Agent<()> for EchoAgent {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "test-only echo agent"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            let text = input
                .messages
                .iter()
                .filter_map(|i| match i {
                    Item::UserMessage { content } => Some(
                        content
                            .iter()
                            .filter_map(|c| match c {
                                ContentPart::Text { text } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<String>(),
                    ),
                    _ => None,
                })
                .next_back()
                .unwrap_or_default();
            Ok(stream::iter(vec![
                AgentEvent::MessageOutput {
                    item: Item::AssistantMessage {
                        content: vec![ContentPart::Text { text }],
                        agent: Some("echo".to_owned()),
                    },
                },
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                },
            ])
            .boxed())
        }
    }

    /// Bind the HTTP-protocol router on an ephemeral port and return its `ws://` URL.
    /// WebSocket upgrades cannot be exercised through `ServiceExt::oneshot`, so these
    /// tests need a real listener.
    async fn spawn_server() -> String {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(EchoAgent))
            .with_default_context()
            .build()
            .expect("server builds");
        let router = server.router();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        format!("ws://{addr}/ws")
    }

    /// Drain frames until a terminal event, returning every frame's parsed JSON.
    async fn read_until_terminal<S>(sock: &mut S) -> Vec<serde_json::Value>
    where
        S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        let mut out = Vec::new();
        while let Some(Ok(msg)) = sock.next().await {
            if let Message::Text(t) = msg {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                let terminal = matches!(
                    v["type"].as_str(),
                    Some("run_completed") | Some("run_failed")
                );
                out.push(v);
                if terminal {
                    break;
                }
            }
        }
        out
    }

    #[tokio::test]
    async fn ws_runs_an_invocation_and_streams_events() {
        let url = spawn_server().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        sock.send(Message::text(r#"{"prompt":"hello"}"#))
            .await
            .unwrap();
        let frames = read_until_terminal(&mut sock).await;
        assert!(
            frames.iter().any(|f| f["type"] == "run_completed"),
            "expected a terminal frame, got {frames:?}"
        );
    }

    /// Regression: `CancellationToken` is one-shot. A context built once per connection
    /// leaves the second run starting already-cancelled, so this asserts the *second*
    /// request on one connection completes too.
    #[tokio::test]
    async fn two_sequential_requests_on_one_connection_both_complete() {
        let url = spawn_server().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        sock.send(Message::text(r#"{"prompt":"first"}"#))
            .await
            .unwrap();
        let first = read_until_terminal(&mut sock).await;
        assert!(first.iter().any(|f| f["type"] == "run_completed"));

        sock.send(Message::text(r#"{"prompt":"second"}"#))
            .await
            .unwrap();
        let second = read_until_terminal(&mut sock).await;
        assert!(
            second.iter().any(|f| f["type"] == "run_completed"),
            "the second run must not start already-cancelled, got {second:?}"
        );
    }

    #[tokio::test]
    async fn binary_frames_are_rejected_with_close_code_1003() {
        let url = spawn_server().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        sock.send(Message::binary(vec![0u8, 1, 2])).await.unwrap();
        let mut code = None;
        while let Some(Ok(msg)) = sock.next().await {
            if let Message::Close(Some(frame)) = msg {
                code = Some(u16::from(frame.code));
                break;
            }
        }
        assert_eq!(code, Some(1003), "expected 1003 Unsupported Data");
    }

    #[tokio::test]
    async fn malformed_json_yields_an_error_frame_not_a_disconnect() {
        let url = spawn_server().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        sock.send(Message::text("not json at all")).await.unwrap();
        let mut saw_error = false;
        while let Some(Ok(msg)) = sock.next().await {
            if let Message::Text(t) = msg {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if v["type"] == "run_failed" {
                    saw_error = true;
                    break;
                }
            }
        }
        assert!(
            saw_error,
            "a bad request must surface as a run_failed frame"
        );
    }

    /// Hangs 5s on the literal prompt `"first"`; otherwise completes immediately,
    /// echoing the prompt text back so a test can tell which turn actually ran.
    struct InterruptibleAgent;

    #[async_trait]
    impl Agent<()> for InterruptibleAgent {
        fn name(&self) -> &str {
            "interruptible"
        }
        fn description(&self) -> &str {
            "test-only agent used to prove a new message interrupts a still-running turn"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            let text = input
                .messages
                .iter()
                .filter_map(|i| match i {
                    Item::UserMessage { content } => Some(
                        content
                            .iter()
                            .filter_map(|c| match c {
                                ContentPart::Text { text } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<String>(),
                    ),
                    _ => None,
                })
                .next_back()
                .unwrap_or_default();

            if text == "first" {
                Ok(stream::once(async {
                    // Long enough that a 2s test timeout always wins the race if — and
                    // only if — cancellation actually interrupts it.
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    AgentEvent::RunCompleted {
                        usage: TokenUsage::default(),
                    }
                })
                .boxed())
            } else {
                Ok(stream::iter(vec![
                    AgentEvent::MessageOutput {
                        item: Item::AssistantMessage {
                            content: vec![ContentPart::Text { text: text.clone() }],
                            agent: Some("interruptible".to_owned()),
                        },
                    },
                    AgentEvent::RunCompleted {
                        usage: TokenUsage::default(),
                    },
                ])
                .boxed())
            }
        }
    }

    /// Regression for the finalize-ordering half of the interrupt semantics: a new
    /// message arriving *while a run is still streaming* must be observable and acted
    /// on immediately, not only once the in-flight run happens to finish on its own.
    ///
    /// This is the AgentCore-documented "interactive sessions with user interrupts"
    /// case (design doc §7.2): a client sends a new turn to redirect a still-responding
    /// agent. A connection handler that only reads the next message *after* fully
    /// draining the current run's event channel can never witness this — the second
    /// message just sits unread in the socket buffer until the slow run ends on its
    /// own — so this test sends both messages back-to-back with no wait in between and
    /// requires the second turn's output within a window far shorter than the first
    /// turn's artificial hang.
    #[tokio::test]
    async fn a_new_message_interrupts_a_still_running_previous_turn() {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(InterruptibleAgent))
            .with_default_context()
            .build()
            .expect("server builds");
        let router = server.router();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let url = format!("ws://{addr}/ws");

        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        sock.send(Message::text(r#"{"prompt":"first"}"#))
            .await
            .unwrap();
        sock.send(Message::text(r#"{"prompt":"second"}"#))
            .await
            .unwrap();

        let frames = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            read_until_terminal(&mut sock),
        )
        .await
        .expect("the second message must interrupt the first turn's hang, not wait behind it");
        assert!(
            frames.iter().any(|f| f.to_string().contains("second")),
            "expected the second turn's echoed text among the post-interrupt frames, got {frames:?}"
        );
    }
}
