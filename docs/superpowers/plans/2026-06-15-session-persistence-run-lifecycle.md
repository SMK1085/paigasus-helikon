# Session Persistence in the Run Lifecycle — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a `Runner` load a `Session`'s persisted history at run start and write the run's semantic items back at run exit, so multi-turn sessions resume across `Runner::run` calls.

**Architecture:** Runner-owned session I/O. A new `SessionRecorder` in `paigasus-helikon-core` translates the run's `AgentInput` + the `AgentEvent` stream into `SessionEvent`s (pairing tool calls with results on drain). `TokioRunner` snapshots the session into the agent's input, taps the event stream into the recorder via `.inspect()`, and appends in `finalize()`. `LlmAgent` is untouched. A new `Runner::resume` default method continues from the session with no new turn.

**Tech Stack:** Rust (edition 2024, MSRV 1.85), `async-trait`, `futures-util` streams, `async-stream`, `tokio`, `jiff::Timestamp`, `tracing`.

**Spec:** `docs/superpowers/specs/2026-06-15-session-persistence-run-lifecycle-design.md`

---

## File Structure

- **`crates/paigasus-helikon-core/src/session.rs`** — add `SessionRecorder` (struct + `new`/`record_input`/`observe`/`drain`) next to `SessionEvent`/`project`, plus its unit tests. Auto-exported via the existing `pub use session::*` in `lib.rs`.
- **`crates/paigasus-helikon-core/src/runner.rs`** — add default `Runner::resume` / `resume_streamed` methods + unit tests.
- **`crates/paigasus-helikon-runtime-tokio/Cargo.toml`** — add `tracing` and `anyhow` to `[dependencies]`.
- **`crates/paigasus-helikon-runtime-tokio/src/lib.rs`** — load history, tap the stream into a recorder, real `finalize()`.
- **`crates/paigasus-helikon-runtime-tokio/tests/session_persistence.rs`** — new integration tests (round-trip, cancel-mid-tool, resume, streamed parity).

**No manual version bumps or CHANGELOG edits.** Both `paigasus-helikon-core` and `paigasus-helikon-runtime-tokio` are already-released crates; release-plz cascades the bumps automatically on merge. Do **not** perform the stub "ascend" ritual.

---

## Task 1: `SessionRecorder` in core

**Files:**
- Modify: `crates/paigasus-helikon-core/src/session.rs`
- Test: same file, new `#[cfg(test)] mod recorder_tests`

- [ ] **Step 1: Add the failing unit tests**

Append to `crates/paigasus-helikon-core/src/session.rs` (end of file):

