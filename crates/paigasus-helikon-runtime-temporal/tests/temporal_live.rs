#![allow(missing_docs)]

//! Live Temporal integration tests for the durable runtime.
//!
//! NOT `#[ignore]`'d: they compile on every PR (so they cannot bit-rot) and
//! skip LOUDLY when no Temporal server is configured. Run against a local dev
//! server with `TEMPORAL_TEST_SERVER=<host:port>` (e.g. `localhost:7233`):
//!
//! ```text
//! temporal server start-dev --headless
//! TEMPORAL_TEST_SERVER=localhost:7233 \
//!   cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1
//! ```
//!
//! Each test mints a unique task queue (uuid), so `--test-threads=1` is a
//! belt-and-braces guard for the shared dev server rather than a correctness
//! requirement. The headline test, [`crash_resume_mid_tool_call`], is the
//! SMA-332 acceptance criterion: it aborts a worker mid-tool-call and proves a
//! fresh worker on the same queue resumes the run from durable history (the
//! turn-0 model call is NOT re-executed) rather than restarting it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;
use paigasus_helikon_core::{
    AgentEvent, AgentInput, CancellationToken, ContentPart, FinishReason, Item, LlmAgent,
    MemorySession, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest, RunConfig,
    RunContext, RunError, Runner, Session, Tool, ToolContext, ToolError, ToolOutput,
};
use paigasus_helikon_runtime_temporal::runner::{TemporalRunner, TemporalRunnerConfig};
use paigasus_helikon_runtime_temporal::worker::{RetryPolicyConfig, TemporalAgentWorker};
use temporalio_client::{Client, ClientOptions, Connection, ConnectionOptions};

// ---------------------------------------------------------------------------
// Env gate (loud-skip pattern; mirrors paigasus-helikon-tools/tests/forkd_live.rs)
// ---------------------------------------------------------------------------

/// Returns the configured Temporal server address (`host:port`), or prints a
/// loud skip message and returns `None`.
fn gate() -> Option<String> {
    match std::env::var("TEMPORAL_TEST_SERVER") {
        Ok(addr) if !addr.is_empty() => Some(addr),
        _ => {
            eprintln!(
                "SKIPPED: set TEMPORAL_TEST_SERVER=<host:port> (e.g. localhost:7233) and start a \
                 dev server (`temporal server start-dev --headless`) to run the live Temporal suite"
            );
            None
        }
    }
}

