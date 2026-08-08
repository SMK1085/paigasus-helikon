//! Maps [`AgentEvent`]s onto AG-UI event frames, with bracketing.
//!
//! # Why bracketing lives here
//!
//! `TokenDelta`/`ReasoningDelta` are bare fragments, but AG-UI requires balanced
//! `*_START` … `*_CONTENT` … `*_END` triples, and `STEP_STARTED` has no
//! "turn finished" event to close it. [`EventMapper`] owns those pairings so no
//! transport has to.
//!
//! # Ordering
//!
//! `ToolCallDelta` is emitted while the model stream drains; the matching
//! `ToolCallItem` only afterwards. `TOOL_CALL_START` is therefore derived from the
//! *first delta* for a call id — its `name` is populated only on that first delta,
//! which is exactly the START payload — and never from `ToolCallItem`, which would
//! put START after the ARGS frames it must precede.
//!
//! # One active tool-call span
//!
//! AG-UI permits a single active tool call at a time, but core's `ToolCallDelta`s for
//! different ids may interleave freely (`paigasus-helikon-core/src/model.rs:56`) and
//! its `ToolCallItem`s carry no ordering guarantee. Reconciling the two is what
//! [`EventMapper`]'s tool-call state exists for, and it owes an AG-UI client three
//! things: spans never overlap, one call id maps to exactly one span, and an id's
//! args reach the client in receipt order inside that span (a client concatenating
//! `TOOL_CALL_ARGS` deltas would otherwise rebuild malformed JSON). Deltas for an id
//! that arrive while another call holds the span are therefore buffered, not
//! interleaved, and replayed when that id takes the span or is flushed.

use std::collections::{HashMap, HashSet};

use paigasus_helikon_core::{AgentEvent, ContentPart, Item};
use serde_json::Value;

use crate::agui::types::event;

/// Which text-like pair is currently open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenText {
    /// No text or thinking block is open.
    None,
    /// A `TEXT_MESSAGE_START` … `TEXT_MESSAGE_END` pair is open.
    Message,
    /// A `THINKING_TEXT_MESSAGE_START` … `THINKING_TEXT_MESSAGE_END` pair is open.
    Thinking,
}

/// Which text-like pair [`EventMapper::open_text`] should open. Two variants, not
/// [`OpenText`]'s three — opening "no block" is not a request this method can be asked
/// to satisfy, so the type itself rules that call out rather than the body handling it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextKind {
    /// Open a `TEXT_MESSAGE_START` … `TEXT_MESSAGE_END` pair.
    Message,
    /// Open a `THINKING_TEXT_MESSAGE_START` … `THINKING_TEXT_MESSAGE_END` pair.
    Thinking,
}

/// Stateful `AgentEvent` → AG-UI frame mapper for exactly one run.
pub(crate) struct EventMapper {
    /// Client-supplied (or generated) thread id, echoed in `RUN_STARTED`/`RUN_FINISHED`.
    thread_id: String,
    /// Client-supplied (or generated) run id, echoed in `RUN_STARTED`/`RUN_FINISHED`.
    run_id: String,
    /// Which text-like pair (message or thinking) is currently open, if any.
    open_text: OpenText,
    /// Id of the currently-open text or thinking message.
    current_message: String,
    /// Monotonic counter behind `msg-N` ids. Stream-local uniqueness is all AG-UI
    /// requires, and deterministic ids let tests assert exact frame sequences.
    next_message: u32,
    /// Whether the current turn's assistant text has already been streamed via
    /// `TokenDelta`. Distinct from `open_text == OpenText::Message`: a `ToolCallDelta`
    /// closes the text block (via `close_text`) well before the matching
    /// `MessageOutput` arrives, so checking "is a message open right now" would miss
    /// that this text was already streamed and re-emit it.
    streamed_text: bool,
    /// Id of the call currently being streamed — has an open `TOOL_CALL_START` and no
    /// `TOOL_CALL_END` yet. `None` if no call is currently open.
    open_call: Option<String>,
    /// Tool name per call id, recorded from whichever `ToolCallDelta` first carries
    /// one. Only a call's *first* delta populates `name`, but the delta that finally
    /// opens that call's span may be a continuation (its buffer resuming, or a
    /// revisit), so the name has to be remembered rather than read off the delta at
    /// hand — otherwise those spans render as `"unknown"`.
    call_names: HashMap<String, String>,
    /// Calls whose deltas arrived while a *different* call was open: `(call_id,
    /// accumulated args)`. AG-UI permits only one active tool-call span at a
    /// time, while core's `ToolCallDelta`s for different ids may interleave freely
    /// (`paigasus-helikon-core/src/model.rs:56`) — buffering instead of closing and
    /// resuming preserves every args chunk exactly once and never emits an unmatched
    /// `TOOL_CALL_END`. A `Vec`, not a map, so flush order is deterministic.
    buffered_calls: Vec<(String, String)>,
    /// Call ids whose span has been emitted *and* closed. A `ToolCallItem` arrives
    /// long after the deltas it describes, by which time the call's span may already
    /// be closed — by its own delta path or by a foreign event that interrupted it.
    /// Without this record the item's arm cannot tell "already rendered" from "never
    /// streamed, synthesize it", and emits a second complete span for one call.
    completed_calls: HashSet<String>,
    /// Whether a `STEP_STARTED` is currently unmatched.
    step_open: bool,
}

impl EventMapper {
    /// Create a mapper for one run.
    // Constructed by the AG-UI SSE endpoint (SMA-461 Task 6, src/agui/sse.rs) and
    // the AG-UI WebSocket endpoint (SMA-461 Task 7, src/agui/ws.rs) for each run.
    // Until either lands, only this module's own tests construct one. Remove this
    // `allow` once either caller lands.
    #[allow(dead_code)]
    pub(crate) fn new(thread_id: String, run_id: String) -> Self {
        Self {
            thread_id,
            run_id,
            open_text: OpenText::None,
            current_message: String::new(),
            next_message: 0,
            streamed_text: false,
            open_call: None,
            call_names: HashMap::new(),
            buffered_calls: Vec::new(),
            completed_calls: HashSet::new(),
            step_open: false,
        }
    }