```rust
#[cfg(test)]
mod recorder_tests {
    use super::*;
    use crate::AgentEvent;

    fn text(s: &str) -> Vec<ContentPart> {
        vec![ContentPart::Text { text: s.to_owned() }]
    }

    #[test]
    fn record_input_maps_user_and_skips_system() {
        let mut r = SessionRecorder::new("root");
        r.record_input(&[
            Item::System { content: text("sys") },
            Item::UserMessage { content: text("hi") },
        ]);
        let out = r.drain();
        assert_eq!(out.len(), 1, "System is skipped; only the user message remains");
        assert!(matches!(&out[0], SessionEvent::UserMessage { content, .. } if content == &text("hi")));
    }

    #[test]
    fn observe_records_assistant_with_item_agent() {
        let mut r = SessionRecorder::new("root");
        r.observe(&AgentEvent::MessageOutput {
            item: Item::AssistantMessage { content: text("yo"), agent: Some("speaker".into()) },
        });
        let out = r.drain();
        assert!(matches!(&out[0], SessionEvent::AssistantMessage { agent, .. } if agent == "speaker"));
    }

    #[test]
    fn observe_falls_back_to_tracked_agent_then_agent_updated() {
        let mut r = SessionRecorder::new("root");
        // No item.agent => falls back to the seeded root.
        r.observe(&AgentEvent::MessageOutput {
            item: Item::AssistantMessage { content: text("a"), agent: None },
        });
        // AgentUpdated changes the tracked agent for subsequent None items.
        r.observe(&AgentEvent::AgentUpdated { agent: "specialist".into() });
        r.observe(&AgentEvent::MessageOutput {
            item: Item::AssistantMessage { content: text("b"), agent: None },
        });
        let out = r.drain();
        assert!(matches!(&out[0], SessionEvent::AssistantMessage { agent, .. } if agent == "root"));
        assert!(matches!(&out[1], SessionEvent::AssistantMessage { agent, .. } if agent == "specialist"));
    }

    #[test]
    fn observe_records_tool_call_result_and_handoff() {
        let mut r = SessionRecorder::new("root");
        r.observe(&AgentEvent::ToolCallItem {
            item: Item::ToolCall { call_id: "c1".into(), name: "echo".into(), args: serde_json::json!({}) },
        });
        r.observe(&AgentEvent::ToolOutputItem {
            item: Item::ToolResult { call_id: "c1".into(), content: text("ok") },
        });
        r.observe(&AgentEvent::HandoffItem { from: "a".into(), to: "b".into() });
        let out = r.drain();
        assert!(matches!(&out[0], SessionEvent::ToolCalled { call_id, .. } if call_id == "c1"));
        assert!(matches!(&out[1], SessionEvent::ToolReturned { call_id, .. } if call_id == "c1"));
        assert!(matches!(&out[2], SessionEvent::HandoffOccurred { from, to, .. } if from == "a" && to == "b"));
    }

    #[test]
    fn drain_synthesizes_result_for_unmatched_tool_call() {
        let mut r = SessionRecorder::new("root");
        r.observe(&AgentEvent::ToolCallItem {
            item: Item::ToolCall { call_id: "c1".into(), name: "slow".into(), args: serde_json::json!({}) },
        });
        // No ToolOutputItem (interrupted mid-tool).
        let out = r.drain();
        assert_eq!(out.len(), 2, "the call plus a synthesized result");
        assert!(matches!(&out[0], SessionEvent::ToolCalled { call_id, .. } if call_id == "c1"));
        match &out[1] {
            SessionEvent::ToolReturned { call_id, content, .. } => {
                assert_eq!(call_id, "c1");
                assert!(matches!(&content[0], ContentPart::Text { text } if text.contains("did not complete")));
            }
            other => panic!("expected synthesized ToolReturned, got {other:?}"),
        }
        // project() yields a matched call/result pair (provider-valid).
        assert_eq!(project(&out).messages.len(), 2);
    }

    #[test]
    fn drain_does_not_synthesize_when_result_present() {
        let mut r = SessionRecorder::new("root");
        r.observe(&AgentEvent::ToolCallItem {
            item: Item::ToolCall { call_id: "c1".into(), name: "echo".into(), args: serde_json::json!({}) },
        });
        r.observe(&AgentEvent::ToolOutputItem {
            item: Item::ToolResult { call_id: "c1".into(), content: text("ok") },
        });
        assert_eq!(r.drain().len(), 2, "no extra synthesized event");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core recorder_tests 2>&1 | tail -20`
Expected: FAIL — `cannot find type/function SessionRecorder` (not yet defined).

- [ ] **Step 3: Implement `SessionRecorder`**

In `crates/paigasus-helikon-core/src/session.rs`, change the imports line:

```rust
use crate::{ContentPart, Item};
```

to:

```rust
use crate::{AgentEvent, ContentPart, Item};
```

Then insert this block immediately **after** the `project` function (before `pub struct SequenceId`):

