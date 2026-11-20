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
//! deltas for a call id, and never from `ToolCallItem`, which would put START after
//! the ARGS frames it must precede. Usually the first delta opens the span, with the
//! name it carries (or `"unknown"`). Core gives `name: Some` exactly once per call,
//! but not always on the first delta (`paigasus-helikon-core/src/model.rs`). So when
//! an unnamed delta starts a new occurrence of a reused provider id (see below), the
//! mapper buffers the deltas of that occurrence until one of them carries the name;
//! then START opens the span with that name and the buffered args follow. If no delta
//! carries the name, the item, the result or the run end flushes the span.
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
//!
//! # Call occurrences
//!
//! Provider call ids are unique **at item level** in one model stream (core's
//! accumulator merges deltas by id), but not across a run: Gemini numbers its calls
//! `fc_{n}` per response, so a later turn, or a handoff child, can reuse an id for a
//! different call. The mapper therefore keys its per-call state on the **AG-UI id**
//! of a call occurrence, not on the provider id. A new occurrence of provider id `x`
//! starts only when the events prove that a new call started: any `ToolCallDelta`
//! for `x` (named or not) when the current occurrence already had its item (the
//! item comes after all deltas of a call), a name-carrying `ToolCallDelta` for `x`
//! when the current occurrence already had one (core gives `name: Some` exactly once
//! per call), a `ToolCallItem` for `x` when the current occurrence already had its
//! item, or the first event for `x`. A name that arrives on a later delta belongs to
//! the occurrence that the earlier unnamed delta started. There is no reset at
//! `TurnStarted`: under `ParallelAgent` a sibling's `TurnStarted` can arrive between
//! one call's deltas and its item. The first occurrence of `x` keeps the id `x`; a
//! later one gets `x~n`, with the smallest `n` ≥ 2 that the run has not emitted yet,
//! so an alias never equals a provider id of the run. Every frame of the call
//! (`START`, `ARGS`, `END`, `RESULT`) uses that AG-UI id. A result for a provider id
//! with no occurrence uses the provider id, or the next free `{id}~{n}` when the run
//! already emitted that id, and the run counts the id as used.
//! A `HandoffItem` retires every occurrence that did not have its `ToolCallItem`
//! (core drops the non-transfer calls of a handoff response): the mapper closes or
//! flushes each one as a complete span and removes its occurrence, so the child's
//! next call with that id is a new occurrence. The transfer call keeps its occurrence,
//! so the transfer result uses its AG-UI id. The limit: under `ParallelAgent`, the
//! `HandoffItem` of one branch also retires the unfinished calls of a sibling branch.
//! AG-UI ids are unique per run only: each request gets a new mapper.
//!
//! A `helikon.approval` frame carries two ids. `tool_call_id` is the AG-UI id of the
//! current occurrence: it links the approval to the call's span. `call_id` is the
//! provider id: it matches `RunDeferred.calls[].call_id`, which the client uses to
//! resolve the deferral.

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

/// One occurrence of a provider call id: one tool call of the run.
#[derive(Debug, Clone)]
struct Occurrence {
    /// The AG-UI `toolCallId` of this occurrence: the provider id, or `{id}~{n}`.
    agui_id: String,
    /// Whether a `ToolCallDelta` with `name: Some(..)` was seen for this occurrence.
    named: bool,
    /// Whether the `ToolCallItem` of this occurrence was seen.
    item_seen: bool,
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
    /// Provider call id → its latest occurrence. A provider id can repeat in one run
    /// (see the module doc), so every other per-call field is keyed on the
    /// occurrence's AG-UI id instead.
    current: HashMap<String, Occurrence>,
    /// Every AG-UI id that this run already gave to an occurrence. An alias is never
    /// one of these, so it never equals a provider id that the run emitted.
    used_ids: HashSet<String>,
    /// AG-UI id of the call currently being streamed — has an open `TOOL_CALL_START`
    /// and no `TOOL_CALL_END` yet. `None` if no call is currently open.
    open_call: Option<String>,
    /// Tool name per AG-UI call id, recorded from whichever `ToolCallDelta` first
    /// carries one. Only one delta of a call carries `name`, but the delta that finally
    /// opens that call's span may be a continuation (its buffer resuming, or a
    /// revisit), so the name has to be remembered rather than read off the delta at
    /// hand — otherwise those spans render as `"unknown"`.
    call_names: HashMap<String, String>,
    /// Calls whose deltas arrived while a *different* call was open, or while the
    /// call waits for its name (see `awaiting_name`): `(AG-UI id, accumulated args)`.
    /// AG-UI permits only one active tool-call span at a time, while core's `ToolCallDelta`s for different ids may interleave freely
    /// (`paigasus-helikon-core/src/model.rs:56`) — buffering instead of closing and
    /// resuming preserves every args chunk exactly once and never emits an unmatched
    /// `TOOL_CALL_END`. A `Vec`, not a map, so flush order is deterministic.
    buffered_calls: Vec<(String, String)>,
    /// AG-UI call ids whose span has been emitted *and* closed. A `ToolCallItem`
    /// arrives long after the deltas it describes, by which time the call's span may already
    /// be closed — by its own delta path or by a foreign event that interrupted it.
    /// Without this record the item's arm cannot tell "already rendered" from "never
    /// streamed, synthesize it", and emits a second complete span for one call.
    completed_calls: HashSet<String>,
    /// AG-UI ids of the occurrences of a reused provider id that an unnamed delta
    /// started, and that no delta has named yet. Their deltas are buffered, so that
    /// `TOOL_CALL_START` carries the name that arrives on a later delta.
    awaiting_name: HashSet<String>,
    /// Whether a `STEP_STARTED` is currently unmatched.
    step_open: bool,
}