    /// Map one event, emitting any bracketing frames it implies.
    // Called per event by the AG-UI SSE endpoint (SMA-461 Task 6) and WebSocket
    // endpoint (SMA-461 Task 7) as they drain an `Agent` run. Until either lands,
    // only this module's own tests call it. Remove this `allow` once either
    // caller lands.
    #[allow(dead_code)]
    pub(crate) fn push(&mut self, ev: &AgentEvent) -> Vec<Value> {
        let mut out = Vec::new();
        match ev {
            AgentEvent::RunStarted { .. } => {
                out.push(event::run_started(&self.thread_id, &self.run_id));
            }
            AgentEvent::TurnStarted { .. } => {
                self.close_text(&mut out);
                self.close_step(&mut out);
                self.streamed_text = false;
                self.step_open = true;
                out.push(event::step_started("turn"));
            }
            AgentEvent::TokenDelta { text } => {
                self.close_call(&mut out);
                self.open_text(TextKind::Message, &mut out);
                if !text.is_empty() {
                    self.streamed_text = true;
                    out.push(event::text_message_content(&self.current_message, text));
                }
            }
            AgentEvent::ReasoningDelta { text } => {
                self.close_call(&mut out);
                self.open_text(TextKind::Thinking, &mut out);
                if !text.is_empty() {
                    out.push(event::thinking_content(&self.current_message, text));
                }
            }
            AgentEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                self.close_text(&mut out);
                if let Some(name) = name {
                    self.call_names
                        .entry(call_id.clone())
                        .or_insert_with(|| name.clone());
                }
                if self.open_call.as_deref() == Some(call_id.as_str()) {
                    // A continuation of the currently-open call.
                    out.push(event::tool_call_args(call_id, args_delta));
                } else if self.open_call.is_none() {
                    // Nothing else is streaming: open this call now. If chunks for
                    // it were buffered while another call held the single active
                    // span, replay them first — they were received before this one,
                    // and a client concatenating `TOOL_CALL_ARGS` deltas would
                    // otherwise rebuild the call's arguments out of order.
                    let buffered = self.take_buffered(call_id);
                    out.push(event::tool_call_start(
                        call_id,
                        self.name_of(call_id),
                        &self.current_message,
                    ));
                    if let Some(buffered) = buffered {
                        out.push(event::tool_call_args(call_id, &buffered));
                    }
                    out.push(event::tool_call_args(call_id, args_delta));
                    self.open_call = Some(call_id.clone());
                } else {
                    // A different call is streaming: AG-UI permits only one active
                    // span, so buffer this id's content rather than interleaving a
                    // second START — it is flushed as a complete span once its own
                    // `ToolCallItem` arrives, when a later delta resumes it above,
                    // or at the latest when the run ends.
                    match self.buffered_calls.iter_mut().find(|(id, _)| id == call_id) {
                        Some(entry) => entry.1.push_str(args_delta),
                        None => self
                            .buffered_calls
                            .push((call_id.clone(), args_delta.clone())),
                    }
                }
            }
            AgentEvent::ToolCallItem { item } => {
                self.close_text(&mut out);
                if let Item::ToolCall {
                    call_id,
                    name,
                    args,
                } = item
                {
                    if self.open_call.as_deref() == Some(call_id.as_str()) {
                        // The call this item describes is the one currently open.
                        self.close_call(&mut out);
                    } else if let Some(buffered) = self.take_buffered(call_id) {
                        // Deltas for this call were buffered while another call was
                        // open: flush the whole thing now as one complete,
                        // self-contained span. Nothing guarantees `ToolCallItem`s
                        // arrive in the order their calls opened, so close whatever
                        // is still open first — a complete span emitted inside
                        // another one would overlap.
                        self.close_call(&mut out);
                        out.push(event::tool_call_start(call_id, name, &self.current_message));
                        out.push(event::tool_call_args(call_id, &buffered));
                        out.push(event::tool_call_end(call_id));
                        self.completed_calls.insert(call_id.clone());
                    } else if self.completed_calls.contains(call_id) {
                        // This call's span is already emitted and closed — by its own
                        // deltas above, or by a foreign event (a `TokenDelta`, another
                        // call's item) that had to close it to take the single active
                        // span. Synthesizing here would emit a second complete span
                        // for one call id.
                    } else {
                        // No deltas were streamed for this call (non-streaming
                        // provider): synthesize the whole triple from the item's own
                        // args so the client still sees a complete call — again, not
                        // nested inside whatever call is currently open.
                        self.close_call(&mut out);
                        out.push(event::tool_call_start(call_id, name, &self.current_message));
                        out.push(event::tool_call_args(call_id, &args.to_string()));
                        out.push(event::tool_call_end(call_id));
                        self.completed_calls.insert(call_id.clone());
                    }
                } else {
                    out.push(self.custom("helikon.unknown", ev));
                }
            }
            AgentEvent::ToolOutputItem { item } => {
                self.close_call(&mut out);
                self.close_text(&mut out);
                if let Item::ToolResult { call_id, content } = item {
                    out.push(event::tool_call_result(call_id, &text_of(content)));
                } else {
                    out.push(self.custom("helikon.unknown", ev));
                }
            }
            AgentEvent::MessageOutput { item } => {
                if self.streamed_text {
                    // Deltas already streamed this text; only close it.
                    self.close_text(&mut out);
                } else {
                    // No deltas were emitted (non-streaming provider, workflow agent):
                    // synthesize the full triple, or the client renders nothing at all.
                    // Also closes any dangling thinking block if only `ReasoningDelta`s
                    // preceded this `MessageOutput` with no `TokenDelta` at all.
                    self.close_text(&mut out);
                    let content = match item {
                        Item::AssistantMessage { content, .. } => text_of(content),
                        _ => String::new(),
                    };
                    if !content.is_empty() {
                        let id = self.new_message_id();
                        out.push(event::text_message_start(&id));
                        out.push(event::text_message_content(&id, &content));
                        out.push(event::text_message_end(&id));
                    }
                }
                self.streamed_text = false;
            }
            AgentEvent::HandoffItem { .. } => out.push(self.custom("helikon.handoff", ev)),
            AgentEvent::AgentUpdated { .. } => out.push(self.custom("helikon.agent_updated", ev)),
            AgentEvent::GuardrailTriggered { .. } => out.push(self.custom("helikon.guardrail", ev)),
            AgentEvent::ApprovalRequested { .. } => out.push(self.custom("helikon.approval", ev)),
            AgentEvent::PermissionDenied { .. } => {
                out.push(self.custom("helikon.permission_denied", ev));
            }
            AgentEvent::RepairStarted { .. } => out.push(self.custom("helikon.repair", ev)),
            AgentEvent::StructuredOutputFailed { .. } => {
                out.push(self.custom("helikon.structured_output_failed", ev));
            }
            AgentEvent::RunCompleted { .. } => {
                self.close_all(&mut out);
                self.streamed_text = false;
                out.push(event::run_finished(&self.thread_id, &self.run_id));
            }
            AgentEvent::RunFailed { error } => {
                self.close_all(&mut out);
                self.streamed_text = false;
                out.push(event::run_error("AGENT_ERROR", error));
            }
            // `AgentEvent` is `#[non_exhaustive]`: a variant added to core later must
            // degrade to a lossless CUSTOM event rather than vanish.
            other => out.push(self.custom("helikon.unknown", other)),
        }
        out
    }

    /// Close any pairs still open. Only needed when a stream ends without a terminal
    /// event; the terminal arms already close every open pair themselves.
    // Called by the AG-UI SSE endpoint (SMA-461 Task 6) and WebSocket endpoint
    // (SMA-461 Task 7) if the underlying `Agent` stream ends without a terminal
    // event (e.g. the connection drops mid-run). Until either lands, only this
    // module's own tests call it. Remove this `allow` once either caller lands.
    #[allow(dead_code)]
    pub(crate) fn finish(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        self.close_all(&mut out);
        out
    }

    /// Build a `CUSTOM` frame carrying the full serialized event as its `value`.
    fn custom(&self, name: &str, ev: &AgentEvent) -> Value {
        event::custom(name, to_value(ev))
    }

    /// Allocate the next deterministic `msg-N` id.
    fn new_message_id(&mut self) -> String {
        let id = format!("msg-{}", self.next_message);
        self.next_message += 1;
        id
    }

    /// Ensure a text-like pair of kind `kind` is open, closing any different kind
    /// that is currently open first. A no-op if `kind` is already open.
    fn open_text(&mut self, kind: TextKind, out: &mut Vec<Value>) {
        let target = match kind {
            TextKind::Message => OpenText::Message,
            TextKind::Thinking => OpenText::Thinking,
        };
        if self.open_text == target {
            return;
        }
        self.close_text(out);
        let id = self.new_message_id();
        self.current_message = id.clone();
        match kind {
            TextKind::Message => out.push(event::text_message_start(&id)),
            TextKind::Thinking => out.push(event::thinking_start(&id)),
        }
        self.open_text = target;
    }

    /// Close whichever text-like pair is currently open, if any.
    fn close_text(&mut self, out: &mut Vec<Value>) {
        match self.open_text {
            OpenText::Message => out.push(event::text_message_end(&self.current_message)),
            OpenText::Thinking => out.push(event::thinking_end(&self.current_message)),
            OpenText::None => {}
        }
        self.open_text = OpenText::None;
    }

    /// Close whichever tool call is currently open, if any, and record it as done so
    /// its `ToolCallItem` — which always arrives later — does not render it twice.
    fn close_call(&mut self, out: &mut Vec<Value>) {
        if let Some(id) = self.open_call.take() {
            out.push(event::tool_call_end(&id));
            self.completed_calls.insert(id);
        }
    }

    /// The tool name recorded for `call_id`, falling back to `"unknown"` only when no
    /// delta for it ever carried one.
    fn name_of(&self, call_id: &str) -> &str {
        self.call_names
            .get(call_id)
            .map_or("unknown", String::as_str)
    }

    /// Remove and return the args buffered for `call_id` while a different call held
    /// the active span, if any.
    fn take_buffered(&mut self, call_id: &str) -> Option<String> {
        let pos = self
            .buffered_calls
            .iter()
            .position(|(id, _)| id == call_id)?;
        Some(self.buffered_calls.remove(pos).1)
    }

    /// Close a still-open `STEP_STARTED`, if any.
    fn close_step(&mut self, out: &mut Vec<Value>) {
        if self.step_open {
            out.push(event::step_finished("turn"));
            self.step_open = false;
        }
    }

    /// Close every still-open pair: text/thinking, the open tool call, every
    /// still-buffered tool call (each as a complete span), then the step. Nothing
    /// may be left buffered once a run terminates.
    fn close_all(&mut self, out: &mut Vec<Value>) {
        self.close_text(out);
        self.close_call(out);
        for (id, args) in std::mem::take(&mut self.buffered_calls) {
            out.push(event::tool_call_start(
                &id,
                self.name_of(&id),
                &self.current_message,
            ));
            out.push(event::tool_call_args(&id, &args));
            out.push(event::tool_call_end(&id));
            self.completed_calls.insert(id);
        }
        self.close_step(out);
    }
}