```rust
/// Accumulates one run's semantic items as [`SessionEvent`]s for the runner to
/// persist at run exit.
///
/// The runner pre-seeds the recorder with the run's `AgentInput` messages (the
/// new turn — which the agent does not re-emit on its event stream), then feeds
/// it every [`crate::AgentEvent`] as the stream flows. [`SessionRecorder::drain`]
/// returns the accumulated log, synthesizing a [`SessionEvent::ToolReturned`]
/// for any tool call left without a result (a run interrupted mid-tool) so the
/// log always [`project`]s to a provider-valid conversation.
#[derive(Debug)]
pub struct SessionRecorder {
    events: Vec<SessionEvent>,
    current_agent: String,
}

impl SessionRecorder {
    /// Create a recorder seeded with the root agent's name. Used to attribute
    /// assistant messages whose [`Item::AssistantMessage`] `agent` is `None`,
    /// before any `RunStarted`/`AgentUpdated` updates the tracked agent.
    pub fn new(root_agent: impl Into<String>) -> Self {
        Self {
            events: Vec::new(),
            current_agent: root_agent.into(),
        }
    }

    /// Record the run's input messages (the new turn). The agent never re-emits
    /// these on its event stream, so the runner records them directly.
    /// [`Item::System`] has no [`SessionEvent`] equivalent and is skipped.
    pub fn record_input(&mut self, messages: &[Item]) {
        for item in messages {
            match item {
                Item::UserMessage { content } => {
                    self.events.push(SessionEvent::user_message(content.clone()));
                }
                Item::AssistantMessage { content, agent } => {
                    let name = agent.clone().unwrap_or_else(|| self.current_agent.clone());
                    self.events
                        .push(SessionEvent::assistant_message(content.clone(), name));
                }
                Item::ToolCall { call_id, name, args } => {
                    self.events.push(SessionEvent::tool_called(
                        call_id.clone(),
                        name.clone(),
                        args.clone(),
                    ));
                }
                Item::ToolResult { call_id, content } => {
                    self.events
                        .push(SessionEvent::tool_returned(call_id.clone(), content.clone()));
                }
                Item::System { .. } => {
                    tracing::debug!(
                        "SessionRecorder: skipping Item::System in input (no SessionEvent variant)"
                    );
                }
            }
        }
    }

    /// Observe one [`crate::AgentEvent`] from the run's stream: record the
    /// semantic items (assistant messages, tool calls, tool results, handoffs)
    /// and track the active agent from `RunStarted` / `AgentUpdated`.
    pub fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::RunStarted { agent } | AgentEvent::AgentUpdated { agent } => {
                self.current_agent = agent.clone();
            }
            AgentEvent::MessageOutput {
                item: Item::AssistantMessage { content, agent },
            } => {
                let name = agent.clone().unwrap_or_else(|| self.current_agent.clone());
                self.events
                    .push(SessionEvent::assistant_message(content.clone(), name));
            }
            AgentEvent::ToolCallItem {
                item: Item::ToolCall { call_id, name, args },
            } => {
                self.events.push(SessionEvent::tool_called(
                    call_id.clone(),
                    name.clone(),
                    args.clone(),
                ));
            }
            AgentEvent::ToolOutputItem {
                item: Item::ToolResult { call_id, content },
            } => {
                self.events
                    .push(SessionEvent::tool_returned(call_id.clone(), content.clone()));
            }
            AgentEvent::HandoffItem { from, to } => {
                self.events
                    .push(SessionEvent::handoff_occurred(from.clone(), to.clone()));
            }
            _ => {}
        }
    }

    /// Consume the accumulated events, appending a synthesized
    /// [`SessionEvent::ToolReturned`] for every [`SessionEvent::ToolCalled`]
    /// left without a matching result (a run cancelled/timed out mid-tool).
    /// The returned log always [`project`]s to a provider-valid conversation.
    pub fn drain(&mut self) -> Vec<SessionEvent> {
        let returned: std::collections::HashSet<String> = self
            .events
            .iter()
            .filter_map(|e| match e {
                SessionEvent::ToolReturned { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        let mut synthesized = Vec::new();
        for e in &self.events {
            if let SessionEvent::ToolCalled { call_id, .. } = e {
                if !returned.contains(call_id) {
                    synthesized.push(SessionEvent::tool_returned(
                        call_id.clone(),
                        vec![ContentPart::Text {
                            text: "tool call did not complete (run cancelled/timed out)".to_owned(),
                        }],
                    ));
                }
            }
        }
        let mut out = std::mem::take(&mut self.events);
        out.extend(synthesized);
        out
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core recorder_tests 2>&1 | tail -20`
Expected: PASS (6 tests).

- [ ] **Step 5: Format and lint**

Run: `cargo fmt --all && cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings 2>&1 | tail -20`
Expected: no warnings (every new `pub` item has a `///` doc).

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/session.rs
git commit -m "feat(core): SMA-392 add SessionRecorder for run-lifecycle persistence"
```

---

## Task 2: `Runner::resume` / `resume_streamed` default methods

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs`
- Test: same file, new `#[cfg(test)] mod resume_tests`

- [ ] **Step 1: Add the failing unit tests**

Append to `crates/paigasus-helikon-core/src/runner.rs` (end of file):

```rust
#[cfg(test)]
mod resume_tests {
    use super::*;
    use crate::{CancellationToken, HookRegistry, MemorySession, Session, TracerHandle};
    use std::sync::{Arc, Mutex};

    // Runner that records how many input messages its run/run_streamed saw.
    #[derive(Default)]
    struct CapturingRunner {
        last_len: Arc<Mutex<Option<usize>>>,
    }

    #[async_trait]
    impl Runner<()> for CapturingRunner {
        async fn run(
            &self,
            _agent: &(dyn Agent<()> + '_),
            _ctx: RunContext<()>,
            input: AgentInput,
            _config: RunConfig,
        ) -> Result<RunResult, RunError> {
            *self.last_len.lock().unwrap() = Some(input.messages.len());
            Ok(RunResult::default())
        }
        async fn run_streamed(
            &self,
            _agent: &(dyn Agent<()> + '_),
            _ctx: RunContext<()>,
            input: AgentInput,
            _config: RunConfig,
        ) -> Result<RunResultStreaming, RunError> {
            *self.last_len.lock().unwrap() = Some(input.messages.len());
            let s: futures_core::stream::BoxStream<'static, AgentEvent> =
                Box::pin(futures_util::stream::empty());
            Ok(RunResultStreaming::new(s))
        }
    }

    struct DummyAgent;
    #[async_trait]
    impl Agent<()> for DummyAgent {
        fn name(&self) -> &str { "dummy" }
        fn description(&self) -> &str { "dummy" }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<futures_core::stream::BoxStream<'static, AgentEvent>, AgentError> {
            Ok(Box::pin(futures_util::stream::empty()))
        }
    }

    fn ctx() -> RunContext<()> {
        RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn resume_delegates_to_run_with_empty_input() {
        let r = CapturingRunner::default();
        r.resume(&DummyAgent, ctx(), RunConfig::default()).await.unwrap();
        assert_eq!(*r.last_len.lock().unwrap(), Some(0));
    }

    #[tokio::test]
    async fn resume_streamed_delegates_with_empty_input() {
        let r = CapturingRunner::default();
        let _ = r.resume_streamed(&DummyAgent, ctx(), RunConfig::default()).await.unwrap();
        assert_eq!(*r.last_len.lock().unwrap(), Some(0));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core resume_tests 2>&1 | tail -20`
