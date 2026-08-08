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

/// Cancel an in-flight run, flush what it already produced, and wait for its detached
/// task to finish.
///
/// Draining *while* waiting is load-bearing twice over. The driver task parks on
/// `tx.send` once the bounded channel fills (a real burst of events comfortably
/// exceeds the 64-slot capacity before the pacer can drain it), so awaiting
/// `run.handle` without concurrently polling `run.rx` deadlocks the connection: nobody
/// ever frees a channel slot, the driver never returns, and the join never resolves —
/// cancelling the token does not help, since the driver is blocked *downstream* of the
/// (now-cancelled) stream and never polls it again to notice. And the buffered events
/// are the interrupted turn's tail, which the client should still receive rather than
/// lose silently — a lost tail (possibly including the turn's own terminal event)
/// would leave the client's transcript misattributing the next turn's frames to this
/// one, with no run id on the wire to tell them apart.
async fn finish_run<S>(sink: &mut S, budget: &mut FrameBudget, run: InFlight)
where
    S: futures_util::Sink<Message> + Unpin,
{
    let InFlight {
        cancel,
        mut handle,
        mut rx,
    } = run;
    cancel.cancel();
    loop {
        tokio::select! {
            maybe_ev = rx.recv() => match maybe_ev {
                Some(ev) => send_event(sink, budget, ev).await,
                // The channel closed with the join not yet observed as complete —
                // fall through to reap the (by now certainly-finished) task below.
                None => break,
            },
            // `&mut handle` (not `handle`) so a loop iteration that takes the
            // `rx.recv()` branch instead can poll the same handle again next time
            // rather than having moved it away.
            result = &mut handle => {
                let _ = result;
                // The task is done, so `tx` (owned by its async block) has already
                // been dropped; drain whatever it buffered before returning. Do not
                // await `handle` again afterward — a `JoinHandle` panics if polled
                // past completion.
                while let Some(ev) = rx.recv().await {
                    send_event(sink, budget, ev).await;
                }
                return;
            }
        }
    }
    let _ = handle.await;
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
/// own. A new message arriving while a run is in flight — or a binary frame, or the
/// connection itself ending — cancels that run and, via [`finish_run`], flushes its
/// buffered tail and *awaits its task* before starting the successor — the run's
/// finalize (and therefore its session write) happens inside that task, so starting
/// the next run first would let it load history without the interrupted turn.
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
                        if let Some(prev) = in_flight.take() {
                            finish_run(&mut sink, &mut budget, prev).await;
                        }
                        let _ = close_unsupported(&mut sink).await;
                        return;
                    }
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => continue,
                };

                // Interrupt: cancel the previous run (if any), flush its buffered
                // tail, and await its task — its finalize step must land before the
                // next run's context is built.
                if let Some(prev) = in_flight.take() {
                    finish_run(&mut sink, &mut budget, prev).await;
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
                // unconditionally. `tx.send` blocks once the 64-slot channel fills, so
                // whoever supersedes this run must keep draining `rx` (see
                // `finish_run`) rather than just awaiting `handle` — otherwise nobody
                // ever frees a slot and this task never returns.
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
        finish_run(&mut sink, &mut budget, prev).await;
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
        Agent, AgentError, AgentEvent, AgentInput, CancellationToken, ContentPart, Item,
        RunContext, TokenUsage,
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

        // The interrupted first turn's own buffered tail — here, its synthetic
        // cancellation terminal — is flushed to the client before the second turn
        // starts (see `finish_run`'s doc comment: silently dropping it would leave
        // the client's transcript misattributing the second turn's frames to the
        // first, with no run id on the wire to tell them apart). So this must read
        // *two* terminal frames, not one.
        let first_terminal = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            read_until_terminal(&mut sock),
        )
        .await
        .expect("the second message must interrupt the first turn's hang, not wait behind it");
        assert!(
            first_terminal
                .iter()
                .any(|f| f["type"] == "run_completed" || f["type"] == "run_failed"),
            "expected the interrupted first turn's own terminal frame, got {first_terminal:?}"
        );

        let second_terminal = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            read_until_terminal(&mut sock),
        )
        .await
        .expect("the second turn must complete promptly once the first turn is flushed");
        assert!(
            second_terminal
                .iter()
                .any(|f| f.to_string().contains("second")),
            "expected the second turn's echoed text, got {second_terminal:?}"
        );
    }

    /// Emits ~200 events with no per-item delay, so the driver task's `tx.send` fills
    /// the connection's bounded (64-slot) channel and parks well before the pacer
    /// (`FrameBudget`, capped at `FRAME_RATE_CAP` per second) can drain it.
    struct ChattyAgent;

    #[async_trait]
    impl Agent<()> for ChattyAgent {
        fn name(&self) -> &str {
            "chatty"
        }
        fn description(&self) -> &str {
            "test-only agent that floods the bounded event channel"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            let mut events: Vec<AgentEvent> = (0..200)
                .map(|i| AgentEvent::TokenDelta {
                    text: format!("t{i}"),
                })
                .collect();
            events.push(AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            });
            Ok(stream::iter(events).boxed())
        }
    }

    /// Regression for a deadlock: interrupting (or otherwise tearing down) a run whose
    /// driver task is parked on a full `tx.send` must not block the connection task
    /// forever. Awaiting the previous run's `JoinHandle` without concurrently draining
    /// its `rx` leaves nobody polling the channel the driver is blocked on, so the
    /// driver never returns and the await never resolves — cancelling the token does
    /// not help, since the driver is blocked *downstream* of the cancelled stream and
    /// never polls it again to notice.
    ///
    /// Wrapped in a `tokio::time::timeout`: without the fix this hangs rather than
    /// fails, so the wrapper turns that into a clean, bounded test failure instead of
    /// an indefinitely stuck test run.
    #[tokio::test]
    async fn interrupting_a_chatty_run_does_not_deadlock_the_connection() {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(ChattyAgent))
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
        sock.send(Message::text(r#"{"prompt":"a"}"#)).await.unwrap();
        sock.send(Message::text(r#"{"prompt":"b"}"#)).await.unwrap();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_until_terminal(&mut sock),
        )
        .await;
        assert!(
            result.is_ok(),
            "connection deadlocked interrupting a chatty run (timed out waiting for a terminal frame)"
        );
    }

    /// Reports the [`CancellationToken`] this run was given, via a side channel, so a
    /// test can assert on cancellation directly rather than inferring it from the
    /// agent's own behaviour (which would also depend on the runner's independent
    /// cancellation racing, an orthogonal concern this test does not need to exercise).
    struct TokenCapturingAgent {
        started: tokio::sync::mpsc::UnboundedSender<CancellationToken>,
    }

    #[async_trait]
    impl Agent<()> for TokenCapturingAgent {
        fn name(&self) -> &str {
            "token-capturing"
        }
        fn description(&self) -> &str {
            "test-only agent that reports its cancellation token then hangs"
        }
        async fn run(
            &self,
            ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            let _ = self.started.send(ctx.cancel().clone());
            Ok(stream::once(async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                }
            })
            .boxed())
        }
    }

    /// Regression: a binary frame must cancel the in-flight run before closing the
    /// connection, not just close it and leave the run to complete unattended in the
    /// background (a real model call nobody reads the output of).
    #[tokio::test]
    async fn a_binary_frame_cancels_the_in_flight_run_before_closing() {
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let server = AgentCoreServer::builder()
            .agent(Arc::new(TokenCapturingAgent {
                started: started_tx,
            }))
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
        sock.send(Message::text(r#"{"prompt":"hi"}"#))
            .await
            .unwrap();

        let token = tokio::time::timeout(std::time::Duration::from_secs(5), started_rx.recv())
            .await
            .expect("timed out waiting for the run to start")
            .expect("agent reported its cancellation token");
        assert!(
            !token.is_cancelled(),
            "sanity: the token must not already be cancelled"
        );

        sock.send(Message::binary(vec![0u8, 1, 2])).await.unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !token.is_cancelled() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the in-flight run's token must be cancelled when a binary frame arrives");
    }
}