impl EventMapper {
    /// Create a mapper for one run.
    // Constructed by the AG-UI SSE endpoint (SMA-461 Task 6, src/agui/sse.rs) and the
    // AG-UI WebSocket endpoint (SMA-461 Task 7, src/agui/ws.rs), one per run in both.
    pub(crate) fn new(thread_id: String, run_id: String) -> Self {
        Self {
            thread_id,
            run_id,
            open_text: OpenText::None,
            current_message: String::new(),
            next_message: 0,
            streamed_text: false,
            current: HashMap::new(),
            used_ids: HashSet::new(),
            open_call: None,
            call_names: HashMap::new(),
            buffered_calls: Vec::new(),
            completed_calls: HashSet::new(),
            awaiting_name: HashSet::new(),
            step_open: false,
        }
    }

    /// Map one event, emitting any bracketing frames it implies.
    // Called per event by both the AG-UI SSE endpoint (SMA-461 Task 6) and the AG-UI
    // WebSocket endpoint (SMA-461 Task 7) as each drains an `Agent` run.
    pub(crate) fn push(&mut self, ev: &AgentEvent) -> Vec<Value> {
        let mut out = Vec::new();
        match ev {
            AgentEvent::RunStarted { .. } => {
                out.push(event::run_started(&self.thread_id, &self.run_id));
            }
            AgentEvent::TurnStarted { .. } => {
                self.close_text(&mut out);
                // A tool-call span must not cross the `STEP_FINISHED`/`STEP_STARTED`
                // boundary below — it would close inside the *next* step.
                self.close_call(&mut out);
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
                let call_id = &self.occurrence_for_delta(call_id, name.is_some());
                if let Some(name) = name {
                    self.awaiting_name.remove(call_id);
                    self.call_names
                        .entry(call_id.clone())
                        .or_insert_with(|| name.clone());
                }
                if self.open_call.as_deref() == Some(call_id.as_str()) {
                    // A continuation of the currently-open call.
                    out.push(event::tool_call_args(call_id, args_delta));
                } else if self.open_call.is_none() && !self.awaiting_name.contains(call_id) {
                    // Nothing else is streaming and the call does not wait for its
                    // name: open this call now. If chunks for it were buffered while
                    // another call held the single active span, or while it waited
                    // for its name, replay them first — they were received before this one, and a
                    // client concatenating `TOOL_CALL_ARGS` deltas would otherwise
                    // rebuild the call's arguments out of order.
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
                    // A different call is streaming (AG-UI permits only one active
                    // span), or this new occurrence of a reused id waits for its
                    // name (`TOOL_CALL_START` carries it): buffer this id's content
                    // rather than emitting a START now — it is flushed as a complete span once
                    // its own `ToolCallItem` arrives, when a later delta resumes it
                    // above, or at the latest when the run ends.
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
                    let call_id = &self.occurrence_for_item(call_id);
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
                        self.emit_span(call_id, name, &buffered, &mut out);
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
                        self.emit_span(call_id, name, &args.to_string(), &mut out);
                    }
                } else {
                    out.push(self.custom("helikon.unknown", ev));
                }
            }
            AgentEvent::ToolOutputItem { item } => {
                self.close_call(&mut out);
                self.close_text(&mut out);
                if let Item::ToolResult { call_id, content } = item {
                    // The result belongs to the current occurrence of its provider id:
                    // in a sequential flow a result comes before the next call that
                    // could reuse the id. With no occurrence, the result uses the
                    // provider id, or the next free `{id}~{n}` when the run already
                    // emitted that id (for example as the alias of another call). The
                    // run counts the id as used, so a later call with that provider
                    // id gets an alias.
                    let call_id = match self.current.get(call_id) {
                        Some(occ) => occ.agui_id.clone(),
                        None => {
                            let id = self.unused_id(call_id);
                            self.used_ids.insert(id.clone());
                            id
                        }
                    };
                    if let Some(buffered) = self.take_buffered(&call_id) {
                        // Only possible under `ParallelAgent`: the call is still
                        // buffered. Flush it first, with its name, so its result never
                        // precedes its own span.
                        let name = self.name_of(&call_id).to_owned();
                        self.emit_span(&call_id, &name, &buffered, &mut out);
                    }
                    out.push(event::tool_call_result(&call_id, &text_of(content)));
                } else {
                    out.push(self.custom("helikon.unknown", ev));
                }
            }
            AgentEvent::MessageOutput { item } => {
                // Take the single active span before emitting anything: a synthesized
                // text triple nested inside an open `TOOL_CALL_START` would break
                // AG-UI's one-active-span rule.
                self.close_call(&mut out);
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
            AgentEvent::HandoffItem { .. } => {
                self.retire_unfinished_calls(&mut out);
                out.push(self.custom("helikon.handoff", ev));
            }
            AgentEvent::AgentUpdated { .. } => out.push(self.custom("helikon.agent_updated", ev)),
            AgentEvent::GuardrailTriggered { .. } => out.push(self.custom("helikon.guardrail", ev)),
            AgentEvent::ApprovalRequested { call_id, .. } => {
                // Keep `call_id` as the provider id (it matches
                // `RunDeferred.calls[].call_id`). Add `tool_call_id`, the AG-UI id
                // of the current occurrence, so a client can connect the approval
                // to the call's span.
                let mut frame = self.custom("helikon.approval", ev);
                if let Some(value) = frame["value"].as_object_mut() {
                    value.insert(
                        "tool_call_id".to_owned(),
                        Value::String(self.agui_id_of(call_id)),
                    );
                }
                out.push(frame);
            }
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
            AgentEvent::UserMessageInjected { .. } => {
                out.push(self.custom("helikon.user_message_injected", ev));
            }
            // AG-UI has no "stopped" run event. A stop is a controlled end, not a
            // failure, so the stream ends with `RUN_FINISHED`; the CUSTOM frame before
            // it carries the reason and usage.
            AgentEvent::RunStopped { .. } => {
                self.close_all(&mut out);
                self.streamed_text = false;
                out.push(self.custom("helikon.run_stopped", ev));
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
    // Called by both the AG-UI SSE endpoint (SMA-461 Task 6) and the AG-UI WebSocket
    // endpoint (SMA-461 Task 7) once their channel from the underlying `Agent` stream
    // closes, in case that stream ended without a terminal event (e.g. a misbehaving
    // agent, or a connection dropped mid-run).
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

    /// The AG-UI id for a `ToolCallDelta` of provider id `call_id`. Any delta, named
    /// or not, starts a new occurrence when the current one already had its
    /// `ToolCallItem` (the item comes after all deltas of a call), or when `call_id`
    /// has no occurrence yet. A name-carrying delta (`has_name`) also starts one when
    /// the current occurrence already had a name. An unnamed delta before the item
    /// stays on the current occurrence, and a name that arrives on a later delta
    /// attaches to the occurrence that the first delta started. When an unnamed
    /// delta starts a new occurrence that gets an alias (a reused id: the current
    /// occurrence already had its item, or the run used the id before, for example
    /// for an occurrence that a handoff retired), the new occurrence waits for its
    /// name (`awaiting_name`).
    fn occurrence_for_delta(&mut self, call_id: &str, has_name: bool) -> String {
        if let Some(occ) = self.current.get_mut(call_id) {
            if !(occ.item_seen || (has_name && occ.named)) {
                occ.named |= has_name;
                return occ.agui_id.clone();
            }
        }
        let agui_id = self.new_occurrence(call_id);
        if let Some(occ) = self.current.get_mut(call_id) {
            occ.named = has_name;
        }
        if agui_id != call_id && !has_name {
            self.awaiting_name.insert(agui_id.clone());
        }
        agui_id
    }

    /// The AG-UI id for a `ToolCallItem` of provider id `call_id`. The item starts a
    /// new occurrence when the current one already had its item (a non-streamed call
    /// that reuses the id), or when `call_id` has no occurrence yet.
    fn occurrence_for_item(&mut self, call_id: &str) -> String {
        if let Some(occ) = self.current.get_mut(call_id) {
            if !occ.item_seen {
                occ.item_seen = true;
                return occ.agui_id.clone();
            }
        }
        let agui_id = self.new_occurrence(call_id);
        if let Some(occ) = self.current.get_mut(call_id) {
            occ.item_seen = true;
        }
        agui_id
    }

    /// Start a new occurrence of provider id `call_id` and return its AG-UI id:
    /// `call_id` itself when the run has not used it, else `{call_id}~{n}` with the
    /// smallest `n` ≥ 2 that the run has not used.
    fn new_occurrence(&mut self, call_id: &str) -> String {
        let agui_id = self.unused_id(call_id);
        self.used_ids.insert(agui_id.clone());
        self.current.insert(
            call_id.to_owned(),
            Occurrence {
                agui_id: agui_id.clone(),
                named: false,
                item_seen: false,
            },
        );
        agui_id
    }

    /// The first AG-UI id for provider id `call_id` that the run has not used:
    /// `call_id` itself, else `{call_id}~{n}` with the smallest `n` ≥ 2 that is not
    /// used. It does not mark the id as used.
    fn unused_id(&self, call_id: &str) -> String {
        if self.used_ids.contains(call_id) {
            (2u64..)
                .map(|n| format!("{call_id}~{n}"))
                .find(|alias| !self.used_ids.contains(alias))
                .expect("an unbounded range always yields an unused alias")
        } else {
            call_id.to_owned()
        }
    }

    /// Retire every occurrence that did not have its `ToolCallItem` when a
    /// `HandoffItem` arrives. Core drops the non-transfer calls of a handoff
    /// response, so no item comes for them. Each one that is still open or buffered
    /// is closed or flushed as a complete span, and its provider id loses its
    /// occurrence, so the child's next call with that id starts a new occurrence.
    /// The ids stay in `used_ids`. Occurrences that had their item (the transfer
    /// call) stay, so the transfer result still finds its AG-UI id.
    fn retire_unfinished_calls(&mut self, out: &mut Vec<Value>) {
        let mut retired: HashSet<String> = HashSet::new();
        self.current.retain(|_, occ| {
            if occ.item_seen {
                true
            } else {
                retired.insert(occ.agui_id.clone());
                false
            }
        });
        if retired.is_empty() {
            return;
        }
        for id in &retired {
            self.awaiting_name.remove(id);
        }
        let open_retired = self
            .open_call
            .as_ref()
            .is_some_and(|id| retired.contains(id));
        let buffered_retired = self
            .buffered_calls
            .iter()
            .any(|(id, _)| retired.contains(id));
        if !(open_retired || buffered_retired) {
            return;
        }
        self.close_text(out);
        self.close_call(out);
        let (flush, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.buffered_calls)
            .into_iter()
            .partition(|(id, _)| retired.contains(id));
        self.buffered_calls = keep;
        for (id, args) in flush {
            let name = self.name_of(&id).to_owned();
            self.emit_span(&id, &name, &args, out);
        }
    }

    /// The AG-UI id of the current occurrence of provider id `call_id`, or `call_id`
    /// itself when it has none (a result for a call this mapper never saw).
    fn agui_id_of(&self, call_id: &str) -> String {
        self.current
            .get(call_id)
            .map_or_else(|| call_id.to_owned(), |occ| occ.agui_id.clone())
    }

    /// Emit one complete, self-contained span for AG-UI id `call_id` and record it
    /// as closed.
    fn emit_span(&mut self, call_id: &str, name: &str, args: &str, out: &mut Vec<Value>) {
        out.push(event::tool_call_start(call_id, name, &self.current_message));
        out.push(event::tool_call_args(call_id, args));
        out.push(event::tool_call_end(call_id));
        self.completed_calls.insert(call_id.to_owned());
    }

    /// The tool name recorded for AG-UI id `call_id`, falling back to `"unknown"` only
    /// when no delta for it ever carried one.
    fn name_of(&self, call_id: &str) -> &str {
        self.call_names
            .get(call_id)
            .map_or("unknown", String::as_str)
    }

    /// Remove and return the args buffered for AG-UI id `call_id` while a different call
    /// held the active span, if any.
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
            let name = self.name_of(&id).to_owned();
            self.emit_span(&id, &name, &args, out);
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
    /// Regression (CodeRabbit, PR #186): a `MessageOutput` synthesizing its own text
    /// triple must not nest that triple inside a still-open tool call. Reachable today
    /// with a `ToolCallDelta` followed by a `MessageOutput` that streamed no deltas.
    #[test]
    fn a_synthesized_message_does_not_nest_inside_an_open_tool_call() {
        let mut m = EventMapper::new("t".to_owned(), "r".to_owned());
        let mut frames = Vec::new();
        frames.extend(m.push(&AgentEvent::ToolCallDelta {
            call_id: "tc1".to_owned(),
            name: Some("search".to_owned()),
            args_delta: "{}".to_owned(),
        }));
        frames.extend(m.push(&AgentEvent::MessageOutput {
            item: Item::AssistantMessage {
                content: vec![ContentPart::Text {
                    text: "done".to_owned(),
                }],
                agent: None,
            },
        }));
        frames.extend(m.finish());
        assert_tool_call_nesting(&frames);
    }

    /// Regression (CodeRabbit, PR #186): a tool-call span must not cross a
    /// `STEP_FINISHED`/`STEP_STARTED` boundary.
    #[test]
    fn a_tool_call_span_does_not_cross_a_turn_boundary() {
        let mut m = EventMapper::new("t".to_owned(), "r".to_owned());
        let mut frames = Vec::new();
        frames.extend(m.push(&AgentEvent::TurnStarted { turn: 1 }));
        frames.extend(m.push(&AgentEvent::ToolCallDelta {
            call_id: "tc1".to_owned(),
            name: Some("search".to_owned()),
            args_delta: "{}".to_owned(),
        }));
        frames.extend(m.push(&AgentEvent::TurnStarted { turn: 2 }));
        frames.extend(m.finish());
        assert_tool_call_nesting(&frames);
    }

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
                // Nothing else may appear inside a call's span. Checking only the
                // `TOOL_CALL_*` frames would let a text triple or a step boundary nest
                // inside an open call and still pass — which is exactly how two
                // missing `close_call`s once went unnoticed here.
                other => {
                    assert!(
                        open.is_none(),
                        "{other} emitted while tool call {open:?} is still open: {frames:?}"
                    );
                }
            }
        }
        assert!(open.is_none(), "a tool call was left open: {frames:?}");
    }