Expected: FAIL — `no method named resume found for ... CapturingRunner`.

- [ ] **Step 3: Add the default methods to the `Runner` trait**

In `crates/paigasus-helikon-core/src/runner.rs`, inside the `#[async_trait] pub trait Runner<Ctx>` block, immediately **after** the `run_streamed` method declaration (the line ending `-> Result<RunResultStreaming, RunError>;`), add:

```rust
    /// Resume a run from the session's persisted history with no new input.
    ///
    /// Equivalent to [`Runner::run`] with an empty [`AgentInput`]: the runner
    /// loads the conversation from `ctx.session()` and continues it. Use this to
    /// continue a multi-turn session, or to retry a failed run without
    /// re-appending the previous turn's user message. (With a `Session` present,
    /// [`Runner::run`]'s `input` is the *new turn*; the session owns history.)
    async fn resume(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        config: RunConfig,
    ) -> Result<RunResult, RunError> {
        self.run(agent, ctx, AgentInput::new(), config).await
    }

    /// Streaming counterpart of [`Runner::resume`].
    async fn resume_streamed(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        config: RunConfig,
    ) -> Result<RunResultStreaming, RunError> {
        self.run_streamed(agent, ctx, AgentInput::new(), config).await
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core resume_tests 2>&1 | tail -20`
Expected: PASS (2 tests).

- [ ] **Step 5: Format and lint**

Run: `cargo fmt --all && cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings 2>&1 | tail -20`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/runner.rs
git commit -m "feat(core): SMA-392 add Runner::resume/resume_streamed default methods"
```

---

## Task 3: Wire load + record + finalize into `TokioRunner`

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/Cargo.toml`
- Modify: `crates/paigasus-helikon-runtime-tokio/src/lib.rs`
- Test: new `crates/paigasus-helikon-runtime-tokio/tests/session_persistence.rs`

- [ ] **Step 1: Add the failing round-trip integration test**

Create `crates/paigasus-helikon-runtime-tokio/tests/session_persistence.rs`:

```rust
//! Session persistence wired into the run lifecycle (SMA-392).

#[path = "common/mod.rs"]
mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;
use paigasus_helikon_core::{
    AgentInput, CancellationToken, ContentPart, FinishReason, Item, MemorySession, Model,
    ModelCapabilities, ModelError, ModelEvent, ModelRequest, RunConfig, Runner, Session,
    SessionEvent,
};
use paigasus_helikon_runtime_tokio::TokioRunner;

use common::{run_context_with_session, text_agent};

/// Model that records each request's messages and replays one scripted turn.
struct RecordingModel {
    requests: Arc<Mutex<Vec<Vec<Item>>>>,
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
}

impl RecordingModel {
    fn new(requests: Arc<Mutex<Vec<Vec<Item>>>>, scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            requests,
            scripts: Mutex::new(scripts.into()),
        })
    }
}

#[async_trait]
impl Model for RecordingModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        self.requests.lock().unwrap().push(request.messages.clone());
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ModelError::Other(anyhow::anyhow!("no more scripted responses")))?;
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

fn say(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TokenDelta { text: text.into() },
        ModelEvent::Finish { reason: FinishReason::Stop },
    ]
}

fn content_text(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_turn_round_trip_sees_prior_messages() {
    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = RecordingModel::new(requests.clone(), vec![say("first"), say("second")]);
    let agent = text_agent(model, Vec::new());

    let r1 = TokioRunner
        .run(
            &agent,
            run_context_with_session(session.clone()),
            AgentInput::from_user_text("hello"),
            RunConfig::default(),
        )
        .await;
    assert!(r1.is_ok(), "turn 1: {r1:?}");

    let r2 = TokioRunner
        .run(
            &agent,
            run_context_with_session(session.clone()),
            AgentInput::from_user_text("again"),
            RunConfig::default(),
        )
        .await;
    assert!(r2.is_ok(), "turn 2: {r2:?}");

    // Acceptance #1: turn 2's model request contains turn 1's user + assistant.
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 2, "one model call per turn");
    let turn2 = &reqs[1];
    assert!(
        turn2.iter().any(|m| matches!(m, Item::UserMessage { content } if content_text(content) == "hello")),
        "turn 2 request must include turn 1's user message: {turn2:?}"
    );
    assert!(
        turn2.iter().any(|m| matches!(m, Item::AssistantMessage { content, .. } if content_text(content) == "first")),
        "turn 2 request must include turn 1's assistant reply: {turn2:?}"
    );

    // Acceptance #2: the persisted log is [User, Asst, User, Asst].
    let events = session.events(None).await.unwrap();
    assert_eq!(events.len(), 4, "{events:?}");
    assert!(matches!(&events[0], SessionEvent::UserMessage { .. }));
    assert!(matches!(&events[1], SessionEvent::AssistantMessage { agent, .. } if agent == "test"));
    assert!(matches!(&events[2], SessionEvent::UserMessage { .. }));
    assert!(matches!(&events[3], SessionEvent::AssistantMessage { .. }));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test session_persistence 2>&1 | tail -25`