/// Connect a fresh client to the `default` namespace on `addr`.
async fn connect(addr: &str) -> Client {
    let target = url::Url::parse(&format!("http://{addr}")).expect("valid temporal target url");
    let connection = Connection::connect(ConnectionOptions::new(target).build())
        .await
        .expect("connect to the Temporal dev server");
    Client::new(connection, ClientOptions::new("default").build()).expect("build Temporal client")
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Concatenate the text parts of a content vec.
fn text_of(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Poll until `path` exists or the deadline passes (panics on timeout).
async fn wait_for_file(path: &Path, within: Duration) {
    let start = std::time::Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < within,
            "timed out after {within:?} waiting for flag file {path:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A unique path under the temp dir that does not yet exist.
fn fresh_temp_path(prefix: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::remove_file(&p);
    p
}

/// A `Model` whose per-turn behavior is driven by the conversation it receives,
/// so it is deterministic under Temporal history replay:
///
/// - if `tool_name` is set and the request carries no `ToolResult` yet → emit
///   one tool call to `tool_name` (turn 0);
/// - otherwise → emit `final_text` and stop.
///
/// `calls` counts every `invoke`; `requests` records each request's messages.
/// Both are shared handles the test asserts on. Because a call served from
/// workflow history never reaches the worker, `calls` counts only turns that
/// actually executed a model activity — the crux of the crash-resume proof.
struct ScriptedModel {
    calls: Arc<AtomicU32>,
    requests: RequestLog,
    tool_name: Option<String>,
    final_text: String,
}

/// Shared handle recording each model request's message list (one entry per
/// `invoke`).
type RequestLog = Arc<Mutex<Vec<Vec<Item>>>>;

/// Build a [`ScriptedModel`] plus its shared `(calls, requests)` handles.
fn scripted_model(
    tool_name: Option<&str>,
    final_text: &str,
) -> (ScriptedModel, Arc<AtomicU32>, RequestLog) {
    let calls = Arc::new(AtomicU32::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = ScriptedModel {
        calls: Arc::clone(&calls),
        requests: Arc::clone(&requests),
        tool_name: tool_name.map(str::to_owned),
        final_text: final_text.to_owned(),
    };
    (model, calls, requests)
}

#[async_trait]
impl Model for ScriptedModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.messages.clone());

        let has_tool_result = request
            .messages
            .iter()
            .any(|m| matches!(m, Item::ToolResult { .. }));

        let events: Vec<Result<ModelEvent, ModelError>> = match (&self.tool_name, has_tool_result) {
            (Some(tool), false) => vec![
                Ok(ModelEvent::ToolCallDelta {
                    call_id: "c1".to_owned(),
                    name: Some(tool.clone()),
                    args_delta: "{}".to_owned(),
                }),
                Ok(ModelEvent::Finish {
                    reason: FinishReason::ToolCalls,
                }),
            ],
            _ => vec![
                Ok(ModelEvent::TokenDelta {
                    text: self.final_text.clone(),
                }),
                Ok(ModelEvent::Finish {
                    reason: FinishReason::Stop,
                }),
            ],
        };
        Ok(Box::pin(stream::iter(events)))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// A `Model` whose activity blocks until the activity's own cancellation fires,
/// then ends its stream cleanly (so the worker-side activity winds down instead
/// of orphaning). Touches `entered_path` when it starts, so a test can wait
/// until the run is genuinely mid-model-call before cancelling.
struct BlockUntilCancelModel {
    entered_path: PathBuf,
}

#[async_trait]
impl Model for BlockUntilCancelModel {
    async fn invoke(
        &self,
        _request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        // Signal "the model activity is running now".
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.entered_path);
        Ok(Box::pin(stream::once(async move {
            cancel.cancelled().await;
            Ok(ModelEvent::Finish {
                reason: FinishReason::Stop,
            })
        })))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// A tool that blocks forever on its FIRST invocation across worker generations
/// (creating `flag_path`), then returns instantly on every subsequent one. The
/// flag lives on disk so it survives the in-process worker "crash" (task
/// `abort()`), making the retry — served by a fresh worker — return promptly.
/// `invocations` counts every `invoke` call.
struct BlockOnceTool {
    schema: serde_json::Value,
    flag_path: PathBuf,
    invocations: Arc<AtomicU32>,
}

/// Build a [`BlockOnceTool`] plus its shared invocation counter.
fn block_once_tool(flag_path: PathBuf) -> (BlockOnceTool, Arc<AtomicU32>) {
    let invocations = Arc::new(AtomicU32::new(0));
    let tool = BlockOnceTool {
        schema: serde_json::json!({ "type": "object" }),
        flag_path,
        invocations: Arc::clone(&invocations),
    };
    (tool, invocations)
}

#[async_trait]
impl Tool<()> for BlockOnceTool {
    fn name(&self) -> &str {
        "blockonce"
    }
    fn description(&self) -> &str {
        "blocks forever on its first invocation across worker generations, then returns instantly"
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext<()>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        // `create_new` atomically claims the "first invocation" slot: it succeeds
        // exactly once (across all worker generations, since the flag is on
        // disk). The winner blocks forever and is aborted mid-block; the retry
        // finds the flag present and returns instantly.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.flag_path)
        {
            Ok(_) => {
                std::future::pending::<()>().await;
                unreachable!("pending() never resolves");
            }
            Err(_) => Ok(ToolOutput::new(serde_json::json!({ "ok": true }))),
        }
    }
}

/// A tool that returns instantly and counts its invocations.
struct EchoTool {
    schema: serde_json::Value,
    invocations: Arc<AtomicU32>,
}

/// Build an [`EchoTool`] plus its shared invocation counter.
fn echo_tool() -> (EchoTool, Arc<AtomicU32>) {
    let invocations = Arc::new(AtomicU32::new(0));
    let tool = EchoTool {
        schema: serde_json::json!({ "type": "object" }),
        invocations: Arc::clone(&invocations),
    };
    (tool, invocations)
}

#[async_trait]
impl Tool<()> for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "returns instantly"
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext<()>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::new(serde_json::json!({ "echoed": true })))
    }
}

/// A `Model` whose `invoke` fails with a transport error — exercises the
/// non-retryable model-failure path (ADR-10) end-to-end so the workflow's
/// activity-failure cause-chain extraction can be validated live.
struct FailingModel;

#[async_trait]
impl Model for FailingModel {
    async fn invoke(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        Err(ModelError::Transport("connection lost".to_owned()))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// A tool that blocks forever on EVERY invocation — every activity attempt hits
/// its start-to-close timeout, so with a small `max_attempts` the tool activity
/// fails at the infra level (exhausted retries), exercising the workflow's
/// tool-activity-failure fold-in path.
struct AlwaysBlockTool {
    schema: serde_json::Value,
}

#[async_trait]
impl Tool<()> for AlwaysBlockTool {
    fn name(&self) -> &str {
        "alwaysblock"
    }
    fn description(&self) -> &str {
        "blocks forever on every invocation"
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext<()>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        std::future::pending::<()>().await;
        unreachable!("pending() never resolves");
    }
}

/// A running worker on its own dedicated OS thread + tokio runtime.
///
/// The temporalio SDK worker future is `!Send` (its executor holds `Rc`/
/// `RefCell` state), so it cannot be `tokio::spawn`ed onto the shared
/// multi-thread test runtime — each worker generation gets its own thread with
/// its own runtime. [`WorkerHandle::stop`] simulates a worker "crash": it drops
/// the worker (abandoning any in-flight activity, which then re-dispatches on
/// its start-to-close timeout) and joins the thread so a fresh generation never
/// races a half-dead one.
struct WorkerHandle {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WorkerHandle {
    /// Stop the worker (drop it) and wait for its thread to tear down. Async so
    /// the blocking join is offloaded off the test reactor.
    async fn stop(mut self) {
        let shutdown = self.shutdown.take();
        let thread = self.thread.take();
        let _ = tokio::task::spawn_blocking(move || {
            drop(shutdown); // dropping the sender resolves the worker's shutdown branch
            if let Some(t) = thread {
                let _ = t.join();
            }
        })
        .await;
    }
}

/// Start a worker on `queue` serving `agent` (short activity timeouts + fast
/// tool retry policy) on a dedicated thread. It runs until [`WorkerHandle::stop`]
/// (or the process exits).
fn start_worker<M: Model + 'static>(
    addr: String,
    queue: String,
    agent: Arc<LlmAgent<(), M, String>>,
    tool_start_to_close: Duration,
    model_start_to_close: Duration,
    tool_max_attempts: u32,
) -> WorkerHandle {
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("worker thread runtime builds");
        rt.block_on(async move {
            let worker = TemporalAgentWorker::builder::<()>()
                .task_queue(&queue)
                .client(connect(&addr).await)
                .with_ctx(|| ())
                .register(agent)
                .expect("agent registers on the worker")
                .tool_start_to_close(tool_start_to_close)
                .model_start_to_close(model_start_to_close)
                .tool_retry_policy(RetryPolicyConfig {
                    initial_interval: Some(Duration::from_millis(200)),
                    maximum_attempts: Some(tool_max_attempts),
                    ..Default::default()
                })
                .build()
                .expect("worker builds");
            tokio::select! {
                res = worker.run() => { let _ = res; }
                _ = shutdown_rx => { /* "crash": drop the worker below */ }
            }
        });
    });
    WorkerHandle {
        shutdown: Some(shutdown_tx),
        thread: Some(thread),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Happy path: a two-turn run (tool call, then final text) completes with the
/// scripted final output, the model activity ran exactly twice, and the tool
/// ran once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn happy_path_tool_roundtrip() {
    let Some(addr) = gate() else {
        return;
    };
    let queue = format!("helikon-happy-{}", uuid::Uuid::new_v4());

    let (model, model_calls, _requests) = scripted_model(Some("echo"), "final-answer");
    let (tool, tool_invocations) = echo_tool();
    let agent = Arc::new(
        LlmAgent::builder::<()>()
            .name("happy")
            .model(model)
            .tool(tool)
            .build(),
    );

    let worker = start_worker(
        addr.clone(),
        queue.clone(),
        Arc::clone(&agent),
        Duration::from_secs(5),
        Duration::from_secs(10),
        5,
    );

    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let runner = TemporalRunner::new(
        connect(&addr).await,
        TemporalRunnerConfig::new(queue.clone()),
    );
    let result = tokio::time::timeout(
        Duration::from_secs(60),
        runner.run(
            agent.as_ref(),
            RunContext::ephemeral(()).with_session(Arc::clone(&session)),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        ),
    )
    .await
    .expect("happy-path run completes within 60s");

    worker.stop().await;

    let run = result.expect("happy-path run is Ok");
    assert_eq!(run.final_output, "final-answer");
    assert_eq!(
        model_calls.load(Ordering::SeqCst),
        2,
        "one model call for the tool-call turn + one for the final turn"
    );
    assert_eq!(tool_invocations.load(Ordering::SeqCst), 1, "tool ran once");
    assert!(
        matches!(run.events.first(), Some(AgentEvent::RunStarted { .. })),
        "first event is RunStarted: {:?}",
        run.events
    );
    assert!(
        run.events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolCallItem { .. } | AgentEvent::ToolOutputItem { .. }
        )),
        "events carry the tool round-trip: {:?}",
        run.events
    );
    assert!(
        matches!(run.events.last(), Some(AgentEvent::RunCompleted { .. })),
        "last event is RunCompleted: {:?}",
        run.events
    );
}