/// Serialize an event for a `CUSTOM` frame's `value`, degrading to `null` rather than
/// failing — a frame with a null value still tells the client the event happened.
fn to_value(ev: &AgentEvent) -> Value {
    serde_json::to_value(ev).unwrap_or(Value::Null)
}

/// Concatenate the text blocks of a content list, ignoring non-text parts.
fn text_of(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::{ContentPart, GuardrailKind, Item, TokenUsage};
    use std::collections::BTreeMap;

    fn mapper() -> EventMapper {
        EventMapper::new("t1".to_owned(), "r1".to_owned())
    }

    /// Collect the `type` of every frame produced for a sequence of events.
    fn types(events: &[AgentEvent]) -> Vec<String> {
        let mut m = mapper();
        let mut out = Vec::new();
        for e in events {
            for f in m.push(e) {
                out.push(f["type"].as_str().unwrap().to_owned());
            }
        }
        for f in m.finish() {
            out.push(f["type"].as_str().unwrap().to_owned());
        }
        out
    }

    /// Like [`types`] but keeps the full frame, not just its `type` — for tests that
    /// need other fields (`toolCallId`, `delta`, …).
    fn frames_of(events: &[AgentEvent]) -> Vec<Value> {
        let mut m = mapper();
        let mut out = Vec::new();
        for e in events {
            out.extend(m.push(e));
        }
        out.extend(m.finish());
        out
    }

    /// The `type` of every frame, for exact-sequence assertions.
    fn kinds_of(frames: &[Value]) -> Vec<&str> {
        frames.iter().map(|f| f["type"].as_str().unwrap()).collect()
    }

    fn tc_delta(call_id: &str, name: Option<&str>, args_delta: &str) -> AgentEvent {
        AgentEvent::ToolCallDelta {
            call_id: call_id.to_owned(),
            name: name.map(str::to_owned),
            args_delta: args_delta.to_owned(),
        }
    }

    fn tc_item(call_id: &str, name: &str) -> AgentEvent {
        AgentEvent::ToolCallItem {
            item: Item::ToolCall {
                call_id: call_id.to_owned(),
                name: name.to_owned(),
                args: serde_json::json!({}),
            },
        }
    }

    /// Assert that `TOOL_CALL_*` frames in `frames` nest properly: at most one call
    /// open at a time, every `TOOL_CALL_ARGS` carried by the call that is open (so
    /// no id's args can leak outside its own span), every `END` matching whichever
    /// call is open, and nothing left open by the end of the sequence.
    ///
    /// Nesting only — [`assert_tool_calls_well_formed`] is the full invariant and is
    /// what tests should normally use. This weaker form exists for the one ordering
    /// that cannot satisfy the stronger one: a delta revisiting an id whose span a
    /// foreign event already closed can only be rendered as a *second* span for that
    /// id (see `a_delta_revisiting_a_closed_id_keeps_its_args_and_its_name`).
    fn assert_tool_call_nesting(frames: &[Value]) {
        let mut open: Option<&str> = None;
        for f in frames {
            match f["type"].as_str().unwrap() {
                "TOOL_CALL_START" => {
                    let id = f["toolCallId"].as_str().unwrap();
                    assert!(
                        open.is_none(),
                        "TOOL_CALL_START for {id:?} while {open:?} is still open: {frames:?}"
                    );
                    open = Some(id);
                }
                "TOOL_CALL_ARGS" => {
                    let id = f["toolCallId"].as_str().unwrap();
                    assert_eq!(
                        open,
                        Some(id),
                        "TOOL_CALL_ARGS for {id:?} outside that call's own span \
                         (open: {open:?}): {frames:?}"
                    );
                }
                "TOOL_CALL_END" => {
                    let id = f["toolCallId"].as_str().unwrap();
                    assert_eq!(
                        open,
                        Some(id),
                        "TOOL_CALL_END for {id:?} does not match the open call {open:?}: {frames:?}"
                    );
                    open = None;
                }
                _ => {}
            }
        }
        assert!(open.is_none(), "a tool call was left open: {frames:?}");
    }

    /// Assert every tool-call invariant the mapper owes an AG-UI client, for the
    /// `frames` produced by `events`:
    ///
    /// 1. spans nest and never overlap ([`assert_tool_call_nesting`]);
    /// 2. each call id opens **at most once** across the whole sequence — a nesting
    ///    check cannot see a duplicate `START`, yet one call split across two spans
    ///    is exactly what breaks a client that keys state on `toolCallId`;
    /// 3. each id's args are emitted **in receipt order and in full** — concatenating
    ///    the `TOOL_CALL_ARGS` deltas the mapper emitted for an id must reproduce the
    ///    concatenation of that id's `ToolCallDelta` chunks in the order they arrived.
    ///    Reordering or dropping a chunk yields malformed JSON on the client, and (1)
    ///    and (2) are both blind to it.
    ///
    /// Ids with no `ToolCallDelta` at all (a non-streaming provider's `ToolCallItem`)
    /// carry no receipt order to check, so (3) skips them.
    fn assert_tool_calls_well_formed(events: &[AgentEvent], frames: &[Value]) {
        assert_tool_call_nesting(frames);

        let mut started: Vec<&str> = Vec::new();
        for f in frames.iter().filter(|f| f["type"] == "TOOL_CALL_START") {
            let id = f["toolCallId"].as_str().unwrap();
            assert!(
                !started.contains(&id),
                "TOOL_CALL_START for {id:?} emitted twice — one call must map to \
                 exactly one span: {frames:?}"
            );
            started.push(id);
        }

        let mut received: BTreeMap<&str, String> = BTreeMap::new();
        for e in events {
            if let AgentEvent::ToolCallDelta {
                call_id,
                args_delta,
                ..
            } = e
            {
                received
                    .entry(call_id.as_str())
                    .or_default()
                    .push_str(args_delta);
            }
        }
        let mut emitted: BTreeMap<&str, String> = BTreeMap::new();
        for f in frames.iter().filter(|f| f["type"] == "TOOL_CALL_ARGS") {
            emitted
                .entry(f["toolCallId"].as_str().unwrap())
                .or_default()
                .push_str(f["delta"].as_str().unwrap());
        }
        for (id, want) in &received {
            let got = emitted.get(id).map_or("", String::as_str);
            assert_eq!(
                got, want,
                "args for {id:?} must be emitted in receipt order and in full: {frames:?}"
            );
        }
    }

    fn assistant(text: &str) -> Item {
        Item::AssistantMessage {
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
            agent: None,
        }
    }

    #[test]
    fn run_lifecycle_maps_to_run_started_and_finished() {
        let t = types(&[
            AgentEvent::RunStarted {
                agent: "a".to_owned(),
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ]);
        assert_eq!(t, vec!["RUN_STARTED", "RUN_FINISHED"]);
    }

    #[test]
    fn token_deltas_are_bracketed_exactly_once() {
        let t = types(&[
            AgentEvent::RunStarted {
                agent: "a".to_owned(),
            },
            AgentEvent::TokenDelta {
                text: "he".to_owned(),
            },
            AgentEvent::TokenDelta {
                text: "llo".to_owned(),
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ]);
        assert_eq!(
            t,
            vec![
                "RUN_STARTED",
                "TEXT_MESSAGE_START",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_END",
                "RUN_FINISHED",
            ]
        );
    }

    /// Regression for the ordering bug: `TOOL_CALL_START` must precede every
    /// `TOOL_CALL_ARGS` for the same id, even though `ToolCallItem` arrives *after* the
    /// deltas in the real core event order.
    #[test]
    fn tool_call_start_precedes_args_despite_item_arriving_last() {
        let t = types(&[
            AgentEvent::ToolCallDelta {
                call_id: "tc1".to_owned(),
                name: Some("search".to_owned()),
                args_delta: "{\"q\":".to_owned(),
            },
            AgentEvent::ToolCallDelta {
                call_id: "tc1".to_owned(),
                name: None,
                args_delta: "\"x\"}".to_owned(),
            },
            AgentEvent::ToolCallItem {
                item: Item::ToolCall {
                    call_id: "tc1".to_owned(),
                    name: "search".to_owned(),
                    args: serde_json::json!({"q": "x"}),
                },
            },
        ]);
        assert_eq!(
            t,
            vec![
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
            ]
        );
        let start = t.iter().position(|x| x == "TOOL_CALL_START").unwrap();
        let first_args = t.iter().position(|x| x == "TOOL_CALL_ARGS").unwrap();
        assert!(start < first_args, "START must precede ARGS");
    }

    /// A non-streaming provider emits no deltas at all: `ToolCallItem` must then
    /// synthesize the whole triple rather than emit a bare, unmatched END.
    #[test]
    fn tool_call_item_without_deltas_synthesizes_the_full_triple() {
        let t = types(&[AgentEvent::ToolCallItem {
            item: Item::ToolCall {
                call_id: "tc9".to_owned(),
                name: "lookup".to_owned(),
                args: serde_json::json!({"a": 1}),
            },
        }]);
        assert_eq!(
            t,
            vec!["TOOL_CALL_START", "TOOL_CALL_ARGS", "TOOL_CALL_END"]
        );
    }

    /// Regression: an agent that emits only `MessageOutput` (non-streaming providers,
    /// workflow agents, the crate's own test fixtures) must still produce *visible*
    /// text. Asserting balance alone would pass on an empty stream.
    #[test]
    fn message_output_without_deltas_produces_visible_text() {
        let mut m = mapper();
        let frames = m.push(&AgentEvent::MessageOutput {
            item: assistant("the whole answer"),
        });
        let kinds: Vec<&str> = frames.iter().map(|f| f["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            vec![
                "TEXT_MESSAGE_START",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_END"
            ]
        );
        assert_eq!(frames[1]["delta"], "the whole answer");
    }

    /// When deltas already streamed the text, `MessageOutput` only closes the run — it
    /// must not repeat the text.
    #[test]
    fn message_output_after_deltas_only_closes_the_run() {
        let mut m = mapper();
        let _ = m.push(&AgentEvent::TokenDelta {
            text: "streamed".to_owned(),
        });
        let frames = m.push(&AgentEvent::MessageOutput {
            item: assistant("streamed"),
        });
        let kinds: Vec<&str> = frames.iter().map(|f| f["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["TEXT_MESSAGE_END"]);
    }

    /// Regression: `STEP_STARTED` is a paired event with no "turn finished" source
    /// event, so the mapper must close it on the next turn and on the terminal.
    #[test]
    fn steps_are_balanced_across_turns() {
        let t = types(&[
            AgentEvent::TurnStarted { turn: 0 },
            AgentEvent::TurnStarted { turn: 1 },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ]);
        assert_eq!(
            t,
            vec![
                "STEP_STARTED",
                "STEP_FINISHED",
                "STEP_STARTED",
                "STEP_FINISHED",
                "RUN_FINISHED",
            ]
        );
    }

    /// Every opened pair must close even when the run fails mid-text.
    #[test]
    fn run_failed_mid_text_closes_every_open_pair() {
        let t = types(&[
            AgentEvent::TurnStarted { turn: 0 },
            AgentEvent::TokenDelta {
                text: "partial".to_owned(),
            },
            AgentEvent::ReasoningDelta {
                text: "hmm".to_owned(),
            },
            AgentEvent::RunFailed {
                error: "boom".to_owned(),
            },
        ]);
        assert_eq!(
            t,
            vec![
                "STEP_STARTED",
                "TEXT_MESSAGE_START",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_END",
                "THINKING_TEXT_MESSAGE_START",
                "THINKING_TEXT_MESSAGE_CONTENT",
                "THINKING_TEXT_MESSAGE_END",
                "STEP_FINISHED",
                "RUN_ERROR",
            ]
        );
    }

    #[test]
    fn interleaved_text_and_reasoning_never_overlap() {
        let t = types(&[
            AgentEvent::TokenDelta {
                text: "a".to_owned(),
            },
            AgentEvent::ReasoningDelta {
                text: "b".to_owned(),
            },
            AgentEvent::TokenDelta {
                text: "c".to_owned(),
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ]);
        assert_eq!(
            t,
            vec![
                "TEXT_MESSAGE_START",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_END",
                "THINKING_TEXT_MESSAGE_START",
                "THINKING_TEXT_MESSAGE_CONTENT",
                "THINKING_TEXT_MESSAGE_END",
                "TEXT_MESSAGE_START",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_END",
                "RUN_FINISHED",
            ]
        );
    }

    #[test]
    fn tool_output_maps_to_tool_call_result() {
        let t = types(&[AgentEvent::ToolOutputItem {
            item: Item::ToolResult {
                call_id: "tc1".to_owned(),
                content: vec![ContentPart::Text {
                    text: "done".to_owned(),
                }],
            },
        }]);
        assert_eq!(t, vec!["TOOL_CALL_RESULT"]);
    }

    #[test]
    fn helikon_specific_events_become_namespaced_custom_events() {
        let cases: Vec<(AgentEvent, &str)> = vec![
            (
                AgentEvent::GuardrailTriggered {
                    kind: GuardrailKind::InputPolicy,
                    info: serde_json::json!({}),
                },
                "helikon.guardrail",
            ),
            (
                AgentEvent::ApprovalRequested {
                    call_id: "c".to_owned(),
                    tool: "t".to_owned(),
                    args: serde_json::json!({}),
                },
                "helikon.approval",
            ),
            (
                AgentEvent::PermissionDenied {
                    tool: "t".to_owned(),
                    reason: "nope".to_owned(),
                },
                "helikon.permission_denied",
            ),
            (
                AgentEvent::HandoffItem {
                    from: "a".to_owned(),
                    to: "b".to_owned(),
                },
                "helikon.handoff",
            ),
            (
                AgentEvent::AgentUpdated {
                    agent: "b".to_owned(),
                },
                "helikon.agent_updated",
            ),
            (AgentEvent::RepairStarted { attempt: 1 }, "helikon.repair"),
            (
                AgentEvent::StructuredOutputFailed {
                    schema_errors: vec!["e".to_owned()],
                    final_text: "x".to_owned(),
                },
                "helikon.structured_output_failed",
            ),
        ];
        for (event, expected_name) in cases {
            let mut m = mapper();
            let frames = m.push(&event);
            assert_eq!(frames.len(), 1, "expected one frame for {expected_name}");
            assert_eq!(frames[0]["type"], "CUSTOM");
            assert_eq!(frames[0]["name"], expected_name);
            assert!(
                frames[0]["value"].is_object(),
                "the original event JSON must be carried"
            );
        }
    }

    /// `AgentEvent` is `#[non_exhaustive]`, so the mapper's `match` needs a wildcard.
    /// This asserts every variant maps to at least one frame, making the count visible
    /// in review even though the compiler cannot enforce exhaustiveness.
    #[test]
    fn every_known_variant_maps_to_at_least_one_frame() {
        let all: Vec<AgentEvent> = vec![
            AgentEvent::RunStarted {
                agent: "a".to_owned(),
            },
            AgentEvent::TurnStarted { turn: 0 },
            AgentEvent::TokenDelta {
                text: "t".to_owned(),
            },
            AgentEvent::ReasoningDelta {
                text: "r".to_owned(),
            },
            AgentEvent::ToolCallDelta {
                call_id: "c".to_owned(),
                name: Some("n".to_owned()),
                args_delta: "{}".to_owned(),
            },
            AgentEvent::MessageOutput {
                item: assistant("m"),
            },
            AgentEvent::ToolCallItem {
                item: Item::ToolCall {
                    call_id: "c2".to_owned(),
                    name: "n".to_owned(),
                    args: serde_json::json!({}),
                },
            },
            AgentEvent::ToolOutputItem {
                item: Item::ToolResult {
                    call_id: "c2".to_owned(),
                    content: vec![ContentPart::Text {
                        text: "o".to_owned(),
                    }],
                },
            },
            AgentEvent::HandoffItem {
                from: "a".to_owned(),
                to: "b".to_owned(),
            },
            AgentEvent::AgentUpdated {
                agent: "b".to_owned(),
            },
            AgentEvent::GuardrailTriggered {
                kind: GuardrailKind::InputPolicy,
                info: serde_json::json!({}),
            },
            AgentEvent::ApprovalRequested {
                call_id: "c".to_owned(),
                tool: "t".to_owned(),
                args: serde_json::json!({}),
            },
            AgentEvent::PermissionDenied {
                tool: "t".to_owned(),
                reason: "r".to_owned(),
            },
            AgentEvent::RepairStarted { attempt: 1 },
            AgentEvent::StructuredOutputFailed {
                schema_errors: vec![],
                final_text: String::new(),
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
            AgentEvent::RunFailed {
                error: "e".to_owned(),
            },
        ];
        assert_eq!(
            all.len(),
            17,
            "AgentEvent gained or lost a variant — update the mapper"
        );
        for event in &all {
            let mut m = mapper();
            assert!(
                !m.push(event).is_empty(),
                "no frame produced for {event:?} — the wildcard arm must not drop events"
            );
        }
    }

    /// Regression (review round 1, Important 1): a turn that narrates before calling a
    /// tool streams `TokenDelta`s, then a `ToolCallDelta` closes the text block, then
    /// `MessageOutput` + `ToolCallItem` both arrive from `transition()`. The mapper must
    /// recognize the text was already streamed and not re-synthesize it.
    #[test]
    fn message_output_after_deltas_with_an_intervening_tool_call_is_not_repeated() {
        let t = types(&[
            AgentEvent::TokenDelta {
                text: "narrating".to_owned(),
            },
            AgentEvent::ToolCallDelta {
                call_id: "tc1".to_owned(),
                name: Some("search".to_owned()),
                args_delta: "{}".to_owned(),
            },
            AgentEvent::MessageOutput {
                item: assistant("narrating"),
            },
            AgentEvent::ToolCallItem {
                item: Item::ToolCall {
                    call_id: "tc1".to_owned(),
                    name: "search".to_owned(),
                    args: serde_json::json!({}),
                },
            },
        ]);
        let content_count = t.iter().filter(|k| *k == "TEXT_MESSAGE_CONTENT").count();
        assert_eq!(
            content_count, 1,
            "assistant text must not be emitted twice: {t:?}"
        );
    }

    /// Regression (review round 1, Important 2): a tool call whose deltas streamed but
    /// whose `ToolCallItem` never arrives (the run fails first) must still have its
    /// `TOOL_CALL_START` closed — the mapper's own closing-invariant applies to tool
    /// calls exactly as it does to text/thinking/step.
    #[test]
    fn run_failed_closes_an_open_tool_call() {
        let t = types(&[
            AgentEvent::ToolCallDelta {
                call_id: "tc1".to_owned(),
                name: Some("search".to_owned()),
                args_delta: "{}".to_owned(),
            },
            AgentEvent::RunFailed {
                error: "boom".to_owned(),
            },
        ]);
        assert_eq!(
            t,
            vec![
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
                "RUN_ERROR"
            ]
        );
    }

    /// Regression (review round 1, Important 4): two tool calls whose deltas and items
    /// interleave must never have overlapping START/END spans — AG-UI's reference
    /// client tracks a single active tool call and rejects a second START while one is
    /// open.
    #[test]
    fn parallel_tool_calls_do_not_overlap() {
        let t = types(&[
            AgentEvent::ToolCallDelta {
                call_id: "tc1".to_owned(),
                name: Some("a".to_owned()),
                args_delta: "{".to_owned(),
            },
            AgentEvent::ToolCallDelta {
                call_id: "tc2".to_owned(),
                name: Some("b".to_owned()),
                args_delta: "{".to_owned(),
            },
            AgentEvent::ToolCallItem {
                item: Item::ToolCall {
                    call_id: "tc1".to_owned(),
                    name: "a".to_owned(),
                    args: serde_json::json!({}),
                },
            },
            AgentEvent::ToolCallItem {
                item: Item::ToolCall {
                    call_id: "tc2".to_owned(),
                    name: "b".to_owned(),
                    args: serde_json::json!({}),
                },
            },
        ]);
        assert_eq!(
            t,
            vec![
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
            ],
            "no overlap: the first call's END must land before the second call's START: {t:?}"
        );
        let first_end = t.iter().position(|x| x == "TOOL_CALL_END").unwrap();
        let second_start = t.iter().rposition(|x| x == "TOOL_CALL_START").unwrap();
        assert!(first_end < second_start, "spans overlap: {t:?}");
    }

    /// Regression (review round 1, Important 3): `ContentPart::ToolUse` is a supported,
    /// text-free `AssistantMessage` shape (Anthropic-style tool_use blocks). Synthesizing
    /// a triple for it would burn a message id and render a blank bubble with an empty
    /// `delta`, which AG-UI's `TEXT_MESSAGE_CONTENT` schema forbids.
    #[test]
    fn message_output_with_only_non_text_content_does_not_synthesize_a_blank_bubble() {
        let mut m = mapper();
        let frames = m.push(&AgentEvent::MessageOutput {
            item: Item::AssistantMessage {
                content: vec![ContentPart::ToolUse {
                    call_id: "tc1".to_owned(),
                    name: "search".to_owned(),
                    args: serde_json::json!({}),
                }],
                agent: None,
            },
        });
        assert!(
            frames.is_empty(),
            "no TEXT_MESSAGE_* frames for text-free content: {frames:?}"
        );
    }

    /// Regression (review round 1, Important 3): an empty `TokenDelta` must not emit an
    /// empty `TEXT_MESSAGE_CONTENT` frame — AG-UI's schema constrains `delta` to be
    /// non-empty.
    #[test]
    fn empty_token_delta_does_not_emit_an_empty_content_frame() {
        let t = types(&[AgentEvent::TokenDelta {
            text: String::new(),
        }]);
        assert!(
            !t.iter().any(|k| k == "TEXT_MESSAGE_CONTENT"),
            "empty delta must not produce a CONTENT frame: {t:?}"
        );
    }

    /// Regression (review round 2, controller-escalated Important): an empty
    /// `TokenDelta` must not mark the turn as "already streamed" — doing so suppresses
    /// `MessageOutput`'s synthesis branch and the entire assistant message vanishes.
    #[test]
    fn empty_token_delta_does_not_suppress_message_output_synthesis() {
        let mut m = mapper();
        let mut frames = Vec::new();
        frames.extend(m.push(&AgentEvent::TokenDelta {
            text: String::new(),
        }));
        frames.extend(m.push(&AgentEvent::MessageOutput {
            item: assistant("real text"),
        }));
        frames.extend(m.push(&AgentEvent::RunCompleted {
            usage: TokenUsage::default(),
        }));
        let content_frames: Vec<&Value> = frames
            .iter()
            .filter(|f| f["type"] == "TEXT_MESSAGE_CONTENT")
            .collect();
        assert_eq!(
            content_frames.len(),
            1,
            "assistant text must not be dropped: {frames:?}"
        );
        assert_eq!(content_frames[0]["delta"], "real text");
    }

    /// Regression (review round 3, replaces round 2's Minor fix): round 2's "close
    /// and resume" fix re-opened an already-closed id with a bogus `"unknown"` name
    /// and, worse, could later orphan its `TOOL_CALL_END` (round 3's own Important
    /// finding). Round 3 replaces closing-and-resuming with buffering: a delta for a
    /// different id while one call is open is buffered rather than interleaved, so
    /// `delta(a) delta(b) delta(a)` streams only `a` — `b`'s chunk sits buffered
    /// until its own `ToolCallItem` or the run's terminal flushes it.
    #[test]
    fn a_delta_for_a_different_id_is_buffered_not_interleaved() {
        let events = vec![
            tc_delta("a", Some("first"), "1"),
            tc_delta("b", Some("second"), "2"),
            tc_delta("a", None, "3"),
        ];
        let frames = frames_of(&events);
        // `finish()` (via `frames_of`) drains the still-buffered "b" at the end, so
        // check only the frames up to that drain for the "streaming" assertion.
        let kinds: Vec<&str> = frames.iter().map(|f| f["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds[..3],
            ["TOOL_CALL_START", "TOOL_CALL_ARGS", "TOOL_CALL_ARGS"],
            "only a streams; b is buffered, not interleaved: {frames:?}"
        );
        let start_ids: Vec<&str> = frames
            .iter()
            .filter(|f| f["type"] == "TOOL_CALL_START")
            .map(|f| f["toolCallId"].as_str().unwrap())
            .collect();
        assert_eq!(
            start_ids,
            vec!["a", "b"],
            "each id must be started exactly once overall (a while streaming, b when \
             flushed at the terminal): {frames:?}"
        );
        let a_args: Vec<&str> = frames
            .iter()
            .filter(|f| f["type"] == "TOOL_CALL_ARGS" && f["toolCallId"] == "a")
            .map(|f| f["delta"].as_str().unwrap())
            .collect();
        assert_eq!(a_args, vec!["1", "3"], "a's own args, in order: {frames:?}");
        assert_tool_calls_well_formed(&events, &frames);
    }

    /// Regression (review round 3, Important — controller-escalated): round 2's
    /// "close and resume" design could emit a `TOOL_CALL_END` with no matching open
    /// `TOOL_CALL_START` once an id was revisited after closing, silently losing the
    /// args chunk that triggered it. Buffering must never orphan an `END` or drop a
    /// chunk, however text/tool-call events interleave.
    #[test]
    fn interleaved_deltas_then_a_token_delta_never_orphan_a_tool_call_end() {
        let events = vec![
            tc_delta("a", Some("first"), "1"),
            tc_delta("b", Some("second"), "2"),
            tc_delta("a", None, "3"),
            AgentEvent::TokenDelta {
                text: "hi".to_owned(),
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        let a_start = frames
            .iter()
            .position(|f| f["type"] == "TOOL_CALL_START" && f["toolCallId"] == "a")
            .unwrap();
        let a_end = frames
            .iter()
            .position(|f| f["type"] == "TOOL_CALL_END" && f["toolCallId"] == "a")
            .unwrap();
        let a_args: Vec<&str> = frames
            .iter()
            .enumerate()
            .filter(|(i, f)| {
                *i > a_start
                    && *i < a_end
                    && f["type"] == "TOOL_CALL_ARGS"
                    && f["toolCallId"] == "a"
            })
            .map(|(_, f)| f["delta"].as_str().unwrap())
            .collect();
        assert_eq!(
            a_args,
            vec!["1", "3"],
            "both of a's chunks must land inside its own span: {frames:?}"
        );
    }

    /// Regression (review round 3): a call buffered while another streamed, with no
    /// `ToolCallItem` of its own before the run ends, must be flushed as a complete
    /// span before the terminal frame — never silently dropped.
    #[test]
    fn a_buffered_call_is_flushed_as_a_complete_span_before_the_run_finishes() {
        let events = vec![
            tc_delta("a", Some("first"), "1"),
            tc_delta("b", Some("second"), "2"),
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        let kinds = kinds_of(&frames);
        let finished_at = kinds.iter().position(|k| *k == "RUN_FINISHED").unwrap();
        assert!(
            kinds[..finished_at].contains(&"TOOL_CALL_END")
                && kinds[..finished_at]
                    .iter()
                    .filter(|k| **k == "TOOL_CALL_END")
                    .count()
                    == 2,
            "both a and b must be fully closed before RUN_FINISHED: {frames:?}"
        );
        let b_frames: Vec<&Value> = frames
            .iter()
            .filter(|f| f.get("toolCallId") == Some(&Value::String("b".to_owned())))
            .collect();
        assert_eq!(
            b_frames
                .iter()
                .map(|f| f["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["TOOL_CALL_START", "TOOL_CALL_ARGS", "TOOL_CALL_END"],
            "b flushed as one complete, self-contained span: {frames:?}"
        );
    }

    /// Regression (review round 3): a cheap general invariant check across several
    /// representative tool-call orderings — sequential, interleaved-then-both-items,
    /// buffered-until-terminal, and non-streaming — `TOOL_CALL_START`/`TOOL_CALL_END`
    /// must always form a properly nested, non-overlapping sequence.
    #[test]
    fn tool_call_spans_are_always_well_formed() {
        let scenarios: Vec<Vec<AgentEvent>> = vec![
            vec![tc_delta("x", Some("n"), "1"), tc_item("x", "n")],
            vec![
                tc_delta("a", Some("a"), "1"),
                tc_delta("b", Some("b"), "2"),
                tc_item("a", "a"),
                tc_item("b", "b"),
            ],
            vec![
                tc_delta("a", Some("a"), "1"),
                tc_delta("b", Some("b"), "2"),
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                },
            ],
            vec![tc_item("z", "z")],
            // A revisited id, terminated via its own item rather than a terminal
            // event — the scenario round 2's "close and resume" design fails on.
            vec![
                tc_delta("a", Some("a"), "1"),
                tc_delta("b", Some("b"), "2"),
                tc_delta("a", None, "3"),
                tc_item("a", "a"),
                tc_item("b", "b"),
            ],
            // Items arriving in an order core does not currently produce (`b`'s
            // before `a`'s, and an item for a call that never streamed) — nothing
            // documents that ordering as a guarantee, so the mapper must hold.
            vec![
                tc_delta("a", Some("a"), "1"),
                tc_delta("b", Some("b"), "2"),
                tc_item("b", "b"),
                tc_item("a", "a"),
            ],
            vec![
                tc_delta("a", Some("a"), "1"),
                tc_item("z", "z"),
                tc_item("a", "a"),
            ],
            // A tool call whose span a foreign event closes, then its own item.
            vec![
                tc_delta("a", Some("a"), "1"),
                AgentEvent::TokenDelta {
                    text: "hi".to_owned(),
                },
                tc_item("a", "a"),
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                },
            ],
        ];
        for events in scenarios {
            let frames = frames_of(&events);
            assert_tool_calls_well_formed(&events, &frames);
        }
    }

    /// Regression (review round 2, Minor): the single-active-span invariant that
    /// applies to a second tool call's START must also apply to a foreign event type —
    /// a `TokenDelta` (or `ReasoningDelta`/`ToolOutputItem`) arriving while a tool call
    /// is open must close that call first.
    #[test]
    fn a_token_delta_closes_any_open_tool_call_first() {
        let t = types(&[
            AgentEvent::ToolCallDelta {
                call_id: "tc1".to_owned(),
                name: Some("search".to_owned()),
                args_delta: "{}".to_owned(),
            },
            AgentEvent::TokenDelta {
                text: "x".to_owned(),
            },
        ]);
        let end = t
            .iter()
            .position(|k| k == "TOOL_CALL_END")
            .expect("the open tool call must close");
        let start = t
            .iter()
            .position(|k| k == "TEXT_MESSAGE_START")
            .expect("text must open");
        assert!(
            end < start,
            "TOOL_CALL_END must precede TEXT_MESSAGE_START: {t:?}"
        );
    }

    /// Regression (review round 4, Important 1): a `ToolCallItem` whose call was
    /// already closed — here by the `TokenDelta` that interrupted it — must emit
    /// nothing. Round 2 had a guard for this; round 3 dropped it with no
    /// replacement, so the item's arm fell through to the "no deltas were streamed"
    /// synthesis branch and re-emitted a second, complete span for the same id.
    /// Reachable with a single tool call plus one token delta.
    #[test]
    fn an_item_for_an_already_closed_call_does_not_re_emit_its_span() {
        let events = vec![
            tc_delta("a", Some("search"), "{}"),
            AgentEvent::TokenDelta {
                text: "hi".to_owned(),
            },
            tc_item("a", "search"),
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        assert_eq!(
            kinds_of(&frames),
            vec![
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
                "TEXT_MESSAGE_START",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_END",
                "RUN_FINISHED",
            ],
            "the item's call is already closed — it must add no frames: {frames:?}"
        );
    }

    /// Regression (review round 4, Important 2): when the call that was streaming
    /// closes and a *buffered* id's next delta arrives, the mapper must resume that
    /// id's buffer rather than open a fresh span for it — otherwise the id gets two
    /// `TOOL_CALL_START`s (the first carrying `"unknown"`, since a continuation
    /// delta has no `name`) and its args are emitted out of receipt order, which
    /// concatenates to malformed JSON on the client.
    #[test]
    fn a_delta_resuming_a_buffered_call_does_not_open_a_second_span() {
        let events = vec![
            tc_delta("a", Some("first"), "1"),
            tc_delta("b", Some("second"), "2"),
            AgentEvent::TokenDelta {
                text: "hi".to_owned(),
            },
            tc_delta("b", None, "3"),
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        assert_eq!(
            kinds_of(&frames),
            vec![
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
                "TEXT_MESSAGE_START",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_END",
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
                "RUN_FINISHED",
            ],
            "b's buffered chunk and its resumed chunk belong to one span: {frames:?}"
        );
        let b_start = frames
            .iter()
            .find(|f| f["type"] == "TOOL_CALL_START" && f["toolCallId"] == "b")
            .unwrap();
        assert_eq!(
            b_start["toolCallName"], "second",
            "the resumed span keeps the name from b's first delta: {frames:?}"
        );
    }

    /// Regression (review round 4, Minor 3a): `ToolCallItem`s need not arrive in the
    /// order their calls opened. An item flushing a *buffered* call while a
    /// different call is still streaming must close that call first — round 3
    /// dropped the `close_call` from this branch, nesting the flushed span inside
    /// the open one.
    #[test]
    fn an_item_for_a_buffered_call_closes_the_open_call_first() {
        let events = vec![
            tc_delta("a", Some("first"), "1"),
            tc_delta("b", Some("second"), "2"),
            tc_item("b", "second"),
            tc_item("a", "first"),
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        assert_eq!(
            kinds_of(&frames),
            vec![
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
            ],
            "a closes before b's flushed span opens: {frames:?}"
        );
    }

    /// Regression (review round 4, Minor 3b): the same omission in the other
    /// non-open branch — an item for a call that never streamed any delta
    /// synthesizes a whole span, which must not nest inside a call that is open.
    #[test]
    fn an_item_for_a_call_with_no_deltas_closes_the_open_call_first() {
        let events = vec![
            tc_delta("a", Some("first"), "1"),
            tc_item("z", "other"),
            tc_item("a", "first"),
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        assert_eq!(
            kinds_of(&frames),
            vec![
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
            ],
            "a closes before z's synthesized span opens: {frames:?}"
        );
    }

    /// Regression (review round 4, Minor 4): only a call's *first* delta carries its
    /// `name`, and that delta may be the one that gets buffered with `name: None`
    /// if the name arrives on a later chunk. The mapper must remember the name per
    /// call id, or a buffered span flushed at the terminal renders as `"unknown"`.
    #[test]
    fn a_buffered_calls_name_is_recorded_from_whichever_delta_carries_it() {
        let events = vec![
            tc_delta("a", Some("first"), "1"),
            tc_delta("b", None, "2"),
            tc_delta("b", Some("second"), "3"),
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        let b_start = frames
            .iter()
            .find(|f| f["type"] == "TOOL_CALL_START" && f["toolCallId"] == "b")
            .unwrap();
        assert_eq!(
            b_start["toolCallName"], "second",
            "the name from b's later delta must reach its flushed span: {frames:?}"
        );
    }

    /// Regression (review round 4, Minor 4): when a buffered call is flushed by its
    /// own `ToolCallItem`, that item carries the authoritative tool name — falling
    /// back to `"unknown"` throws it away.
    #[test]
    fn a_buffered_call_flushed_by_its_item_uses_the_items_name() {
        let events = vec![
            tc_delta("a", Some("first"), "1"),
            tc_delta("b", None, "2"),
            tc_item("b", "lookup"),
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        let b_start = frames
            .iter()
            .find(|f| f["type"] == "TOOL_CALL_START" && f["toolCallId"] == "b")
            .unwrap();
        assert_eq!(
            b_start["toolCallName"], "lookup",
            "the item's own name is authoritative: {frames:?}"
        );
    }

    /// Documents the one ordering that *cannot* satisfy one-span-per-id: a delta
    /// revisiting an id whose span a foreign event (here a `TokenDelta`) already
    /// closed. An `END` is not retractable, so the choice is a second span for that
    /// id or a dropped args chunk — and dropping content is strictly worse. What the
    /// mapper still owes is asserted here: proper nesting, no lost or reordered
    /// args, and the real tool name on *both* spans (a client keyed on
    /// `toolCallId` concatenates them back into one correct call).
    #[test]
    fn a_delta_revisiting_a_closed_id_keeps_its_args_and_its_name() {
        let events = vec![
            tc_delta("a", Some("first"), "1"),
            AgentEvent::TokenDelta {
                text: "hi".to_owned(),
            },
            tc_delta("a", None, "3"),
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ];
        let frames = frames_of(&events);
        assert_tool_call_nesting(&frames);
        let a_args: String = frames
            .iter()
            .filter(|f| f["type"] == "TOOL_CALL_ARGS" && f["toolCallId"] == "a")
            .map(|f| f["delta"].as_str().unwrap())
            .collect();
        assert_eq!(a_args, "13", "no args chunk may be dropped: {frames:?}");
        let names: Vec<&str> = frames
            .iter()
            .filter(|f| f["type"] == "TOOL_CALL_START")
            .map(|f| f["toolCallName"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["first", "first"],
            "a continuation delta has no name of its own — the id's recorded name \
             must be reused rather than falling back to \"unknown\": {frames:?}"
        );
    }
}