Expected: FAIL — turn 2's request does NOT contain "hello"/"first" (history isn't loaded yet), and `events.len()` is 0 (finalize is still a no-op append).

- [ ] **Step 3: Add the `tracing` and `anyhow` dependencies**

In `crates/paigasus-helikon-runtime-tokio/Cargo.toml`, under `[dependencies]`, add two lines (keep the existing alignment style):

```toml
tracing      = { workspace = true }
anyhow       = { workspace = true }
```

- [ ] **Step 4: Implement load + record + real finalize**

In `crates/paigasus-helikon-runtime-tokio/src/lib.rs`, update the core import to add `SessionRecorder`:

```rust
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, RunConfig, RunContext, RunError, RunResult,
    RunResultStreaming, Runner, Session, SessionRecorder,
};
```

Replace the entire `finalize` function (the placeholder) with these two functions:

```rust
/// Snapshot the session into the merged input and seed a recorder with the
/// run's new-turn messages. A read failure is a hard error: the run cannot
/// faithfully resume from an unreadable session, so it fails before the agent
/// starts. (The write side, by contrast, is best-effort — see `finalize`.)
async fn load_and_record(
    session: &Arc<dyn Session>,
    agent_name: &str,
    input: AgentInput,
) -> Result<(AgentInput, Arc<Mutex<SessionRecorder>>), RunError> {
    let snapshot = session
        .snapshot()
        .await
        .map_err(|e| RunError::Other(anyhow::Error::new(e)))?;
    let mut recorder = SessionRecorder::new(agent_name);
    recorder.record_input(&input.messages);

    let mut merged = AgentInput::new();
    merged.messages = snapshot.messages;
    merged.messages.extend(input.messages);
    Ok((merged, Arc::new(Mutex::new(recorder))))
}

/// Post-run finalization: drain the recorder and append the run's events.
/// Persistence is best-effort — an append error is logged, never propagated, so
/// the run's outcome (Ok / Cancelled / Timeout / Agent error) is unchanged.
async fn finalize(session: &Arc<dyn Session>, recorder: &Arc<Mutex<SessionRecorder>>) {
    let events = recorder
        .lock()
        .expect("session recorder mutex poisoned")
        .drain();
    if let Err(e) = session.append(&events).await {
        tracing::warn!(
            error = %e,
            "session persistence failed during finalize; run outcome unaffected"
        );
    }
}
```

Replace the body of `TokioRunner::run` with:

```rust
    async fn run(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResult, RunError> {
        let timeout = config.timeout;
        let ctx = ctx.with_run_config(config);
        let cancel = ctx.cancel().clone();
        let session = ctx.session().clone();
        let failure = ctx.failure_handle();

        // Load persisted history and seed the recorder with the new turn.
        let (merged, recorder) = load_and_record(&session, agent.name(), input).await?;

        let stream = agent.run(ctx, merged).await?;
        let (controlled_stream, outcome) = controlled(stream, cancel, timeout);
        let rec_inspect = Arc::clone(&recorder);
        let recorded = controlled_stream
            .inspect(move |ev| {
                rec_inspect
                    .lock()
                    .expect("session recorder mutex poisoned")
                    .observe(ev)
            })
            .boxed();
        // Do NOT `?`-short-circuit before finalize: agent failures surface as
        // collect()=Err, and finalize must still run.
        let collected = RunResultStreaming::with_failure(recorded, failure)
            .collect()
            .await;
        finalize(&session, &recorder).await;

        // A cancel/timeout outcome wins even if `collected` is Ok.
        match outcome.get() {
            Outcome::Cancelled => Err(RunError::Cancelled),
            Outcome::TimedOut => Err(RunError::Timeout),
            Outcome::Completed => collected,
        }
    }
```