    /// Assert every tool-call invariant the mapper owes an AG-UI client, for the
    /// `frames` produced by `events`:
    ///
    /// 1. spans nest and never overlap ([`assert_tool_call_nesting`]);
    /// 2. each AG-UI call id opens **at most once** across the whole sequence — a
    ///    nesting check cannot see a duplicate `START`, yet one call split across two
    ///    spans is exactly what breaks a client that keys state on `toolCallId`;
    /// 3. each call occurrence's args are emitted **in receipt order and in full** —
    ///    concatenating the `TOOL_CALL_ARGS` deltas the mapper emitted for an AG-UI id
    ///    must reproduce the concatenation of that occurrence's `ToolCallDelta`
    ///    chunks in the order they arrived. Reordering or dropping a chunk yields
    ///    malformed JSON on the client, and (1) and (2) are both blind to it;
    /// 4. each `TOOL_CALL_RESULT` uses the AG-UI id of the call that its
    ///    `ToolOutputItem` closes.
    ///
    /// A provider id can repeat in one run (Gemini numbers its calls `fc_{n}` per
    /// response). The oracle finds the occurrences from the fixture's own structure,
    /// not from the mapper's rule: it cuts the events at each `TurnStarted` and each
    /// `HandoffItem`, and each provider id seen (as a delta or an item) in one cut is
    /// one occurrence. Occurrence 1 of `x` is `x`, a later one is `x~n` with the
    /// smallest `n` ≥ 2 that no earlier occurrence took. This holds for the
    /// sequential fixtures only: a `ParallelAgent` fixture, where a sibling's
    /// `TurnStarted` cuts one call in two, must use [`assert_tool_call_nesting`].
    ///
    /// Occurrences with no `ToolCallDelta` at all (a non-streaming provider's
    /// `ToolCallItem`) carry no receipt order to check, so (3) skips them.
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

