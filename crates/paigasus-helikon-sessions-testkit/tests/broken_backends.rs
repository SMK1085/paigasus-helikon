//! Negative tests: `run_tool_deferred` must fail for a backend that corrupts
//! one thing. `Fault::None` proves that the wrapper is otherwise neutral.
#![allow(missing_docs)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use paigasus_helikon_core::{
    ConversationSnapshot, Item, MemorySession, SequenceId, Session, SessionError, SessionEvent,
};
use paigasus_helikon_sessions_testkit::run_tool_deferred;
use serde_json::Value;

#[derive(Clone, Copy)]
enum Fault {
    None,
    ReverseHandoffPath,
    ArgsThroughF64,
    CalledArgsIntoDeferral,
    RenameSnapshotToolCall,
    SwapEvents2And3,
    SameSequencePerBatch,
}

/// A `MemorySession` wrapper that applies one `Fault` on read.
struct Broken {
    inner: MemorySession,
    fault: Fault,
    /// The event count before each `append`: the start index of each batch.
    batch_starts: Mutex<Vec<u64>>,
}

fn broken(fault: Fault) -> Arc<dyn Session> {
    Arc::new(Broken {
        inner: MemorySession::new(),
        fault,
        batch_starts: Mutex::new(Vec::new()),
    })
}

/// Round-trip every JSON number through `f64`, recursively. Only a value that
/// `f64` cannot hold exactly (the 2^53+1 `ref_id`) changes. A whole number that
/// converts back to the same integer keeps its integer kind, so every other
/// number stays equal.
fn through_f64(v: &mut Value) {
    match v {
        Value::Number(n) => {
            let f = n.as_f64().expect("number fits f64");
            let exact_i = n.as_i64().filter(|i| (*i as f64) == f && f as i64 == *i);
            let exact_u = n.as_u64().filter(|u| (*u as f64) == f && f as u64 == *u);
            *v = match (exact_i, exact_u) {
                (Some(i), _) => Value::from(i),
                (_, Some(u)) => Value::from(u),
                _ => Value::from(f),
            };
        }
        Value::Array(a) => a.iter_mut().for_each(through_f64),
        Value::Object(o) => o.values_mut().for_each(through_f64),
        _ => {}
    }
}

#[async_trait]
impl Session for Broken {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError> {
        let start = self.inner.events(None).await?.len() as u64;
        self.batch_starts.lock().unwrap().push(start);
        self.inner.append(events).await
    }

    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        let mut log = self.inner.events(None).await?;
        match self.fault {
            Fault::None | Fault::RenameSnapshotToolCall => {}
            Fault::ReverseHandoffPath => {
                for ev in &mut log {
                    if let SessionEvent::ToolDeferred { handoff_path, .. } = ev {
                        handoff_path.reverse();
                    }
                }
            }
            Fault::ArgsThroughF64 => {
                for ev in &mut log {
                    match ev {
                        SessionEvent::ToolCalled { args, .. }
                        | SessionEvent::ToolDeferred { args, .. } => through_f64(args),
                        _ => {}
                    }
                }
            }
            Fault::CalledArgsIntoDeferral => {
                let called: Vec<(String, Value)> = log
                    .iter()
                    .filter_map(|ev| match ev {
                        SessionEvent::ToolCalled { call_id, args, .. } => {
                            Some((call_id.clone(), args.clone()))
                        }
                        _ => None,
                    })
                    .collect();
                for ev in &mut log {
                    if let SessionEvent::ToolDeferred { call_id, args, .. } = ev {
                        if let Some((_, a)) = called.iter().find(|(id, _)| id == call_id) {
                            *args = a.clone();
                        }
                    }
                }
            }
            Fault::SwapEvents2And3 => log.swap(2, 3),
            Fault::SameSequencePerBatch => {
                let starts = self.batch_starts.lock().unwrap().clone();
                let seq_of = |pos: u64| starts.iter().copied().filter(|s| *s <= pos).max().unwrap();
                let after = since.map(|s| s.0);
                return Ok(log
                    .into_iter()
                    .enumerate()
                    .filter(|(pos, _)| after.is_none_or(|a| seq_of(*pos as u64) > a))
                    .map(|(_, ev)| ev)
                    .collect());
            }
        }
        let skip = since.map_or(0, |s| s.0 as usize + 1);
        Ok(log.into_iter().skip(skip).collect())
    }

    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        let mut snap = self.inner.snapshot().await?;
        if matches!(self.fault, Fault::RenameSnapshotToolCall) {
            let mut renamed = false;
            for item in snap.messages.iter_mut() {
                if let Item::ToolCall { name, .. } = item {
                    if name == "transfer_funds" {
                        *name = "transfer_fund".into();
                        renamed = true;
                        break;
                    }
                }
            }
            assert!(renamed, "Broken: no transfer_funds ToolCall in snapshot");
        }
        Ok(snap)
    }
}

#[tokio::test]
async fn passthrough_wrapper_passes() {
    run_tool_deferred(|| async { broken(Fault::None) }).await;
}

#[tokio::test]
#[should_panic(expected = "run_tool_deferred: ToolDeferred d1: handoff_path")]
async fn reversed_handoff_path_fails() {
    run_tool_deferred(|| async { broken(Fault::ReverseHandoffPath) }).await;
}

#[tokio::test]
#[should_panic(expected = "run_tool_deferred: ToolDeferred d1: args")]
async fn args_through_f64_fails() {
    run_tool_deferred(|| async { broken(Fault::ArgsThroughF64) }).await;
}

#[tokio::test]
#[should_panic(expected = "run_tool_deferred: ToolDeferred d1: args")]
async fn called_args_in_deferral_fails() {
    run_tool_deferred(|| async { broken(Fault::CalledArgsIntoDeferral) }).await;
}

#[tokio::test]
#[should_panic(expected = "run_tool_deferred: snapshot().messages: JSON")]
async fn renamed_snapshot_tool_call_fails() {
    run_tool_deferred(|| async { broken(Fault::RenameSnapshotToolCall) }).await;
}

#[tokio::test]
#[should_panic(expected = "run_tool_deferred: events(None): event kinds")]
async fn swapped_events_fail() {
    run_tool_deferred(|| async { broken(Fault::SwapEvents2And3) }).await;
}

#[tokio::test]
#[should_panic(expected = "run_tool_deferred: events(Some(SequenceId(5))): event kinds")]
async fn same_sequence_per_batch_fails() {
    run_tool_deferred(|| async { broken(Fault::SameSequencePerBatch) }).await;
}