Replace the body of `TokioRunner::run_streamed` with:

```rust
    async fn run_streamed(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResultStreaming, RunError> {
        let timeout = config.timeout;
        let ctx = ctx.with_run_config(config);
        let cancel = ctx.cancel().clone();
        let session = ctx.session().clone();
        let failure = ctx.failure_handle();

        let (merged, recorder) = load_and_record(&session, agent.name(), input).await?;

        let stream = agent.run(ctx, merged).await?;
        let (controlled_stream, outcome) = controlled(stream, cancel, timeout);
        let rec_inspect = Arc::clone(&recorder);
        let mut recorded = controlled_stream
            .inspect(move |ev| {
                rec_inspect
                    .lock()
                    .expect("session recorder mutex poisoned")
                    .observe(ev)
            })
            .boxed();

        let out = async_stream::stream! {
            while let Some(ev) = recorded.next().await {
                // Finalize BEFORE exposing a terminal event: a consumer may stop
                // polling (and drop the stream) the moment it sees the terminal,
                // so anything after the `yield` could never run.
                if matches!(
                    ev,
                    AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. }
                ) {
                    finalize(&session, &recorder).await;
                }
                yield ev;
            }
            // Cancel/timeout: the inner stream ended without a terminal event, so
            // synthesize one — again after finalize, for the same reason.
            match outcome.get() {
                Outcome::Cancelled => {
                    finalize(&session, &recorder).await;
                    yield AgentEvent::RunFailed { error: "run cancelled".to_owned() };
                }
                Outcome::TimedOut => {
                    finalize(&session, &recorder).await;
                    yield AgentEvent::RunFailed { error: "run timed out".to_owned() };
                }
                Outcome::Completed => {}
            }
        };
        Ok(RunResultStreaming::with_failure(Box::pin(out), failure))
    }
```

Also update the module-level doc on `finalize`'s old comment is gone; no other references remain. (`.inspect()` and `.boxed()` come from the already-imported `futures_util::stream::StreamExt as _`.)

- [ ] **Step 5: Run the new test to verify it passes**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test session_persistence 2>&1 | tail -25`
Expected: PASS (`multi_turn_round_trip_sees_prior_messages`).

- [ ] **Step 6: Run the existing runtime-tokio tests for regressions**

Run: `cargo test -p paigasus-helikon-runtime-tokio 2>&1 | tail -30`
Expected: PASS — in particular `finalize_runs_on_every_run_exit` still sees `append_count() == 1` on every exit (finalize still appends exactly once per run, now with real events).

- [ ] **Step 7: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-tokio --all-features --all-targets -- -D warnings 2>&1 | tail -20
git add crates/paigasus-helikon-runtime-tokio/Cargo.toml crates/paigasus-helikon-runtime-tokio/src/lib.rs crates/paigasus-helikon-runtime-tokio/tests/session_persistence.rs Cargo.lock
git commit -m "feat(runtime-tokio): SMA-392 load session history and persist run events in finalize"
```

---

## Task 4: Cancel-mid-tool persists a provider-valid log (H1)

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/session_persistence.rs`

- [ ] **Step 1: Add the failing test (and its helpers)**

Append to `crates/paigasus-helikon-runtime-tokio/tests/session_persistence.rs`. First extend the imports at the top of the file to add the tool + project + timeout types and the cancel-aware context builder:

```rust
use std::time::Duration;

use paigasus_helikon_core::{project, Tool, ToolContext, ToolError, ToolOutput};
use common::run_context_with_session_and_cancel;
```

Then add the blocking tool, a tool-call script helper, and the test:

```rust
/// Tool whose invocation never returns — lets a run be cancelled mid-execution.
struct BlockingTool {
    name: String,
    schema: serde_json::Value,
}

#[async_trait]
impl Tool<()> for BlockingTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "blocks forever"
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
        unreachable!("pending() never resolves")
    }
}

