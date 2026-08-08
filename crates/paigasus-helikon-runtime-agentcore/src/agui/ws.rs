//! AG-UI `GET /ws` — bidirectional AG-UI event exchange.
//!
//! Same connection lifecycle as the HTTP-protocol `GET /ws` (`crate::ws`): a
//! `tokio::select!` loop races reading the next inbound frame against draining the
//! in-flight run's events, a fresh [`CancellationToken`] and [`RunContext`] per run (a
//! token is one-shot, so a per-connection context would leave the *second* message on
//! any connection starting already-cancelled), a 2 MiB inbound cap, and binary frames
//! closed with 1003. See `crate::ws`'s module docs for why each of those is load-bearing.
//!
//! It differs in exactly four ways:
//!
//! 1. Inbound text frames are parsed as [`RunAgentInput`] rather than
//!    [`crate::InvocationRequest`].
//! 2. Each run gets its own [`EventMapper`], seeded with `thread_id` (the header
//!    session id, else the body's `threadId`, else a fresh UUID) and `run_id` (the
//!    body's `runId`, else a fresh UUID).
//! 3. Each run resolves a **fresh, unshared session** via `InMemorySessionProvider::new(1)`
//!    rather than the server's configured session provider — see `agui::sse`'s module
//!    docs for why: AG-UI clients resend the whole conversation in `messages`, and
//!    [`Runner::run_streamed`] seeds `history ++ input.messages`, so a persisted
//!    session would double-count every prior turn.
//! 4. Outbound frames are AG-UI events (via [`EventMapper::push`]/[`EventMapper::finish`])
//!    rather than raw [`AgentEvent`] JSON, paced through a [`FrameBudget`] that splits
//!    oversize frames on their `delta` field rather than wrapping them in
//!    `helikon.chunk` envelopes.
//!
//! A body that fails to parse sends a `RUN_ERROR` frame (`VALIDATION_ERROR`) and keeps
//! the connection open, exactly like a run whose input is otherwise invalid — a
//! malformed frame is a client error, not a reason to drop the socket.
//!
//! [`Runner::run_streamed`]: paigasus_helikon_core::Runner::run_streamed

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
use paigasus_helikon_core::{AgentEvent, CancellationToken, Session};
use paigasus_helikon_runtime_axum::{InMemorySessionProvider, SessionKey, SessionProvider as _};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    agui::{
        map::EventMapper,
        types::{event, RunAgentInput},
    },
    error::AgentCoreError,
    frame::{FrameBudget, SplitStrategy},
    server::AppState,
    session::extract_session_id,
};

/// Maximum bytes accepted in one inbound frame, matching `/invocations`' body cap and
/// the HTTP-protocol `/ws`.
const MAX_INBOUND_BYTES: usize = 2 * 1024 * 1024;

/// `GET /ws` — upgrade to a WebSocket carrying [`RunAgentInput`]/AG-UI-event vocabulary.
/// See the [module docs](self) for the full contract and how it differs from the
/// HTTP-protocol `/ws` (`crate::ws::ws_upgrade`).
pub(crate) async fn ws_upgrade<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
    upgrade: WebSocketUpgrade,
    parts: Parts,
) -> Result<Response, AgentCoreError> {
    // Severed from `parts.headers` (owned, not borrowed) before `parts` moves into the
    // upgrade callback below.
    let session_id = extract_session_id(&parts.headers)?.map(str::to_owned);
    Ok(upgrade
        .max_message_size(MAX_INBOUND_BYTES)
        .on_upgrade(move |socket| connection(socket, state, parts, session_id)))
}

/// The in-flight run's cancel token, its detached driver task, the receiving end of the
/// channel that driver forwards events through, and the [`EventMapper`] translating
/// this run's [`AgentEvent`]s into AG-UI frames.
struct InFlight {
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
    rx: tokio::sync::mpsc::Receiver<AgentEvent>,
    mapper: EventMapper,
}

/// What racing the in-flight run's event channel produced.
enum InFlightOutcome {
    /// One event arrived; map and send it.
    Event(AgentEvent),
    /// The channel closed — the run's driver task has finished (with or without a
    /// terminal event ever having been observed).
    Closed,
}

/// `select!` helper: await the in-flight run's next event, or never resolve if no run
/// is active — so `select!` simply never picks this branch while `in_flight` is `None`.
async fn recv_in_flight(in_flight: &mut Option<InFlight>) -> InFlightOutcome {
    match in_flight {
        Some(f) => match f.rx.recv().await {
            Some(ev) => InFlightOutcome::Event(ev),
            None => InFlightOutcome::Closed,
        },
        None => std::future::pending().await,
    }
}