        /// The AG-UI id of the occurrence of `call_id` in cut `cut`, allocating it
        /// on the occurrence's first event.
        fn occurrence(
            call_id: &str,
            cut: usize,
            seen: &mut HashMap<(String, usize), String>,
            latest: &mut HashMap<String, String>,
            taken: &mut HashSet<String>,
        ) -> String {
            let key = (call_id.to_owned(), cut);
            if let Some(id) = seen.get(&key) {
                return id.clone();
            }
            let id = if taken.contains(call_id) {
                (2..)
                    .map(|n| format!("{call_id}~{n}"))
                    .find(|alias| !taken.contains(alias))
                    .unwrap()
            } else {
                call_id.to_owned()
            };
            taken.insert(id.clone());
            seen.insert(key, id.clone());
            latest.insert(call_id.to_owned(), id.clone());
            id
        }

        let mut cut = 0usize;
        let mut seen: HashMap<(String, usize), String> = HashMap::new();
        let mut latest: HashMap<String, String> = HashMap::new();
        let mut taken: HashSet<String> = HashSet::new();
        let mut received: BTreeMap<String, String> = BTreeMap::new();
        let mut want_results: Vec<String> = Vec::new();
        for e in events {
            match e {
                AgentEvent::TurnStarted { .. } | AgentEvent::HandoffItem { .. } => cut += 1,
                AgentEvent::ToolCallDelta {
                    call_id,
                    args_delta,
                    ..
                } => {
                    let id = occurrence(call_id, cut, &mut seen, &mut latest, &mut taken);
                    received.entry(id).or_default().push_str(args_delta);
                }
                AgentEvent::ToolCallItem {
                    item: Item::ToolCall { call_id, .. },
                } => {
                    occurrence(call_id, cut, &mut seen, &mut latest, &mut taken);
                }
                AgentEvent::ToolOutputItem {
                    item: Item::ToolResult { call_id, .. },
                } => {
                    // A result with no occurrence uses the provider id, or the next
                    // free `{id}~{n}` when that id is already taken, and the mapper
                    // counts the id as used, as the oracle does here.
                    let id = latest.get(call_id).cloned().unwrap_or_else(|| {
                        let id = if taken.contains(call_id) {
                            (2..)
                                .map(|n| format!("{call_id}~{n}"))
                                .find(|alias| !taken.contains(alias))
                                .unwrap()
                        } else {
                            call_id.clone()
                        };
                        taken.insert(id.clone());
                        id
                    });
                    want_results.push(id);
                }
                _ => {}
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
            let got = emitted.get(id.as_str()).map_or("", String::as_str);
            assert_eq!(
                got, want,
                "args for {id:?} must be emitted in receipt order and in full: {frames:?}"
            );
        }
        let got_results: Vec<&str> = frames
            .iter()
            .filter(|f| f["type"] == "TOOL_CALL_RESULT")
            .map(|f| f["toolCallId"].as_str().unwrap())
            .collect();
        assert_eq!(
            got_results, want_results,
            "each TOOL_CALL_RESULT must use the AG-UI id of the call it closes: {frames:?}"
        );
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
    fn user_message_injected_maps_to_a_custom_frame() {
        let mut m = EventMapper::new("t".to_owned(), "r".to_owned());
        let frames = m.push(&AgentEvent::UserMessageInjected {
            content: vec![ContentPart::Text {
                text: "hi".to_owned(),
            }],
        });
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["type"], "CUSTOM");
        assert_eq!(frames[0]["name"], "helikon.user_message_injected");
    }

