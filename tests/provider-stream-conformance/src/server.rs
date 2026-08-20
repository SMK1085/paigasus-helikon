//! A paced HTTP/1.1 server that plays a scripted streaming response back at a
//! real client.
//!
//! `wiremock` writes a whole body in one `set_body_raw` call, so it cannot
//! express the three endings this suite has to tell apart: a body that pauses
//! mid-flight, a body that ends cleanly, and a body that is torn down without
//! its terminating chunk. This module can, which is what lets the conformance
//! scenarios distinguish "the stream was truncated" from "the stream errored".
//!
//! Everything here talks real HTTP over a loopback socket, so the provider
//! drivers under test run their production transport unmodified.

use std::convert::Infallible;
use std::io;
use std::sync::Arc;
use std::task::Poll;

use futures_util::stream::{self, BoxStream, Stream, StreamExt};
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::GateHandle;

/// One frame of the scripted body: either bytes, or the failure that ends the
/// response uncleanly.
type ScriptFrame = Result<Frame<Bytes>, io::Error>;

/// The response body type. Boxing the stream keeps this nameable, which the
/// `service_fn` return type needs.
type ScriptBody = StreamBody<BoxStream<'static, ScriptFrame>>;

/// How a scripted response body terminates once every chunk has been written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// End the body normally. hyper writes the terminating chunk, so the client
    /// observes an ordinary end-of-stream.
    Clean,
    /// End the body uncleanly. The body stream yields an error, so hyper tears
    /// the connection down *without* the terminating chunk and the client
    /// observes a transport error rather than an end-of-stream. This is the
    /// distinction the error scenarios rest on.
    Abort,
}

/// One scripted streaming response.
#[derive(Debug, Clone)]
pub struct Script {
    /// Value for the response's `content-type` header, e.g.
    /// `"text/event-stream"`.
    pub content_type: &'static str,
    /// Body chunks, handed to the response body in order. Each is written as
    /// its own HTTP chunk, so a fixture can split a wire event wherever the
    /// scenario needs it split.
    pub chunks: Vec<Vec<u8>>,
    /// Pause once this many chunks have been handed over, and resume only when
    /// the gate is released. `Some(0)` pauses before the first chunk;
    /// `Some(chunks.len())` pauses after the last one but before the ending, so
    /// the body is still open. `None` sends everything without pausing. A value
    /// above `chunks.len()` never pauses, which makes the gate a no-op — keep it
    /// within `0..=chunks.len()`.
    pub gate_after: Option<usize>,
    /// How the body terminates once every chunk has been written.
    pub ending: Ending,
}

/// A running paced server, bound to an ephemeral loopback port.
///
/// The accept loop is spawned detached and is *not* stopped when this value is
/// dropped: a caller may hand a still-streaming response back to its own caller
/// and drop the server, and the response must keep flowing. The listener is
/// released when the test process exits.
pub struct PacedServer {
    /// The ephemeral port the listener bound to.
    port: u16,
    /// Present exactly when the script set `gate_after`.
    gate: Option<GateHandle>,
}

/// What the request handler needs, shared across the connections of one server.
struct Shared {
    /// The script being played.
    script: Script,
    /// Taken by the first request; a `oneshot` can only be awaited once, and
    /// one server plays its script to one request.
    gate_rx: Mutex<Option<oneshot::Receiver<()>>>,
}

impl PacedServer {
    /// Bind an ephemeral loopback port and start serving `script`.
    ///
    /// Every request gets the same script, but the gate — if the script has one
    /// — fires for the first request only.
    pub async fn start(script: Script) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("paced server should bind an ephemeral loopback port");
        let port = listener
            .local_addr()
            .expect("bound listener should have a local address")
            .port();

        let (gate, gate_rx) = if script.gate_after.is_some() {
            let (tx, rx) = oneshot::channel();
            (Some(GateHandle { tx }), Some(rx))
        } else {
            (None, None)
        };

        let shared = Arc::new(Shared {
            script,
            gate_rx: Mutex::new(gate_rx),
        });