/// THE ACCEPTANCE CRITERION (SMA-332): a worker aborted mid-tool-call is
/// replaced by a fresh worker on the same queue, and the run resumes from
/// durable history rather than restarting.
///
/// Proof: the turn-0 `call_model` activity completed (and was written to
/// history) BEFORE the tool blocked, so on resume it is served from history and
/// the model is invoked exactly twice total (once per turn), never three times.
/// The tool is invoked twice: the blocked first attempt plus the successful
/// retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_resume_mid_tool_call() {
    let Some(addr) = gate() else {
        return;
    };
    let queue = format!("helikon-crash-{}", uuid::Uuid::new_v4());
    let flag_path = fresh_temp_path("helikon-blockonce");

    let (model, model_calls, _requests) = scripted_model(Some("blockonce"), "resumed-done");
    let (tool, tool_invocations) = block_once_tool(flag_path.clone());
    let agent = Arc::new(
        LlmAgent::builder::<()>()
            .name("crasher")
            .model(model)
            .tool(tool)
            .build(),
    );

    // Start the run first (it starts the workflow on the server; the workflow
    // makes progress as soon as a worker polls the queue).
    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let runner = TemporalRunner::new(
        connect(&addr).await,
        TemporalRunnerConfig::new(queue.clone()),
    );
    let agent_for_run = Arc::clone(&agent);
    let run_handle = tokio::spawn(async move {
        runner
            .run(
                agent_for_run.as_ref(),
                RunContext::ephemeral(()).with_session(session),
                AgentInput::from_user_text("go"),
                RunConfig::default(),
            )
            .await
    });

    // Worker generation 1: picks up the run, completes turn-0 model call, then
    // blocks in the tool.
    let worker1 = start_worker(
        addr.clone(),
        queue.clone(),
        Arc::clone(&agent),
        Duration::from_secs(5),
        Duration::from_secs(10),
        5,
    );

    // Wait until the tool is genuinely mid-block, then "crash" the worker.
    wait_for_file(&flag_path, Duration::from_secs(60)).await;
    worker1.stop().await;

    // Worker generation 2: fresh worker on the SAME queue. The tool's
    // start-to-close (5s) times out attempt 1, the retry lands here, and the
    // workflow replays turn-0 from history (model NOT re-invoked).
    let worker2 = start_worker(
        addr.clone(),
        queue.clone(),
        Arc::clone(&agent),
        Duration::from_secs(5),
        Duration::from_secs(10),
        5,
    );

    let result = tokio::time::timeout(Duration::from_secs(120), run_handle)
        .await
        .expect("crash-resume run completes within 120s")
        .expect("run task did not panic");

    worker2.stop().await;
    let _ = std::fs::remove_file(&flag_path);

    let run = result.expect("run resumes and completes Ok after the crash");
    assert_eq!(run.final_output, "resumed-done");
    assert_eq!(
        model_calls.load(Ordering::SeqCst),
        2,
        "turn-0 call_model must be served from history (2 total = tool-call turn + final turn), \
         not re-executed on the fresh worker"
    );
    assert_eq!(
        tool_invocations.load(Ordering::SeqCst),
        2,
        "tool ran twice: the blocked first attempt + the successful retry"
    );
}