fn call_tool(call_id: &str, name: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallDelta {
            call_id: call_id.into(),
            name: Some(name.into()),
            args_delta: "{}".into(),
        },
        ModelEvent::Finish { reason: FinishReason::ToolCalls },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_mid_tool_persists_provider_valid_log() {
    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = RecordingModel::new(requests, vec![call_tool("c1", "blocker")]);
    let tool: Arc<dyn Tool<()>> = Arc::new(BlockingTool {
        name: "blocker".into(),
        schema: serde_json::json!({"type": "object"}),
    });
    let agent = text_agent(model, vec![tool]);

    let cancel = CancellationToken::new();
    let ctx = run_context_with_session_and_cancel(session.clone(), cancel.clone());

    let res = tokio::time::timeout(Duration::from_secs(5), async {
        let run_fut = TokioRunner.run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        );
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        };
        let (r, _) = tokio::join!(run_fut, canceller);
        r
    })
    .await
    .expect("cancel within 5s");
    assert!(matches!(res, Err(paigasus_helikon_core::RunError::Cancelled)), "{res:?}");

    // The persisted log pairs the tool call with a synthesized result.
    let events = session.events(None).await.unwrap();
    let calls: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ToolCalled { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    let results: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ToolReturned { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(calls, vec!["c1"], "tool call persisted: {events:?}");
    assert_eq!(results, vec!["c1"], "synthesized result paired with the call: {events:?}");

    // project() => no dangling tool call (the last message is the tool result).
    let snap = project(&events);
    assert!(
        matches!(snap.messages.last(), Some(Item::ToolResult { call_id, .. }) if call_id == "c1"),
        "projection must end in the matched tool result: {:?}",
        snap.messages
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test session_persistence cancel_mid_tool 2>&1 | tail -25`
Expected: PASS. (This validates Task 1's `drain` synthesis end-to-end: the recorder captured `ToolCalled c1` before the tool blocked, the cancel dropped the tool future, and `finalize` synthesized the matching `ToolReturned`.)

- [ ] **Step 3: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-tokio --all-features --all-targets -- -D warnings 2>&1 | tail -20
git add crates/paigasus-helikon-runtime-tokio/tests/session_persistence.rs
git commit -m "test(runtime-tokio): SMA-392 cancel mid-tool persists a provider-valid log"
```

---

## Task 5: `resume()` round-trip and `run_streamed` parity

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/session_persistence.rs`

- [ ] **Step 1: Add the `resume()` round-trip test**

Append to `crates/paigasus-helikon-runtime-tokio/tests/session_persistence.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_continues_from_history_without_new_turn() {
    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = RecordingModel::new(requests.clone(), vec![say("first"), say("second")]);
    let agent = text_agent(model, Vec::new());

    // Turn 1: a normal run with a new user turn.
    TokioRunner
        .run(
            &agent,
            run_context_with_session(session.clone()),
            AgentInput::from_user_text("hello"),
            RunConfig::default(),
        )
        .await
        .unwrap();

    // resume(): no new turn — continues from persisted history.
    TokioRunner
        .resume(&agent, run_context_with_session(session.clone()), RunConfig::default())
        .await
        .unwrap();

    // The resume request saw turn 1's messages, and added NO new user message.
    let reqs = requests.lock().unwrap();
    let resume_req = &reqs[1];
    assert!(
        resume_req.iter().any(|m| matches!(m, Item::UserMessage { content } if content_text(content) == "hello")),
        "resume must load prior history: {resume_req:?}"
    );
    let user_count = resume_req
        .iter()
        .filter(|m| matches!(m, Item::UserMessage { .. }))
        .count();
    assert_eq!(user_count, 1, "resume adds no new user message: {resume_req:?}");

    // Persisted log: [User hello, Asst first, Asst second] — no second user message.
    let events = session.events(None).await.unwrap();
    let user_events = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::UserMessage { .. }))
        .count();
    assert_eq!(user_events, 1, "resume persisted no extra user message: {events:?}");
}
```

- [ ] **Step 2: Add the `run_streamed` parity test**

Append to the same file (uses `futures_util::StreamExt` for `.next()` while draining):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_streamed_persists_when_drained() {
    use futures_util::StreamExt as _;

    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = RecordingModel::new(requests, vec![say("hi")]);
    let agent = text_agent(model, Vec::new());

    let handle = TokioRunner
        .run_streamed(
            &agent,
            run_context_with_session(session.clone()),
            AgentInput::from_user_text("yo"),
            RunConfig::default(),
        )
        .await
        .unwrap();

    // Drain the stream to its terminal so finalize runs.
    let mut events = handle.events;
    while events.next().await.is_some() {}

    let persisted = session.events(None).await.unwrap();
    assert_eq!(persisted.len(), 2, "user + assistant persisted: {persisted:?}");
    assert!(matches!(&persisted[0], SessionEvent::UserMessage { .. }));
    assert!(matches!(&persisted[1], SessionEvent::AssistantMessage { agent, .. } if agent == "test"));
}
```

- [ ] **Step 3: Run the new tests**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test session_persistence 2>&1 | tail -25`
Expected: PASS (all session-persistence tests).

- [ ] **Step 4: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-tokio --all-features --all-targets -- -D warnings 2>&1 | tail -20
git add crates/paigasus-helikon-runtime-tokio/tests/session_persistence.rs
git commit -m "test(runtime-tokio): SMA-392 resume round-trip and run_streamed persistence parity"
```

> **Multi-agent attribution (spec M1):** agent-name resolution is covered at the unit level in Task 1 (`observe_falls_back_to_tracked_agent_then_agent_updated` exercises `item.agent` ∨ tracked-agent updated by `AgentUpdated`). The runner-owned design records sub-agent messages with their own `item.agent` (always set by `build_items`), so a full handoff integration test is redundant for the persistence guarantee and is intentionally not added here.

---

## Task 6: Documentation

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs` (rustdoc on `run` / `run_streamed`)

- [ ] **Step 1: Document the session contract on `Runner::run` and `run_streamed`**

In `crates/paigasus-helikon-core/src/runner.rs`, replace the one-line doc on `run`:

```rust
    /// Run the agent to completion and return the aggregated result.
```

with:

```rust
    /// Run the agent to completion and return the aggregated result.
    ///
    /// **With a `Session`** (always present on [`RunContext`]): the runner loads
    /// persisted history at start and seeds the conversation as
    /// `history ++ input.messages`, so `input` is the *new turn* — the session
    /// owns history. The run's events are persisted at exit. To continue with no
    /// new turn (or to retry a failed run without re-appending the user message),
    /// use [`Runner::resume`].
```

and replace the one-line doc on `run_streamed`:

```rust
    /// Run the agent and return a streaming result handle.
```

with:

```rust
    /// Run the agent and return a streaming result handle.
    ///
    /// Session loading/seeding matches [`Runner::run`]. **The returned stream
    /// must be driven to its terminal for the run's events to be persisted:** a
    /// consumer that drops the stream early may skip the finalize step and leave
    /// a partial turn (or nothing) written.
```

- [ ] **Step 2: Verify docs build with warnings denied**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps 2>&1 | tail -20`
Expected: success, no warnings.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-core/src/runner.rs
git commit -m "docs(core): SMA-392 document the Session contract on Runner::run/run_streamed"
```

---

## Task 7: Full local CI gate

**Files:** none (verification only)

- [ ] **Step 1: Format check**

Run: `cargo fmt --all -- --check`
Expected: no diff.

- [ ] **Step 2: Clippy (workspace)**

Run: `cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -20`
Expected: no warnings.

- [ ] **Step 3: Tests (workspace)**

Run: `cargo test --workspace --all-features 2>&1 | tail -30`
Expected: all pass, including `paigasus-helikon-core` (`recorder_tests`, `resume_tests`) and `paigasus-helikon-runtime-tokio` (`session_persistence`, `run_control`, `run_streamed`, `run_smoke`, `run_error`).

- [ ] **Step 4: Docs + doc-coverage**

Run:
```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps 2>&1 | tail -10
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh 2>&1 | tail -10
```
Expected: both succeed (doc coverage ≥ 80%).

- [ ] **Step 5: Final commit if anything changed**

If `cargo fmt` reformatted or `Cargo.lock` changed:

```bash
git add -- Cargo.lock crates/
git commit -m "chore(runtime-tokio): SMA-392 fmt + lockfile after session persistence"
```

(Use explicit paths — never `git add -A`; `.env`/`.claude` are untracked-but-not-ignored.)

---

## Self-Review

**Spec coverage:**
- Load (concatenate history ++ input) → Task 3 (`load_and_record`), verified by Task 3 round-trip.
- Partial-transcript-on-every-exit → Task 3 finalize + existing `finalize_runs_on_every_run_exit`; Task 4 covers the cancel path's contents.
- Runner-owned architecture; `LlmAgent` untouched → Tasks 1+3 (no `agent.rs` change).
- Decision 4 (drain pairing) → Task 1 (`drain` + unit tests) + Task 4 (end-to-end).
- Decision 5 (`resume`) → Task 2 (default methods + unit tests) + Task 5 (round-trip).
- Decision 6 (read hard-fail / write best-effort) → Task 3 (`load_and_record` returns `Err`; `finalize` logs and swallows).
- Acceptance #1 (multi-turn round-trip) → Task 3; #2 (projection identity) → Task 3 event-log assertions; #3 (finalize on every exit) → existing test still green (Task 3 Step 6).
- M3 (streamed drain requirement) → Task 6 docs; Task 5 parity test drains the stream.
- L1 (unbounded growth) → noted out-of-scope in the spec; no task.

**Placeholder scan:** none — every code step contains complete code; every command has expected output.

**Type consistency:** `SessionRecorder::{new, record_input, observe, drain}` used identically across Tasks 1/3. `finalize(&session, &recorder)` and `load_and_record(&session, agent.name(), input)` signatures match their call sites. `RecordingModel`/`say`/`content_text`/`call_tool`/`BlockingTool` are defined once (Tasks 3/4) and reused (Tasks 4/5). Session events asserted as `SessionEvent::{UserMessage, AssistantMessage, ToolCalled, ToolReturned}` match the core definitions.