/// Cancel an in-flight run, flush what it already produced (through its own
/// [`EventMapper`]), and wait for its detached task to finish — then close any pairs
/// that run's mapper still has open.
///
/// Draining *while* waiting is load-bearing twice over; see `crate::ws::finish_run`'s
/// doc comment for the deadlock this avoids (a chatty driver parked on a full channel)
/// and why the buffered tail must reach the client rather than being dropped. The
/// closing step at the end is this endpoint's own addition: a stream this run's `Agent`
/// produced may have left a text or tool-call span open, and finishing it here — not
/// only at connection teardown — is what lets the client see it promptly rather than
/// only once the whole connection ends (which may be never).
async fn finish_run<S>(sink: &mut S, budget: &mut FrameBudget, run: InFlight)
where
    S: futures_util::Sink<Message> + Unpin,
{
    let InFlight {
        cancel,
        mut handle,
        mut rx,
        mut mapper,
    } = run;
    cancel.cancel();
    loop {
        tokio::select! {
            maybe_ev = rx.recv() => match maybe_ev {
                Some(ev) => send_mapped(sink, budget, &mut mapper, ev).await,
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
                    send_mapped(sink, budget, &mut mapper, ev).await;
                }
                send_finish(sink, budget, &mut mapper).await;
                return;
            }
        }
    }
    let _ = handle.await;
    send_finish(sink, budget, &mut mapper).await;
}

/// Drive one upgraded connection: read `RunAgentInput` requests, run them, stream AG-UI
/// events back. See the [module docs](self) for the full contract.
async fn connection<Ctx: Send + Sync + 'static>(
    socket: WebSocket,
    state: AppState<Ctx>,
    parts: Parts,
    session_id: Option<String>,
) {
    let (mut sink, mut stream) = socket.split();
    let mut budget = FrameBudget::new_with_splitter(SplitStrategy::Content { field: "delta" });
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
                // tail through its own mapper, and await its task — its finalize
                // step must land before the next run's context is built.
                if let Some(prev) = in_flight.take() {
                    finish_run(&mut sink, &mut budget, prev).await;
                }

                let input: RunAgentInput = match serde_json::from_str(&text) {
                    Ok(r) => r,
                    Err(e) => {
                        send_frames(
                            &mut sink,
                            &mut budget,
                            vec![event::run_error(
                                "VALIDATION_ERROR",
                                &format!("invalid RunAgentInput body: {e}"),
                            )],
                        )
                        .await;
                        continue;
                    }
                };

                let thread_id = session_id
                    .clone()
                    .or_else(|| input.thread_id.clone())
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                let run_id = input
                    .run_id
                    .clone()
                    .unwrap_or_else(|| Uuid::new_v4().to_string());

                // Fresh, unshared session per run — never the server's configured
                // provider. See the module docs for why.
                let session: Arc<dyn Session> = match InMemorySessionProvider::new(1)
                    .session(SessionKey::new(None, None))
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        send_frames(
                            &mut sink,
                            &mut budget,
                            vec![event::run_error("INTERNAL_ERROR", &e.to_string())],
                        )
                        .await;
                        continue;
                    }
                };

                // A fresh token and a fresh RunContext per run: CancellationToken is
                // one-shot.
                let cancel = CancellationToken::new();
                let ctx = match state.context.build(&parts, session, cancel.clone()).await {
                    Ok(c) => c,
                    Err(e) => {
                        send_frames(
                            &mut sink,
                            &mut budget,
                            vec![event::run_error("INTERNAL_ERROR", &e.to_string())],
                        )
                        .await;
                        continue;
                    }
                };

                let (tx, rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
                let runner = Arc::clone(&state.runner);
                let agent = Arc::clone(&state.agent);
                let run_config = state.run_config.clone();
                let agent_input = input.into_agent_input();

                // Detached driver, exactly as `crate::ws`'s does: the runner's
                // finalize step only runs when its stream is driven to termination,
                // so drain unconditionally. `tx.send` blocks once the 64-slot channel
                // fills, so whoever supersedes this run must keep draining `rx` (see
                // `finish_run`) rather than just awaiting `handle` — otherwise nobody
                // ever frees a slot and this task never returns.
                let handle = tokio::spawn(async move {
                    let mut events = match runner
                        .run_streamed(agent.as_ref(), ctx, agent_input, run_config)
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

                let mapper = EventMapper::new(thread_id, run_id);
                in_flight = Some(InFlight { cancel, handle, rx, mapper });
            }
            outcome = recv_in_flight(&mut in_flight) => {
                match outcome {
                    InFlightOutcome::Event(ev) => {
                        if let Some(run) = in_flight.as_mut() {
                            send_mapped(&mut sink, &mut budget, &mut run.mapper, ev).await;
                        }
                    }
                    // The channel closed on its own (not via an interrupt or a binary
                    // frame): the run's stream ended, but nothing guarantees it ended
                    // with a terminal event. Join the (by now finished) task and close
                    // whatever the mapper still has open, immediately rather than only
                    // at connection teardown, then clear `in_flight` so a later
                    // interrupt does not try to finish this run a second time.
                    InFlightOutcome::Closed => {
                        if let Some(run) = in_flight.take() {
                            let InFlight { handle, mut mapper, .. } = run;
                            let _ = handle.await;
                            send_finish(&mut sink, &mut budget, &mut mapper).await;
                        }
                    }
                }
            }
        }
    }

    if let Some(prev) = in_flight {
        finish_run(&mut sink, &mut budget, prev).await;
    }
}