/// Cancelling a run mid-flight returns `Err(RunError::Cancelled)` and still
/// persists the first-turn user message to the session (finalize-on-every-exit).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_returns_cancelled_and_persists_partial() {
    let Some(addr) = gate() else {
        return;
    };
    let queue = format!("helikon-cancel-{}", uuid::Uuid::new_v4());
    let entered_path = fresh_temp_path("helikon-cancel-entered");

    let agent = Arc::new(
        LlmAgent::builder::<()>()
            .name("canceller")
            .model(BlockUntilCancelModel {
                entered_path: entered_path.clone(),
            })
            .build(),
    );

    let worker = start_worker(
        addr.clone(),
        queue.clone(),
        Arc::clone(&agent),
        Duration::from_secs(5),
        Duration::from_secs(8),
        5,
    );

    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let cancel = CancellationToken::new();
    let runner = TemporalRunner::new(
        connect(&addr).await,
        TemporalRunnerConfig::new(queue.clone()),
    );
    let agent_for_run = Arc::clone(&agent);
    let session_for_run = Arc::clone(&session);
    let cancel_for_run = cancel.clone();
    let run_handle = tokio::spawn(async move {
        runner
            .run(
                agent_for_run.as_ref(),
                RunContext::ephemeral(())
                    .with_session(session_for_run)
                    .with_cancel(cancel_for_run),
                AgentInput::from_user_text("hello-cancel"),
                RunConfig::default(),
            )
            .await
    });

    // Wait until the model activity is running, then cancel the run.
    wait_for_file(&entered_path, Duration::from_secs(60)).await;
    cancel.cancel();

    let result = tokio::time::timeout(Duration::from_secs(60), run_handle)
        .await
        .expect("cancelled run resolves within 60s")
        .expect("run task did not panic");

    worker.stop().await;
    let _ = std::fs::remove_file(&entered_path);

    assert!(
        matches!(result, Err(RunError::Cancelled)),
        "a cancelled run maps to Err(RunError::Cancelled): {result:?}"
    );

    let snapshot = session.snapshot().await.expect("session snapshot readable");
    assert!(
        snapshot.messages.iter().any(|m| matches!(
            m,
            Item::UserMessage { content } if text_of(content) == "hello-cancel"
        )),
        "the first-turn user message must be persisted despite cancellation: {:?}",
        snapshot.messages
    );
}