    #[test]
    fn run_stopped_maps_to_a_custom_frame_then_run_finished() {
        let t = types(&[
            AgentEvent::RunStarted {
                agent: "a".to_owned(),
            },
            AgentEvent::RunStopped {
                reason: "enough".to_owned(),
                usage: TokenUsage::default(),
            },
        ]);
        assert_eq!(t, vec!["RUN_STARTED", "CUSTOM", "RUN_FINISHED"]);
        let mut m = EventMapper::new("t".to_owned(), "r".to_owned());
        let frames = m.push(&AgentEvent::RunStopped {
            reason: "enough".to_owned(),
            usage: TokenUsage::default(),
        });
        assert_eq!(frames[0]["name"], "helikon.run_stopped");
        assert_eq!(frames[0]["value"]["reason"], "enough");
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

    fn tc_result(call_id: &str, text: &str) -> AgentEvent {
        AgentEvent::ToolOutputItem {
            item: Item::ToolResult {
                call_id: call_id.to_owned(),
                content: vec![ContentPart::Text {
                    text: text.to_owned(),
                }],
            },
        }
    }

    fn turn(n: u32) -> AgentEvent {
        AgentEvent::TurnStarted { turn: n }
    }

    fn completed() -> AgentEvent {
        AgentEvent::RunCompleted {
            usage: TokenUsage::default(),
        }
    }

    /// `(toolCallId, toolCallName)` of every `TOOL_CALL_START`, in frame order.
    fn starts(frames: &[Value]) -> Vec<(String, String)> {
        frames
            .iter()
            .filter(|f| f["type"] == "TOOL_CALL_START")
            .map(|f| {
                (
                    f["toolCallId"].as_str().unwrap().to_owned(),
                    f["toolCallName"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// The `toolCallId` of every `TOOL_CALL_RESULT`, in frame order.
    fn result_ids(frames: &[Value]) -> Vec<&str> {
        frames
            .iter()
            .filter(|f| f["type"] == "TOOL_CALL_RESULT")
            .map(|f| f["toolCallId"].as_str().unwrap())
            .collect()
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
            .collect()
    }

    /// SMA-810 (T14): Gemini numbers its calls `fc_{n}` per response, so turn 2 can
    /// reuse turn 1's id for a different call. The reuse must get its own AG-UI id,
    /// its own name and its own complete span, and its result must use that id.
    #[test]
    fn a_reused_call_id_in_a_later_turn_gets_its_own_span() {
        let streamed = vec![
            turn(1),
            tc_delta("fc_0", Some("a"), "{\"x\":1}"),
            tc_item("fc_0", "a"),
            tc_result("fc_0", "ra"),
            turn(2),
            tc_delta("fc_0", Some("b"), "{\"y\":2}"),
            tc_item("fc_0", "b"),
            tc_result("fc_0", "rb"),
            completed(),
        ];
        let not_streamed = vec![
            turn(1),
            tc_item("fc_0", "a"),
            tc_result("fc_0", "ra"),
            turn(2),
            tc_item("fc_0", "b"),
            tc_result("fc_0", "rb"),
            completed(),
        ];
        for events in [streamed, not_streamed] {
            let frames = frames_of(&events);
            assert_tool_calls_well_formed(&events, &frames);
            assert_eq!(
                starts(&frames),
                pairs(&[("fc_0", "a"), ("fc_0~2", "b")]),
                "the second use of fc_0 is a new call with its own id and name: {frames:?}"
            );
            assert_eq!(result_ids(&frames), vec!["fc_0", "fc_0~2"], "{frames:?}");
        }
    }

    /// The handoff fixture of T14: the parent's turn 1 uses `fc_1` for a normal call;
    /// the handoff response is `[search fc_0, transfer fc_1]` (`ordered` gives the
    /// order of their deltas), and only the transfer gets a `ToolCallItem`. The child
    /// response is `[lookup fc_0, pay fc_1]`.
    fn handoff_fixture(transfer_first: bool) -> Vec<AgentEvent> {
        let search = tc_delta("fc_0", Some("search"), "{\"q\":\"s\"}");
        let transfer = tc_delta("fc_1", Some("transfer_to_billing"), "{}");
        let (first, second) = if transfer_first {
            (transfer, search)
        } else {
            (search, transfer)
        };
        vec![
            turn(1),
            tc_delta("fc_1", Some("balance"), "{\"acct\":1}"),
            tc_item("fc_1", "balance"),
            tc_result("fc_1", "42"),
            turn(2),
            first,
            second,
            tc_item("fc_1", "transfer_to_billing"),
            AgentEvent::HandoffItem {
                from: "triage".to_owned(),
                to: "billing".to_owned(),
            },
            tc_result("fc_1", "transferred to billing"),
            AgentEvent::AgentUpdated {
                agent: "billing".to_owned(),
            },
            turn(1),
            tc_delta("fc_0", Some("lookup"), "{\"id\":7}"),
            tc_delta("fc_1", Some("pay"), "{\"amt\":3}"),
            tc_item("fc_0", "lookup"),
            tc_item("fc_1", "pay"),
            tc_result("fc_0", "found"),
            tc_result("fc_1", "paid"),
            completed(),
        ]
    }

    /// SMA-810 (T14): a handoff child that reuses the parent's provider ids renders
    /// each of its calls with its own name and a fresh AG-UI id, and the transfer
    /// result uses the AG-UI id of the transfer call. No args merge across calls.
    #[test]
    fn a_handoff_child_reusing_ids_renders_its_own_calls() {
        let events = handoff_fixture(false);
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        assert_eq!(
            starts(&frames),
            pairs(&[
                ("fc_1", "balance"),
                ("fc_0", "search"),
                ("fc_1~2", "transfer_to_billing"),
                ("fc_0~2", "lookup"),
                ("fc_1~3", "pay"),
            ]),
            "{frames:?}"
        );
        assert_eq!(
            result_ids(&frames),
            vec!["fc_1", "fc_1~2", "fc_0~2", "fc_1~3"],
            "{frames:?}"
        );
    }

    /// SMA-810 (T14): the same handoff with the transfer delta first, so `search`
    /// stays buffered until the run ends.
    #[test]
    fn a_handoff_child_reusing_ids_renders_its_own_calls_when_search_is_buffered() {
        let events = handoff_fixture(true);
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        let mut got = starts(&frames);
        got.sort();
        assert_eq!(
            got,
            pairs(&[
                ("fc_0", "search"),
                ("fc_0~2", "lookup"),
                ("fc_1", "balance"),
                ("fc_1~2", "transfer_to_billing"),
                ("fc_1~3", "pay"),
            ]),
            "{frames:?}"
        );
        assert_eq!(
            result_ids(&frames),
            vec!["fc_1", "fc_1~2", "fc_0~2", "fc_1~3"],
            "{frames:?}"
        );
    }

    /// SMA-810 (T14): under `ParallelAgent`, branch B's `TurnStarted` can arrive
    /// between branch A's deltas and A's `ToolCallItem`. With unique provider ids no
    /// occurrence rule fires again, so the frames are exactly the frames of the
    /// mapper before SMA-810 (pinned below from a run of that mapper). There is no
    /// reset at `TurnStarted`, so the name stays and no alias appears.
    #[test]
    fn parallel_branches_with_unique_ids_render_as_today() {
        // Case 1: all of A's deltas arrive before B's `TurnStarted`. One span for
        // `call_a`, with its name, and no second span.
        let complete_before = vec![
            turn(1),
            tc_delta("call_a", Some("lookup"), "{\"q\":"),
            tc_delta("call_a", None, "1}"),
            turn(1),
            tc_item("call_a", "lookup"),
            tc_result("call_a", "ok"),
            completed(),
        ];
        // Case 2: more A deltas arrive after B's `TurnStarted`. The turn boundary
        // closed A's span, so the revisit is a second span of the same id (as in
        // `a_delta_revisiting_a_closed_id_keeps_its_args_and_its_name`), and it
        // keeps the name.
        let continued_after = vec![
            turn(1),
            tc_delta("call_a", Some("lookup"), "{\"q\":"),
            turn(1),
            tc_delta("call_a", None, "1}"),
            tc_item("call_a", "lookup"),
            tc_result("call_a", "ok"),
            completed(),
        ];
        // Frames of the mapper before SMA-810 for each case, recorded from a run of
        // that mapper on these exact inputs.
        let pinned_complete_before = r#"[{"stepName":"turn","type":"STEP_STARTED"},{"toolCallId":"call_a","toolCallName":"lookup","type":"TOOL_CALL_START"},{"delta":"{\"q\":","toolCallId":"call_a","type":"TOOL_CALL_ARGS"},{"delta":"1}","toolCallId":"call_a","type":"TOOL_CALL_ARGS"},{"toolCallId":"call_a","type":"TOOL_CALL_END"},{"stepName":"turn","type":"STEP_FINISHED"},{"stepName":"turn","type":"STEP_STARTED"},{"content":"ok","toolCallId":"call_a","type":"TOOL_CALL_RESULT"},{"stepName":"turn","type":"STEP_FINISHED"},{"runId":"r1","threadId":"t1","type":"RUN_FINISHED"}]"#;
        let pinned_continued_after = r#"[{"stepName":"turn","type":"STEP_STARTED"},{"toolCallId":"call_a","toolCallName":"lookup","type":"TOOL_CALL_START"},{"delta":"{\"q\":","toolCallId":"call_a","type":"TOOL_CALL_ARGS"},{"toolCallId":"call_a","type":"TOOL_CALL_END"},{"stepName":"turn","type":"STEP_FINISHED"},{"stepName":"turn","type":"STEP_STARTED"},{"toolCallId":"call_a","toolCallName":"lookup","type":"TOOL_CALL_START"},{"delta":"1}","toolCallId":"call_a","type":"TOOL_CALL_ARGS"},{"toolCallId":"call_a","type":"TOOL_CALL_END"},{"content":"ok","toolCallId":"call_a","type":"TOOL_CALL_RESULT"},{"stepName":"turn","type":"STEP_FINISHED"},{"runId":"r1","threadId":"t1","type":"RUN_FINISHED"}]"#;
        for (events, pinned) in [
            (complete_before, pinned_complete_before),
            (continued_after, pinned_continued_after),
        ] {
            let frames = frames_of(&events);
            assert_tool_call_nesting(&frames);
            let want: Vec<Value> = serde_json::from_str(pinned).unwrap();
            assert_eq!(frames, want, "the frames must not change: {frames:?}");
            assert!(
                starts(&frames)
                    .iter()
                    .all(|(id, name)| id == "call_a" && name == "lookup"),
                "no alias and no \"unknown\" name: {frames:?}"
            );
        }
        let one_span = frames_of(&[
            turn(1),
            tc_delta("call_a", Some("lookup"), "{}"),
            turn(1),
            tc_item("call_a", "lookup"),
        ]);
        assert_eq!(
            starts(&one_span).len(),
            1,
            "the item after the sibling's turn adds no second span: {one_span:?}"
        );
    }

    /// SMA-810 (G5): an alias never equals a provider id that the run already
    /// emitted. The run uses `x~2` as a real provider id, then reuses `x`.
    #[test]
    fn an_alias_skips_an_emitted_provider_id() {
        let events = vec![
            turn(1),
            tc_delta("x~2", Some("real"), "{}"),
            tc_item("x~2", "real"),
            tc_result("x~2", "r0"),
            turn(2),
            tc_delta("x", Some("first"), "{\"n\":1}"),
            tc_item("x", "first"),
            tc_result("x", "r1"),
            turn(3),
            tc_delta("x", Some("second"), "{\"n\":2}"),
            tc_item("x", "second"),
            tc_result("x", "r2"),
            completed(),
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        assert_eq!(
            starts(&frames),
            pairs(&[("x~2", "real"), ("x", "first"), ("x~3", "second")]),
            "{frames:?}"
        );
        assert_eq!(result_ids(&frames), vec!["x~2", "x", "x~3"], "{frames:?}");
    }

    /// SMA-810 (G5): a `helikon.approval` frame for a reused id carries the AG-UI id
    /// of the current occurrence in `tool_call_id`, so a client can connect it to the
    /// span. `call_id` stays the provider id, which matches `RunDeferred`.
    #[test]
    fn approval_frame_uses_the_current_alias() {
        let events = vec![
            turn(1),
            tc_delta("fc_0", Some("a"), "{}"),
            tc_item("fc_0", "a"),
            tc_result("fc_0", "ra"),
            turn(2),
            tc_delta("fc_0", Some("pay"), "{\"amt\":3}"),
            tc_item("fc_0", "pay"),
            AgentEvent::ApprovalRequested {
                call_id: "fc_0".to_owned(),
                tool: "pay".to_owned(),
                args: serde_json::json!({"amt": 3}),
            },
            completed(),
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        let approval = frames
            .iter()
            .find(|f| f["name"] == "helikon.approval")
            .unwrap();
        assert_eq!(approval["value"]["tool_call_id"], "fc_0~2", "{frames:?}");
        assert_eq!(approval["value"]["call_id"], "fc_0", "{frames:?}");
        assert_eq!(approval["value"]["tool"], "pay", "{frames:?}");
    }

    /// SMA-810: a result for a provider id with no occurrence uses the provider id.
    /// The run counts that id as used, so a later streamed call with the same id gets
    /// an alias, and the client sees two different ids.
    #[test]
    fn a_result_with_no_occurrence_reserves_its_id() {
        let events = vec![
            turn(1),
            tc_result("fc_0", "orphan"),
            turn(2),
            tc_delta("fc_0", Some("lookup"), "{}"),
            tc_item("fc_0", "lookup"),
            tc_result("fc_0", "found"),
            completed(),
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        assert_eq!(
            starts(&frames),
            pairs(&[("fc_0~2", "lookup")]),
            "{frames:?}"
        );
        assert_eq!(result_ids(&frames), vec!["fc_0", "fc_0~2"], "{frames:?}");
    }

    /// SMA-810 (G5): a result with no occurrence whose provider id the run already
    /// emitted (here as the alias `x~2` of another call) gets a new AG-UI id by the
    /// same smallest-unused rule, so it does not attach to that other call.
    #[test]
    fn a_result_with_no_occurrence_never_reuses_an_emitted_id() {
        let events = vec![
            turn(1),
            tc_delta("x", Some("a"), "{}"),
            tc_item("x", "a"),
            tc_result("x", "ra"),
            turn(2),
            tc_delta("x", Some("b"), "{}"),
            tc_item("x", "b"),
            tc_result("x", "rb"),
            turn(3),
            tc_result("x~2", "orphan"),
            completed(),
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        assert_eq!(
            starts(&frames),
            pairs(&[("x", "a"), ("x~2", "b")]),
            "{frames:?}"
        );
        assert_eq!(result_ids(&frames), vec!["x", "x~2", "x~2~2"], "{frames:?}");
    }

    /// SMA-810 (G5): core drops the non-transfer calls of a handoff response, so
    /// `search fc_0` never gets a `ToolCallItem`. The `HandoffItem` retires it: its
    /// span is complete, and the child's `lookup fc_0` is a new occurrence. The
    /// child's first delta has no name, so the span waits for it and opens as
    /// `lookup` with all of its args. The transfer result uses the transfer's id.
    #[test]
    fn a_handoff_retires_the_unfinished_calls_of_the_handoff_response() {
        let events = vec![
            turn(1),
            tc_delta("fc_0", Some("search"), "{\"q\":\"s\"}"),
            tc_delta("fc_1", Some("transfer_to_billing"), "{}"),
            tc_item("fc_1", "transfer_to_billing"),
            AgentEvent::HandoffItem {
                from: "triage".to_owned(),
                to: "billing".to_owned(),
            },
            tc_result("fc_1", "transferred to billing"),
            AgentEvent::AgentUpdated {
                agent: "billing".to_owned(),
            },
            turn(1),
            tc_delta("fc_0", None, "{\"id\":"),
            tc_delta("fc_0", Some("lookup"), "7}"),
            tc_item("fc_0", "lookup"),
            tc_result("fc_0", "found"),
            completed(),
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        assert_eq!(
            starts(&frames),
            pairs(&[
                ("fc_0", "search"),
                ("fc_1", "transfer_to_billing"),
                ("fc_0~2", "lookup"),
            ]),
            "{frames:?}"
        );
        let args_of = |id: &str| -> String {
            frames
                .iter()
                .filter(|f| f["type"] == "TOOL_CALL_ARGS" && f["toolCallId"] == id)
                .map(|f| f["delta"].as_str().unwrap())
                .collect()
        };
        assert_eq!(args_of("fc_0"), "{\"q\":\"s\"}", "{frames:?}");
        assert_eq!(args_of("fc_0~2"), "{\"id\":7}", "{frames:?}");
        assert_eq!(result_ids(&frames), vec!["fc_1", "fc_0~2"], "{frames:?}");
    }

    /// SMA-810 (G5): a result for a call that is still buffered (only possible under
    /// `ParallelAgent`) flushes that call first, with its name, so the result never
    /// precedes its own span.
    #[test]
    fn a_result_for_a_buffered_call_flushes_it_with_its_name() {
        let events = vec![
            tc_delta("a", Some("first"), "1"),
            tc_delta("b", Some("second"), "2"),
            tc_result("b", "done"),
            completed(),
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
                "TOOL_CALL_RESULT",
                "RUN_FINISHED",
            ],
            "{frames:?}"
        );
        assert_eq!(
            starts(&frames),
            pairs(&[("a", "first"), ("b", "second")]),
            "{frames:?}"
        );
    }

    /// SMA-810 (G5): an item-only call `x` (a non-streaming provider), then a
    /// streamed call that reuses `x` (for example a handoff to an agent on a
    /// streaming provider). The name-carrying delta after the item proves a new call,
    /// so the second call gets its own AG-UI id, name and span.
    #[test]
    fn a_streamed_reuse_of_an_item_only_call_id_gets_its_own_span() {
        let events = vec![
            turn(1),
            tc_item("x", "a"),
            tc_result("x", "ra"),
            turn(2),
            tc_delta("x", Some("b"), "{\"k\":1}"),
            tc_item("x", "b"),
            tc_result("x", "rb"),
            completed(),
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        assert_eq!(
            starts(&frames),
            pairs(&[("x", "a"), ("x~2", "b")]),
            "{frames:?}"
        );
        assert_eq!(result_ids(&frames), vec!["x", "x~2"], "{frames:?}");
    }

    /// A first-use call whose first delta has no name streams live, as before
    /// SMA-810: the first delta opens the span with `"unknown"`, and the later name
    /// does not change the START that the client already has.
    #[test]
    fn a_first_use_call_with_an_unnamed_first_delta_streams_live() {
        let events = vec![
            turn(1),
            tc_delta("y", None, "{\"q\":"),
            tc_delta("y", Some("b"), "1}"),
            tc_item("y", "b"),
            completed(),
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        assert_eq!(
            frames,
            vec![
                event::step_started("turn"),
                event::tool_call_start("y", "unknown", ""),
                event::tool_call_args("y", "{\"q\":"),
                event::tool_call_args("y", "1}"),
                event::tool_call_end("y"),
                event::step_finished("turn"),
                event::run_finished("t1", "r1"),
            ],
        );
    }

    /// SMA-810 (G5): core lets `name: Some` arrive on a later delta than the first
    /// args chunk (`paigasus-helikon-core/src/model.rs`). After the item of the first
    /// `x`, an unnamed delta for `x` already starts the new occurrence, and the name
    /// that arrives later belongs to that occurrence.
    #[test]
    fn an_unnamed_first_delta_after_the_item_starts_a_new_occurrence() {
        let events = vec![
            turn(1),
            tc_delta("x", Some("a"), "{}"),
            tc_item("x", "a"),
            tc_result("x", "ra"),
            turn(2),
            tc_delta("x", None, "{\"q\":"),
            tc_delta("x", Some("b"), "1}"),
            tc_item("x", "b"),
            tc_result("x", "rb"),
            completed(),
        ];
        let frames = frames_of(&events);
        assert_tool_calls_well_formed(&events, &frames);
        assert_eq!(
            starts(&frames),
            pairs(&[("x", "a"), ("x~2", "b")]),
            "{frames:?}"
        );
        let args: String = frames
            .iter()
            .filter(|f| f["type"] == "TOOL_CALL_ARGS" && f["toolCallId"] == "x~2")
            .map(|f| f["delta"].as_str().unwrap())
            .collect();
        assert_eq!(args, "{\"q\":1}", "{frames:?}");
        assert_eq!(result_ids(&frames), vec!["x", "x~2"], "{frames:?}");
    }
}