        tokio::spawn(async move {
            loop {
                let (stream, _peer) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    // A peer that vanished between the SYN and the accept kills
                    // only that connection. Bailing out here would drop the
                    // listener, and every later connect would get
                    // ECONNREFUSED — which reads as a provider bug in the
                    // subject tasks rather than as a dead test server.
                    Err(err) if is_transient_accept_error(&err) => continue,
                    Err(_) => break,
                };
                let shared = Arc::clone(&shared);
                tokio::spawn(async move {
                    let service =
                        service_fn(move |req: Request<Incoming>| respond(Arc::clone(&shared), req));
                    // No `graceful_shutdown`: the abort ending has to be an
                    // unclean termination, and a graceful shutdown would flush
                    // a well-formed end of body first.
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });

        Self { port, gate }
    }

    /// The origin to point a client at, e.g. `http://127.0.0.1:52413`.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Take the gate, if the script declared one. Releasing the returned handle
    /// lets the server send the chunks it is holding back. Returns `None` on
    /// the second call, and for a script with no `gate_after`.
    pub fn take_gate(&mut self) -> Option<GateHandle> {
        self.gate.take()
    }
}

/// Handle one request: drain its body, then stream the script back.
async fn respond(
    shared: Arc<Shared>,
    req: Request<Incoming>,
) -> Result<Response<ScriptBody>, Infallible> {
    // Drain the request body before responding. A client that is still writing
    // when the connection goes away can see a reset instead of the response, and
    // that would present as a provider bug.
    let _ = req.into_body().collect().await;

    let gate_rx = shared.gate_rx.lock().await.take();

    // Unbounded on purpose. A bounded channel would add back-pressure that
    // usually — but only usually — keeps the abort error from reaching hyper in
    // the same poll pass as the last chunk, which silently masks a missing
    // deferral in `script_frames` and makes that bug unreproducible in a test.
    // With no back-pressure, the deferral is the single mechanism responsible
    // for the abort flush, and its removal fails a test every time.
    let (tx, rx) = mpsc::unbounded_channel::<ScriptFrame>();
    let feeder = Arc::clone(&shared);
    tokio::spawn(async move { feed(feeder, gate_rx, tx).await });

    let body = StreamBody::new(script_frames(rx).boxed());

    Ok(Response::builder()
        .status(200)
        .header("content-type", shared.script.content_type)
        .body(body)
        .expect("scripted response should build"))
}

/// Turn the feeder's channel into the stream hyper writes from, deferring the
/// abort error by exactly one poll.
///
/// That deferral is load-bearing. hyper's connection loop runs `poll_write`
/// then `poll_flush`, and an error out of the body escapes `poll_write` before
/// `poll_flush` is reached in the same pass. So if the error were handed over in
/// the same pass that produced the last chunk, hyper would tear the connection
/// down with the response head and every chunk still sitting in its write
/// buffer, and the client would see a connection that closed before it ever got
/// a response — not the truncated body the abort scenarios need. Returning
/// `Pending` once (with an immediate self-wake) forces the intervening flush, so
/// the client receives the head and every chunk, and only then loses the
/// connection mid-body.
///
/// The guarantee has a limit worth knowing before you lean on it: this forces
/// hyper to *call* `poll_flush`, not to have drained the socket. A scripted body
/// larger than the socket send buffer (~64 KB on loopback) can still lose its
/// tail when the abort lands, because the flush would return `Pending` with
/// bytes still buffered. Every conformance fixture is orders of magnitude
/// smaller than that; if you add a large abort fixture, re-check this.
fn script_frames(
    mut rx: mpsc::UnboundedReceiver<ScriptFrame>,
) -> impl Stream<Item = ScriptFrame> + Send {
    let mut deferred: Option<io::Error> = None;
    stream::poll_fn(move |cx| {
        if let Some(err) = deferred.take() {
            return Poll::Ready(Some(Err(err)));
        }
        match rx.poll_recv(cx) {
            Poll::Ready(Some(Err(err))) => {
                // Defer by one poll so hyper flushes what it has already
                // written before this error tears the connection down. Removing
                // this makes the client receive an empty body on every abort;
                // `abort_delivers_buffered_chunks_before_erroring` is the test
                // that catches it.
                deferred = Some(err);
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            other => other,
        }
    })
}

/// Push the script's chunks into `tx`, pausing at the gate, then apply the
/// ending.
async fn feed(
    shared: Arc<Shared>,
    mut gate_rx: Option<oneshot::Receiver<()>>,
    tx: mpsc::UnboundedSender<ScriptFrame>,
) {
    let script = &shared.script;

    for (already_sent, chunk) in script.chunks.iter().enumerate() {
        if script.gate_after == Some(already_sent) {
            wait_for_gate(&mut gate_rx).await;
        }
        if tx
            .send(Ok(Frame::data(Bytes::from(chunk.clone()))))
            .is_err()
        {
            // The client hung up, or the connection died. Nothing left to play.
            return;
        }
    }

    if script.gate_after == Some(script.chunks.len()) {
        wait_for_gate(&mut gate_rx).await;
    }

    match script.ending {
        // Dropping the sender ends the stream, and hyper writes the terminating
        // chunk.
        Ending::Clean => drop(tx),
        // An error out of the body makes hyper abandon the connection without
        // the terminating chunk, so the client sees a transport error.
        Ending::Abort => {
            let _ = tx.send(Err(io::Error::other("aborted")));
        }
    }
}

/// Whether an `accept` error killed only one connection rather than the
/// listener.
fn is_transient_accept_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::Interrupted
    )
}

