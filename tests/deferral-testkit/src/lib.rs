//! Internal, unpublished test helpers.
//!
//! This crate holds the helpers that the core and runtime-tokio integration
//! tests share. Use it only as a path-only dev-dependency.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, MemorySession, Model, ModelCapabilities, ModelError,
    ModelEvent, ModelRequest, Session, SessionEvent,
};

/// A scripted model that keeps every request it gets. Each call pops one
/// script; an empty queue gives an error.
pub struct RequestLog {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl RequestLog {
    /// Make a model that answers its calls with `scripts`, in order.
    pub fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(VecDeque::from(scripts)),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// Every request the model got, in call order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Model for RequestLog {
    async fn invoke(
        &self,
        request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        self.requests.lock().unwrap().push(request.clone());
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ModelError::Other(anyhow::anyhow!("no more scripted responses")))?;
        Ok(Box::pin(futures_util::stream::iter(
            script.into_iter().map(Ok),
        )))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// Each `ToolCall` has a `ToolResult` with the same call id later, with no
/// `UserMessage` or `AssistantMessage` between them. Each `ToolResult` closes
/// one earlier open `ToolCall` with its call id. A call id can repeat only
/// after its earlier call has a result. A call after a result starts a new
/// round, so no call may wait for a result then.
///
/// Its self-test is `pairing_oracle_rejects_a_call_after_a_result_while_a_call_waits`
/// in core `tests/deferred_resume.rs`.
pub fn assert_tool_calls_paired(messages: &[Item]) {
    // The calls that wait for a result, in call order.
    let mut outstanding: Vec<&str> = Vec::new();
    // A result came after the last call: the next call starts a new round.
    let mut seen_result_since_call = false;
    for item in messages {
        match item {
            Item::ToolCall { call_id, .. } => {
                assert!(
                    !outstanding.contains(&call_id.as_str()),
                    "call {call_id} repeats while it waits for a result: {messages:?}"
                );
                assert!(
                    !(seen_result_since_call && !outstanding.is_empty()),
                    "call {call_id} starts a new round while {outstanding:?} wait for a result: {messages:?}"
                );
                seen_result_since_call = false;
                outstanding.push(call_id);
            }
            Item::ToolResult { call_id, .. } => {
                let Some(p) = outstanding.iter().position(|c| *c == call_id.as_str()) else {
                    panic!("result {call_id} has no open earlier call: {messages:?}");
                };
                // Each result closes one call.
                outstanding.remove(p);
                seen_result_since_call = true;
            }
            Item::UserMessage { .. } | Item::AssistantMessage { .. } => {
                assert!(
                    outstanding.is_empty(),
                    "tool calls {outstanding:?} are not paired: {messages:?}"
                );
            }
            _ => {}
        }
    }
    assert!(
        outstanding.is_empty(),
        "tool calls {outstanding:?} are not paired: {messages:?}"
    );
}

/// The cancel text of core 0.6.0, as `with_late_transfer_results` writes it.
///
/// This text is fixed. Today it equals `CANCELLED_TOOL_CALL_TEXT` in core
/// `session.rs`. Do not change it to follow a later change of that constant.
pub const LEGACY_CANCEL_TEXT: &str = "tool call did not complete (run cancelled/timed out)";

/// A copy of `session` in the layout of core 0.6.0 (before SMA-810).
///
/// `session` must hold one handoff run in the current layout. This removes
/// each `ToolReturned` directly after a `HandoffOccurred` (the transfer
/// results of that handoff response) and appends a cancel result for each
/// removed one at the end of the run's events, in call order.
pub async fn with_late_transfer_results(session: &Arc<MemorySession>) -> Arc<MemorySession> {
    let recorded = session.events(None).await.unwrap();
    let mut events = Vec::with_capacity(recorded.len());
    let mut late = Vec::new();
    let mut after_handoff = false;
    for ev in recorded {
        match &ev {
            SessionEvent::HandoffOccurred { .. } => {
                after_handoff = true;
                events.push(ev);
            }
            SessionEvent::ToolReturned {
                call_id, content, ..
            } if after_handoff => {
                let text = text_of(content);
                assert!(
                    text.starts_with("transferred to ")
                        || text.starts_with("transfer not performed"),
                    "{call_id} after a handoff is not a transfer result: {text}"
                );
                late.push(call_id.clone());
            }
            _ => {
                after_handoff = false;
                events.push(ev);
            }
        }
    }
    assert!(!late.is_empty(), "the log has no transfer result to move");
    events.extend(late.into_iter().map(|id| {
        SessionEvent::tool_returned(
            id,
            vec![ContentPart::Text {
                text: LEGACY_CANCEL_TEXT.to_owned(),
            }],
        )
    }));
    let legacy = Arc::new(MemorySession::new());
    legacy.append(&events).await.unwrap();
    legacy
}

fn text_of(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}