/// Session round-trip: a second run on the same `MemorySession` sees the first
/// run's user + assistant items in its model request (persisted by the runner,
/// reloaded on the next run).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_round_trip() {
    let Some(addr) = gate() else {
        return;
    };
    let queue = format!("helikon-session-{}", uuid::Uuid::new_v4());

    let (model, _calls, requests) = scripted_model(None, "reply-one");
    let agent = Arc::new(
        LlmAgent::builder::<()>()
            .name("session")
            .model(model)
            .build(),
    );

    let worker = start_worker(
        addr.clone(),
        queue.clone(),
        Arc::clone(&agent),
        Duration::from_secs(5),
        Duration::from_secs(10),
        5,
    );

    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let runner = TemporalRunner::new(
        connect(&addr).await,
        TemporalRunnerConfig::new(queue.clone()),
    );

    let r1 = tokio::time::timeout(
        Duration::from_secs(60),
        runner.run(
            agent.as_ref(),
            RunContext::ephemeral(()).with_session(Arc::clone(&session)),
            AgentInput::from_user_text("hello"),
            RunConfig::default(),
        ),
    )
    .await
    .expect("run 1 within 60s");
    assert!(r1.is_ok(), "run 1 is Ok: {r1:?}");

    let r2 = tokio::time::timeout(
        Duration::from_secs(60),
        runner.run(
            agent.as_ref(),
            RunContext::ephemeral(()).with_session(Arc::clone(&session)),
            AgentInput::from_user_text("again"),
            RunConfig::default(),
        ),
    )
    .await
    .expect("run 2 within 60s");
    assert!(r2.is_ok(), "run 2 is Ok: {r2:?}");

    worker.stop().await;

    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 2, "one model call per run: {reqs:?}");
    let turn2 = &reqs[1];
    assert!(
        turn2.iter().any(|m| matches!(
            m,
            Item::UserMessage { content } if text_of(content) == "hello"
        )),
        "run 2's request must include run 1's user message: {turn2:?}"
    );
    assert!(
        turn2.iter().any(|m| matches!(
            m,
            Item::AssistantMessage { content, .. } if text_of(content) == "reply-one"
        )),
        "run 2's request must include run 1's assistant reply: {turn2:?}"
    );
}