/// Await the gate, if there is one left to await. A dropped `GateHandle`
/// resolves the receiver with an error, which releases the body too — a test
/// that forgets to release should not hang forever.
async fn wait_for_gate(gate_rx: &mut Option<oneshot::Receiver<()>>) {
    if let Some(rx) = gate_rx.take() {
        let _ = rx.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A clean ending must deliver every chunk and terminate the body normally.
    #[tokio::test]
    async fn clean_ending_delivers_all_chunks() {
        let server = PacedServer::start(Script {
            content_type: "text/event-stream",
            chunks: vec![b"data: one\n\n".to_vec(), b"data: two\n\n".to_vec()],
            gate_after: None,
            ending: Ending::Clean,
        })
        .await;

        let body = reqwest::Client::new()
            .post(server.base_url())
            .send()
            .await
            .expect("request should succeed")
            .text()
            .await
            .expect("clean body should read to completion");

        assert_eq!(body, "data: one\n\ndata: two\n\n");
    }

    /// An aborted ending must surface as a transport error, not a clean EOF.
    /// This is what separates scenario S4 from S3.
    #[tokio::test]
    async fn abort_ending_surfaces_as_an_error() {
        let server = PacedServer::start(Script {
            content_type: "text/event-stream",
            chunks: vec![b"data: one\n\n".to_vec()],
            gate_after: None,
            ending: Ending::Abort,
        })
        .await;

        let result = reqwest::Client::new()
            .post(server.base_url())
            .send()
            .await
            .expect("headers should arrive")
            .text()
            .await;

        assert!(
            result.is_err(),
            "an aborted body must not read as a clean EOF, got {result:?}"
        );
    }

    /// The abort path owes the client *both* halves of the contract: every
    /// chunk written before the abort, and then an error rather than a clean
    /// EOF. `abort_ending_surfaces_as_an_error` only checks the second half, so
    /// it passes even when the client receives an empty body — which is exactly
    /// what happens if the body error is allowed to escape hyper's `poll_write`
    /// before `poll_flush` runs. This test is what pins the deferral in
    /// `script_frames`.
    #[tokio::test]
    async fn abort_delivers_buffered_chunks_before_erroring() {
        let server = PacedServer::start(Script {
            content_type: "text/event-stream",
            chunks: vec![b"data: one\n\n".to_vec(), b"data: two\n\n".to_vec()],
            gate_after: None,
            ending: Ending::Abort,
        })
        .await;

        let mut frames = reqwest::Client::new()
            .post(server.base_url())
            .send()
            .await
            .expect("headers should arrive")
            .bytes_stream();

        let mut received: Vec<u8> = Vec::new();
        let mut ended_with_error = false;
        while let Some(frame) = frames.next().await {
            match frame {
                Ok(bytes) => received.extend_from_slice(&bytes),
                Err(_) => {
                    ended_with_error = true;
                    break;
                }
            }
        }

        assert_eq!(
            String::from_utf8_lossy(&received),
            "data: one\n\ndata: two\n\n",
            "chunks written before the abort must reach the client"
        );
        assert!(
            ended_with_error,
            "the abort must still surface as an error, not a clean EOF"
        );
    }

    /// The gate owes two things, and the cancel scenarios in the later subject
    /// tasks depend on both: the pre-gate bytes must actually reach the client
    /// (they fire cancellation only after observing an event, which cannot
    /// happen if nothing arrived), and everything after the gate must stay
    /// unsent so the translator is still holding buffered state when they do.
    ///
    /// Part (a) is a positive assertion on purpose. Asserting only that the body
    /// has *not* finished is one-directional — on a machine slow enough to miss
    /// the window a completely broken gate passes — and the trailing body
    /// comparison cannot rescue it, because a broken gate produces
    /// byte-identical output. Waiting for the prefix to arrive fails loudly
    /// instead.
    #[tokio::test]
    async fn gate_delivers_prefix_then_withholds_until_released() {
        // Generous: exceeding this means the bytes never came, not that the
        // machine was busy. Only ever reached on failure, so it costs nothing.
        const ARRIVAL: Duration = Duration::from_secs(5);
        // Short: here a timeout is the passing outcome, so the test waits this
        // long on every run.
        const WITHHOLD: Duration = Duration::from_millis(300);

        let mut server = PacedServer::start(Script {
            content_type: "text/event-stream",
            chunks: vec![b"data: one\n\n".to_vec(), b"data: two\n\n".to_vec()],
            gate_after: Some(1),
            ending: Ending::Clean,
        })
        .await;
        let gate = server.take_gate().expect("gate_after was set");

        let mut frames = Box::pin(
            reqwest::Client::new()
                .post(server.base_url())
                .send()
                .await
                .expect("headers should arrive")
                .bytes_stream(),
        );

        // (a) The pre-gate chunk must actually reach the client.
        let prefix = tokio::time::timeout(ARRIVAL, accumulate(&mut frames, "data: one\n\n".len()))
            .await
            .expect("pre-gate chunk never reached the client");
        assert_eq!(prefix, "data: one\n\n");

        // (b) Everything after the gate must be withheld.
        assert!(
            tokio::time::timeout(WITHHOLD, frames.next()).await.is_err(),
            "gate did not withhold the post-gate chunk"
        );

        // (c) Releasing delivers the remainder and ends the body cleanly.
        gate.release();
        let rest = tokio::time::timeout(ARRIVAL, drain(&mut frames))
            .await
            .expect("body should complete after release");
        assert_eq!(rest, "data: two\n\n");
    }

    /// Pull frames until at least `want` bytes have arrived. Panics rather than
    /// returning short, so a caller's `assert_eq!` never compares a truncated
    /// read against the expected prefix.
    async fn accumulate<S>(frames: &mut S, want: usize) -> String
    where
        S: Stream<Item = reqwest::Result<Bytes>> + Unpin,
    {
        let mut buf: Vec<u8> = Vec::new();
        while buf.len() < want {
            match frames.next().await {
                Some(Ok(bytes)) => buf.extend_from_slice(&bytes),
                Some(Err(err)) => panic!("stream errored before the prefix arrived: {err}"),
                None => panic!("stream ended before the prefix arrived"),
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Read the rest of the body to its clean end.
    async fn drain<S>(frames: &mut S) -> String
    where
        S: Stream<Item = reqwest::Result<Bytes>> + Unpin,
    {
        let mut buf: Vec<u8> = Vec::new();
        while let Some(frame) = frames.next().await {
            buf.extend_from_slice(&frame.expect("body should not error after release"));
        }
        String::from_utf8_lossy(&buf).into_owned()
    }
}