/// Map one event through `mapper` and send every resulting frame.
async fn send_mapped<S>(
    sink: &mut S,
    budget: &mut FrameBudget,
    mapper: &mut EventMapper,
    ev: AgentEvent,
) where
    S: futures_util::Sink<Message> + Unpin,
{
    send_frames(sink, budget, mapper.push(&ev)).await;
}

/// Close whatever `mapper` still has open and send the resulting frames, if any.
async fn send_finish<S>(sink: &mut S, budget: &mut FrameBudget, mapper: &mut EventMapper)
where
    S: futures_util::Sink<Message> + Unpin,
{
    send_frames(sink, budget, mapper.finish()).await;
}

/// Serialize and write a batch of AG-UI frames, in order, each through the frame budget.
async fn send_frames<S>(sink: &mut S, budget: &mut FrameBudget, frames: Vec<Value>)
where
    S: futures_util::Sink<Message> + Unpin,
{
    for frame in frames {
        for wire_frame in budget.admit(frame).await {
            if sink.send(Message::text(wire_frame)).await.is_err() {
                return;
            }
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
        Agent, AgentError, AgentEvent, AgentInput, RunContext, TokenUsage,
    };
    use tokio_tungstenite::tungstenite::Message;

    use crate::AgentCoreServer;

    struct TinyAgent;

    #[async_trait]
    impl Agent<()> for TinyAgent {
        fn name(&self) -> &str {
            "tiny"
        }
        fn description(&self) -> &str {
            "emits one token then completes"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            Ok(stream::iter(vec![
                AgentEvent::TokenDelta {
                    text: "hi".to_owned(),
                },
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                },
            ])
            .boxed())
        }
    }

    async fn spawn() -> String {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(TinyAgent))
            .with_default_context()
            .build()
            .unwrap();
        let router = server.agui_router();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        format!("ws://{addr}/ws")
    }

    async fn read_until_finished<S>(sock: &mut S) -> Vec<String>
    where
        S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        let mut kinds = Vec::new();
        while let Some(Ok(msg)) = sock.next().await {
            if let Message::Text(t) = msg {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                let ty = v["type"].as_str().unwrap().to_owned();
                let done = ty == "RUN_FINISHED" || ty == "RUN_ERROR";
                kinds.push(ty);
                if done {
                    break;
                }
            }
        }
        kinds
    }

    #[tokio::test]
    async fn ws_streams_agui_events() {
        let url = spawn().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        sock.send(Message::text(
            r#"{"threadId":"t1","runId":"r1","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .await
        .unwrap();
        let kinds = read_until_finished(&mut sock).await;
        assert!(
            kinds.contains(&"TEXT_MESSAGE_START".to_owned()),
            "{kinds:?}"
        );
        assert!(kinds.contains(&"TEXT_MESSAGE_END".to_owned()), "{kinds:?}");
        assert_eq!(kinds.last().unwrap(), "RUN_FINISHED");
    }

    #[tokio::test]
    async fn two_sequential_requests_on_one_connection_both_complete() {
        let url = spawn().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        for run in ["r1", "r2"] {
            let body = format!(
                r#"{{"threadId":"t1","runId":"{run}","messages":[{{"role":"user","content":"x"}}]}}"#
            );
            sock.send(Message::text(body)).await.unwrap();
            let kinds = read_until_finished(&mut sock).await;
            assert_eq!(
                kinds.last().unwrap(),
                "RUN_FINISHED",
                "run {run}: {kinds:?}"
            );
        }
    }

    #[tokio::test]
    async fn binary_frames_are_rejected_with_close_code_1003() {
        let url = spawn().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        sock.send(Message::binary(vec![1, 2, 3])).await.unwrap();
        let mut code = None;
        while let Some(Ok(msg)) = sock.next().await {
            if let Message::Close(Some(f)) = msg {
                code = Some(u16::from(f.code));
                break;
            }
        }
        assert_eq!(code, Some(1003));
    }
}