/// Checklist item 1: a real non-retryable model failure crosses the activity
/// boundary and its `ErrorKindPayload` JSON is extracted from the *right* level
/// of the failure cause-chain and parsed back — the run maps to
/// `Err(RunError::Agent(..))` carrying the original model message, and the JSON
/// envelope does NOT leak through (which is what a mis-targeted
/// `activity_failure_message` would produce).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn model_failure_maps_to_typed_agent_error() {
    let Some(addr) = gate() else {
        return;
    };
    let queue = format!("helikon-modelfail-{}", uuid::Uuid::new_v4());

    let agent = Arc::new(
        LlmAgent::builder::<()>()
            .name("failer")
            .model(FailingModel)
            .build(),
    );

    let worker = start_worker(
        addr.clone(),
        queue.clone(),
        Arc::clone(&agent),
        Duration::from_secs(5),
        Duration::from_secs(10),
        5,
    );

    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let runner = TemporalRunner::new(
        connect(&addr).await,
        TemporalRunnerConfig::new(queue.clone()),
    );
    let result = tokio::time::timeout(
        Duration::from_secs(60),
        runner.run(
            agent.as_ref(),
            RunContext::ephemeral(()).with_session(Arc::clone(&session)),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        ),
    )
    .await
    .expect("model-failure run resolves within 60s");

    worker.stop().await;

    let err = result.expect_err("a non-retryable model failure maps to Err");
    let message = match err {
        RunError::Agent(agent_err) => agent_err.to_string(),
        other => panic!("expected RunError::Agent, got {other:?}"),
    };
    assert!(
        message.contains("connection lost"),
        "the model error message must survive the cause-chain round-trip: {message:?}"
    );
    assert!(
        !message.contains("\"Model\""),
        "the ErrorKindPayload JSON must be parsed, not leaked as a raw string \
         (a mis-targeted activity_failure_message would leak it): {message:?}"
    );
}

/// Checklist item 4: a tool-activity INFRA failure (a tool that blocks forever,
/// so every attempt times out and retries exhaust) folds into the run rather
/// than hanging it or losing the session write. The failed tool result is fed
/// back to the model, which then completes the run — events preserved, session
/// persisted, no hang. (This is v0's fold-in behavior, not a terminal
/// run-failure.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_infra_failure_folds_into_run_and_persists() {
    let Some(addr) = gate() else {
        return;
    };
    let queue = format!("helikon-toolinfra-{}", uuid::Uuid::new_v4());

    let (model, _calls, _requests) = scripted_model(Some("alwaysblock"), "recovered");
    let agent = Arc::new(
        LlmAgent::builder::<()>()
            .name("toolfailer")
            .model(model)
            .tool(AlwaysBlockTool {
                schema: serde_json::json!({ "type": "object" }),
            })
            .build(),
    );

    // Short tool start-to-close + only 2 attempts so the infra failure surfaces
    // fast (~2 timeouts of 2s each) instead of hanging.
    let worker = start_worker(
        addr.clone(),
        queue.clone(),
        Arc::clone(&agent),
        Duration::from_secs(2),
        Duration::from_secs(10),
        2,
    );

    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let runner = TemporalRunner::new(
        connect(&addr).await,
        TemporalRunnerConfig::new(queue.clone()),
    );
    let result = tokio::time::timeout(
        Duration::from_secs(90),
        runner.run(
            agent.as_ref(),
            RunContext::ephemeral(()).with_session(Arc::clone(&session)),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        ),
    )
    .await
    .expect("tool-infra-failure run resolves within 90s (no hang)");

    worker.stop().await;

    let run = result.expect("run folds the tool failure and completes Ok");
    assert_eq!(run.final_output, "recovered");
    assert!(
        matches!(run.events.first(), Some(AgentEvent::RunStarted { .. })),
        "events preserved (starts with RunStarted): {:?}",
        run.events
    );
    assert!(
        matches!(run.events.last(), Some(AgentEvent::RunCompleted { .. })),
        "events preserved (ends with RunCompleted): {:?}",
        run.events
    );

    // Session write not lost: the first-turn user message is persisted.
    let snapshot = session.snapshot().await.expect("session snapshot readable");
    assert!(
        snapshot.messages.iter().any(|m| matches!(
            m,
            Item::UserMessage { content } if text_of(content) == "go"
        )),
        "the session write must not be lost: {:?}",
        snapshot.messages
    );
}
