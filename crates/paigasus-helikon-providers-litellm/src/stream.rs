//! SSE chunk → [`ModelEvent`] translation.
//!
//! Two invariants carry this module:
//!
//! 1. **`Finish` is emitted only from [`ChatTranslator::finish`]**, called at
//!    `[DONE]`/EOF — never inline with a chunk. With
//!    `stream_options.include_usage`, the usage snapshot arrives in a chunk
//!    *after* the one carrying `finish_reason`, so an inline `Finish` would be
//!    followed by `Usage` and violate core's "Finish is the terminal event"
//!    contract on every turn.
//! 2. **Tool-call `name`/`arguments` are buffered until the `id` is known,
//!    and `name` keeps buffering past that point**, because the id is not
//!    guaranteed to arrive first, and both fields fragment across deltas.
//!    Once the id is known, `arguments` drains on every delta, but `name`
//!    keeps accumulating until a completion signal (a delta carrying
//!    arguments, or one carrying no name fragment) or the end-of-stream
//!    flush releases it. A delta that carries neither `index` nor `id`
//!    is correlated by its position within `delta.tool_calls`, but only
//!    when *no* entry in that same array carries an explicit `index` — a
//!    synthesized positional key must never be allowed to collide with a
//!    genuine explicit index elsewhere in the array, which would silently
//!    merge two distinct calls. A mixed array (some entries indexed, one
//!    not) is non-conforming for OpenAI-compatible streaming; the
//!    ambiguous entry is skipped with a loud warning rather than guessed.

use std::collections::{HashMap, HashSet};

use paigasus_helikon_core::{FinishReason, ModelEvent};

use crate::sse::{StreamChunk, ToolCallChunk};

/// Correlation key for a streaming tool call.
// `PartialOrd`/`Ord`: `flush_buffered_names` sorts keys for a deterministic
// end-of-stream flush order, and `Key::Index` sorting before `Key::Id` is
// what makes the dual-key winner predictable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Key {
    /// Correlated by `delta.tool_calls[].index`, when present — or, as a
    /// last resort, by position within `delta.tool_calls` (in OpenAI-
    /// compatible streaming the array position *is* the index, so a
    /// positional continuation delta correctly joins a call already keyed
    /// by `Index`).
    Index(u32),
    /// Correlated by `delta.tool_calls[].id`, when `index` is absent.
    Id(String),
}

/// Buffered name and args fragments for a tool call.
///
/// Both fields start out buffered here regardless of whether the call's
/// `id` is known yet. Once the id is known, `args` is drain-once — taken on
/// the first delta after the id is observed and never re-prepended — while
/// `name` keeps accumulating across every delta and is cleared only when it
/// flushes (SMA-547 §1).
///
/// **No `Default` impl, deliberately.** Every buffer must carry the `seq` it
/// was created with, so construction goes through [`Pending::new`] via
/// [`ChatTranslator::ensure_pending`]. Deriving `Default` would let an
/// `.or_default()` call site silently mint a buffer with `seq: 0`, which
/// would corrupt the merge order in [`ChatTranslator::canonicalize`]
/// (SMA-550). The absence of the derive is what makes that a compile error
/// rather than a latent bug.
struct Pending {
    /// Monotonic creation order across all buffers in one stream.
    ///
    /// Used to merge two buffers for one call in wire order (SMA-550) and to
    /// give `flush_buffered_names` a deterministic end-of-stream order.
    // Not yet read: the consuming logic lands in SMA-550 Task 2. Stamped here
    // first so every `Pending` carries it from construction onward — see the
    // struct doc for why that ordering (not a later retrofit) is load-bearing.
    #[allow(dead_code)]
    seq: u64,
    /// Accumulated function-name fragments.
    name: String,
    /// Accumulated JSON-arguments fragments.
    args: String,
}

impl Pending {
    /// An empty buffer stamped with its creation order.
    fn new(seq: u64) -> Self {
        Self {
            seq,
            name: String::new(),
            args: String::new(),
        }
    }
}

/// Accumulates SSE deltas and produces [`ModelEvent`]s.
///
/// One instance tracks a single streamed response. `consume` is called once
/// per chunk in order; `finish` is called once at stream end (`[DONE]` or
/// EOF) to emit the terminal event, if any.
pub(crate) struct ChatTranslator {
    /// Resolved call ids, keyed by correlation [`Key`].
    tool_calls: HashMap<Key, String>,
    /// Key → the tool name already emitted to the consumer.
    ///
    /// Holds the *value*, not just the key, so a late fragment can be told
    /// apart from a backend that repeats the whole name on every delta
    /// (SMA-547 §3).
    name_emitted: HashMap<Key, String>,
    /// Keys for which the late-name-fragment warning has already fired, so a
    /// chatty backend cannot produce one warn per argument chunk.
    warned_late_name: HashSet<Key>,
    /// Name/args fragments buffered per call.
    ///
    /// `args` is drain-once: taken on the first delta after the call's `id`
    /// is known and never re-prepended. `name` accumulates across every delta
    /// and is cleared only when the name flushes (SMA-547 §1).
    pending: HashMap<Key, Pending>,
    /// Next value handed out by [`Self::ensure_pending`]; never reused.
    next_seq: u64,
    /// The most recent `finish_reason` observed, buffered until [`Self::finish`].
    finish_reason: Option<String>,
    /// Whether the multi-choice warning has already fired for this stream.
    warned_multi_choice: bool,
}

impl ChatTranslator {
    /// Construct a translator with empty state.
    pub(crate) fn new() -> Self {
        Self {
            tool_calls: HashMap::new(),
            name_emitted: HashMap::new(),
            warned_late_name: HashSet::new(),
            pending: HashMap::new(),
            next_seq: 0,
            finish_reason: None,
            warned_multi_choice: false,
        }
    }

    /// Ensure a buffer exists for `key`, stamping a fresh `seq` on creation.
    ///
    /// Deliberately returns nothing rather than `&mut Pending`: callers then
    /// reach the buffer through `self.pending.get_mut(..)`, which borrows one
    /// field instead of all of `self` and so leaves the surrounding
    /// disjoint-field borrows of `name_emitted` and `tool_calls` intact.
    fn ensure_pending(&mut self, key: &Key) {
        if !self.pending.contains_key(key) {
            self.pending
                .insert(key.clone(), Pending::new(self.next_seq));
            self.next_seq += 1;
        }
    }

    /// Consume one chunk. Never emits `Finish`.
    pub(crate) fn consume(&mut self, chunk: StreamChunk) -> Vec<ModelEvent> {
        let mut out = Vec::new();

        if chunk.choices.len() > 1 && !self.warned_multi_choice {
            self.warned_multi_choice = true;
            tracing::warn!(
                target: "paigasus::litellm::stream",
                n = chunk.choices.len(),
                "response carries multiple choices; only the first is read"
            );
        }

        if let Some(choice) = chunk.choices.first() {
            if let Some(delta) = &choice.delta {
                if let Some(text) = delta.content.as_deref().filter(|s| !s.is_empty()) {
                    out.push(ModelEvent::TokenDelta {
                        text: text.to_owned(),
                    });
                }
                let reasoning = delta
                    .reasoning_content
                    .as_deref()
                    .or(delta.reasoning.as_deref())
                    .filter(|s| !s.is_empty());
                if let Some(text) = reasoning {
                    out.push(ModelEvent::ReasoningDelta {
                        text: text.to_owned(),
                    });
                }
                if let Some(tcs) = &delta.tool_calls {
                    // A positional key is only safe to synthesize when no
                    // entry in *this* array carries an explicit `index` —
                    // otherwise a synthesized `Key::Index(pos)` can collide
                    // with a genuine explicit index elsewhere in the same
                    // array. See the module docs.
                    let any_explicit_index = tcs.iter().any(|tc| tc.index.is_some());
                    for (pos, tc) in tcs.iter().enumerate() {
                        self.handle_tool_call(tc, pos, any_explicit_index, &mut out);
                    }
                }
            }
            if let Some(reason) = &choice.finish_reason {
                // Buffered — see the module docs.
                self.finish_reason = Some(reason.clone());
            }
        }

        if let Some(u) = &chunk.usage {
            out.push(ModelEvent::Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                cached_input_tokens: u
                    .prompt_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens),
                reasoning_tokens: u
                    .completion_tokens_details
                    .as_ref()
                    .and_then(|d| d.reasoning_tokens),
            });
        }

        out
    }

    fn handle_tool_call(
        &mut self,
        tc: &ToolCallChunk,
        pos: usize,
        any_explicit_index: bool,
        out: &mut Vec<ModelEvent>,
    ) {
        let key = match (tc.index, tc.id.as_deref()) {
            (Some(i), _) => Key::Index(i),
            (None, Some(id)) => Key::Id(id.to_owned()),
            (None, None) if any_explicit_index => {
                // A synthesized positional key would risk colliding with a
                // genuine explicit `index` elsewhere in this same array
                // (e.g. entry 0 explicitly `index: 1`, entry 1 has neither
                // — pos 1 would collide with `Key::Index(1)`). That mixes
                // two distinct calls into one and corrupts both, which is
                // worse than dropping this entry, so skip it loudly.
                tracing::warn!(
                    target: "paigasus::litellm::stream",
                    pos,
                    "tool-call delta at this position has neither index nor id, and \
                     another entry in the same array carries an explicit index; \
                     skipping to avoid a key collision"
                );
                return;
            }
            (None, None) => {
                tracing::debug!(
                    target: "paigasus::litellm::stream",
                    pos,
                    "tool-call delta has neither index nor id; correlating by position"
                );
                Key::Index(pos as u32)
            }
        };

        // Both signals test the effective fragment, so a backend sending
        // `"name": ""` behaves like one omitting the field.
        let name_frag = tc
            .function
            .as_ref()
            .and_then(|f| f.name.as_deref())
            .unwrap_or("");
        let args_frag = tc
            .function
            .as_ref()
            .and_then(|f| f.arguments.as_deref())
            .unwrap_or("");

        if let Some(id) = tc.id.as_deref() {
            self.tool_calls
                .entry(key.clone())
                .or_insert_with(|| id.to_owned());
        }

        let Some(call_id) = self.tool_calls.get(&key).cloned() else {
            // No id yet — buffer both fragments.
            self.ensure_pending(&key);
            let slot = self
                .pending
                .get_mut(&key)
                .expect("ensure_pending just inserted this key");
            slot.name.push_str(name_frag);
            slot.args.push_str(args_frag);
            return;
        };

        // A name fragment arriving after the name was emitted cannot be
        // recovered — the event is already downstream. Warn once per call,
        // and not at all when the fragment merely repeats what was emitted.
        if let Some(emitted) = self.name_emitted.get(&key) {
            if !name_frag.is_empty()
                && !emitted.starts_with(name_frag)
                && self.warned_late_name.insert(key.clone())
            {
                tracing::warn!(
                    target: "paigasus::litellm::stream",
                    %call_id,
                    fragment = %name_frag,
                    emitted = %emitted,
                    "tool-call name fragment arrived after the name was emitted; \
                     it cannot be recovered and is dropped"
                );
            }
        }

        // Once a name has flushed, nothing reads `slot.name` again for the
        // life of the stream — skip the accumulation so a backend that
        // repeats the whole name on every delta doesn't grow an unread
        // `String` for no reason (SMA-547 §4). Captured from the same
        // `name_emitted` state the flush condition below re-derives, so it
        // cannot change which deltas flush.
        let already_emitted = self.name_emitted.contains_key(&key);

        self.ensure_pending(&key);
        let slot = self
            .pending
            .get_mut(&key)
            .expect("ensure_pending just inserted this key");
        // A name fragment identical to the name accumulated so far is treated
        // as a whole-name repeat and skipped, not appended -- otherwise a
        // backend that resends the complete function name on every delta
        // (instead of incrementally fragmenting it) would double it, e.g.
        // "search" + "search" -> "searchsearch". Pre-fix this case emitted
        // "search" correctly, so appending unconditionally here would be a
        // regression, not merely an unhandled edge.
        //
        // Known, accepted limitation: a tool genuinely named e.g. "aa" whose
        // name fragments as "a" + "a" cannot be told apart from a repeat
        // under this rule and assembles as "a", not "aa". This matches
        // pre-SMA-547 behaviour (the old code emitted "a" on the first delta
        // and suppressed the second as a duplicate), so it is not a new
        // regression -- whereas omitting this guard regresses the
        // repeated-whole-name case from correct to corrupted. Do not remove
        // this guard without re-deciding that trade-off.
        if !already_emitted && slot.name != name_frag {
            slot.name.push_str(name_frag);
        }
        // `args` is drain-once.
        let mut args = std::mem::take(&mut slot.args);
        args.push_str(args_frag);

        // The name is complete when either signal appears: this delta carried
        // arguments, or this delta carried no name fragment.
        let flush = !self.name_emitted.contains_key(&key)
            && !slot.name.is_empty()
            && (!args_frag.is_empty() || name_frag.is_empty());

        let emit_name = if flush {
            let name = std::mem::take(&mut slot.name);
            self.name_emitted.insert(key.clone(), name.clone());
            Some(name)
        } else {
            None
        };

        if slot.name.is_empty() && slot.args.is_empty() {
            self.pending.remove(&key);
        }

        if emit_name.is_none() && args.is_empty() {
            return;
        }

        out.push(ModelEvent::ToolCallDelta {
            call_id,
            name: emit_name,
            args_delta: args,
        });
    }

    /// Emit any tool name still buffered at end-of-stream.
    ///
    /// Reached by the zero-argument shape, where no `arguments` fragment ever
    /// arrives to signal the name is complete. Correctness, not diagnostics:
    /// the agent loop dispatches on the presence of an `Item::ToolCall` and
    /// reads the tool to run from its `name`.
    ///
    /// Skips entries whose `id` never resolved (nothing to emit under) and
    /// entries whose resolved `call_id` already emitted a name. The latter is
    /// not redundant with `name_emitted`: a call reached under both
    /// `Key::Index` and `Key::Id` has two entries for one `call_id`, and the
    /// contract allows only one name-carrying delta per call.
    fn flush_buffered_names(&mut self) -> Vec<ModelEvent> {
        let mut keys: Vec<Key> = self.pending.keys().cloned().collect();
        keys.sort();

        let mut already: HashSet<String> = self
            .name_emitted
            .keys()
            .filter_map(|k| self.tool_calls.get(k).cloned())
            .collect();

        let mut out = Vec::new();
        for key in keys {
            if self.name_emitted.contains_key(&key) {
                continue;
            }
            let Some(call_id) = self.tool_calls.get(&key).cloned() else {
                continue;
            };
            let Some(slot) = self.pending.get_mut(&key) else {
                continue;
            };
            if slot.name.is_empty() {
                continue;
            }
            // Claimed only once we know this key actually has a name to
            // flush — claiming earlier would let an empty-name entry for a
            // resolved call_id block a sibling key that does have one.
            if !already.insert(call_id.clone()) {
                continue;
            }
            let name = std::mem::take(&mut slot.name);
            self.name_emitted.insert(key, name.clone());
            out.push(ModelEvent::ToolCallDelta {
                call_id,
                name: Some(name),
                args_delta: String::new(),
            });
        }
        out
    }

    /// Emit any buffered tool name, then the terminal `Finish`, if a
    /// `finish_reason` was ever observed.
    ///
    /// Emits no `Finish` on a truncated stream: fabricating `Finish::Stop`
    /// would make a dropped connection indistinguishable from a clean
    /// completion, and `ModelTurnAccumulator` defaults to `Stop`, so the
    /// truncated text would be committed to session history as final. A
    /// buffered *name* is still flushed — the tool call is real, and the
    /// truncation is signalled by the absent `Finish`.
    pub(crate) fn finish(&mut self) -> Vec<ModelEvent> {
        let mut out = self.flush_buffered_names();
        // After the flush, so entries it drained are not re-reported as lost.
        self.warn_unresolved_pending();
        let Some(raw) = self.finish_reason.take() else {
            return out;
        };
        let reason = match raw.as_str() {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::Length,
            "tool_calls" | "function_call" => FinishReason::ToolCalls,
            "content_filter" => FinishReason::ContentFilter,
            other => FinishReason::Other(other.to_owned()),
        };
        out.push(ModelEvent::Finish { reason });
        out
    }

    /// Warn when tool-call fragments were buffered but never flushed.
    ///
    /// A call whose `id` never arrived stays in `pending` forever — a backend
    /// that never sends an `id` at all silently drops the call, so this makes
    /// the loss loud instead of indistinguishable from "the model didn't call
    /// a tool."
    ///
    /// Only entries with **no resolved `call_id`** qualify. Since SMA-547
    /// `pending` also holds entries for calls whose id *is* known (their name
    /// is still accumulating), and reporting those would fire a false warning
    /// on every healthy stream that buffers a name.
    fn warn_unresolved_pending(&self) {
        let keys: Vec<String> = self
            .pending
            .keys()
            .filter(|k| !self.tool_calls.contains_key(k))
            .map(|k| format!("{k:?}"))
            .collect();
        if keys.is_empty() {
            return;
        }
        tracing::warn!(
            target: "paigasus::litellm::stream",
            keys = ?keys,
            "stream ended with buffered tool-call fragments whose id was never resolved; dropping them"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(v: serde_json::Value) -> StreamChunk {
        serde_json::from_value(v).expect("chunk must deserialize")
    }

    fn texts(evs: &[ModelEvent]) -> Vec<String> {
        evs.iter()
            .filter_map(|e| match e {
                ModelEvent::TokenDelta { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn content_deltas_become_token_deltas() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hel"}}]
        })));
        assert_eq!(texts(&evs), vec!["Hel"]);
    }

    #[test]
    fn empty_content_emits_nothing() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"content": ""}}]
        })));
        assert!(evs.is_empty());
    }

    #[test]
    fn reasoning_content_becomes_reasoning_delta() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"reasoning_content": "thinking"}}]
        })));
        assert!(matches!(&evs[0], ModelEvent::ReasoningDelta { text } if text == "thinking"));
    }

    #[test]
    fn reasoning_fallback_field_is_honoured() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"reasoning": "alt"}}]
        })));
        assert!(matches!(&evs[0], ModelEvent::ReasoningDelta { text } if text == "alt"));
    }

    #[test]
    fn finish_is_not_emitted_inline() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        })));
        assert!(
            !evs.iter().any(|e| matches!(e, ModelEvent::Finish { .. })),
            "Finish must be deferred to finish(), never emitted inline"
        );
    }

    #[test]
    fn trailing_usage_chunk_then_finish_preserves_ordering() {
        // The exact shape captured from LiteLLM 1.97.0: the usage snapshot
        // arrives in its own chunk AFTER the finish_reason chunk.
        let mut t = ChatTranslator::new();
        let mut all = Vec::new();
        all.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"content": "hi"}}]
        }))));
        all.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        }))));
        all.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {}}],
            "usage": {"prompt_tokens": 8, "completion_tokens": 6,
                      "completion_tokens_details": {"reasoning_tokens": 0}}
        }))));
        all.extend(t.finish());

        let last = all.last().unwrap();
        assert!(
            matches!(last, ModelEvent::Finish { .. }),
            "Finish must be the terminal event, got {last:?}"
        );
        let usage_pos = all
            .iter()
            .position(|e| matches!(e, ModelEvent::Usage { .. }))
            .expect("usage must be emitted");
        assert!(usage_pos < all.len() - 1, "Usage must precede Finish");
    }

    #[test]
    fn usage_maps_all_token_fields() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 10, "completion_tokens": 4,
                "prompt_tokens_details": {"cached_tokens": 3},
                "completion_tokens_details": {"reasoning_tokens": 2}
            }
        })));
        match &evs[0] {
            ModelEvent::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                reasoning_tokens,
            } => {
                assert_eq!(*input_tokens, 10);
                assert_eq!(*output_tokens, 4);
                assert_eq!(*cached_input_tokens, Some(3));
                assert_eq!(*reasoning_tokens, Some(2));
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn usage_without_details_objects_still_maps() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [], "usage": {"prompt_tokens": 1, "completion_tokens": 2}
        })));
        match &evs[0] {
            ModelEvent::Usage {
                cached_input_tokens,
                reasoning_tokens,
                ..
            } => {
                assert!(cached_input_tokens.is_none());
                assert!(reasoning_tokens.is_none());
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn finish_reasons_map_leniently() {
        for (raw, expected) in [
            ("stop", FinishReason::Stop),
            ("length", FinishReason::Length),
            ("tool_calls", FinishReason::ToolCalls),
            ("function_call", FinishReason::ToolCalls),
            ("content_filter", FinishReason::ContentFilter),
        ] {
            let mut t = ChatTranslator::new();
            t.consume(chunk(serde_json::json!({
                "choices": [{"index": 0, "delta": {}, "finish_reason": raw}]
            })));
            let evs = t.finish();
            assert!(matches!(&evs[0], ModelEvent::Finish { reason } if *reason == expected));
        }
    }

    #[test]
    fn unknown_finish_reason_lands_in_other() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "guardrail_intervened"}]
        })));
        let evs = t.finish();
        match &evs[0] {
            ModelEvent::Finish { reason } => {
                assert_eq!(*reason, FinishReason::Other("guardrail_intervened".into()));
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn truncated_stream_emits_no_finish() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"content": "partial"}}]
        })));
        assert!(
            t.finish().is_empty(),
            "a stream that never sent finish_reason must not fabricate Finish"
        );
    }

    #[test]
    fn tool_call_name_is_emitted_once_then_args_follow() {
        let mut t = ChatTranslator::new();
        let mut evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "call_1", "function": {"name": "get_weather", "arguments": "{\"ci"}}
            ]}}]
        })));
        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "ty\":\"Berlin\"}"}}
            ]}}]
        }))));

        let calls: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    name,
                    args_delta,
                } => Some((call_id.clone(), name.clone(), args_delta.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "call_1");
        assert_eq!(calls[0].1, Some("get_weather".to_owned()));
        assert_eq!(calls[1].1, None, "name must be emitted only once");
        let joined: String = calls.iter().map(|c| c.2.clone()).collect();
        assert_eq!(joined, "{\"city\":\"Berlin\"}");
    }

    #[test]
    fn tool_call_id_arriving_late_does_not_lose_name_or_args() {
        // The id is NOT guaranteed to arrive on the first delta.
        let mut t = ChatTranslator::new();
        let mut evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"name": "sea", "arguments": "{\"q"}}
            ]}}]
        })));
        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"name": "rch", "arguments": "\":1}"}}
            ]}}]
        }))));
        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "call_late"}
            ]}}]
        }))));

        let calls: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    name,
                    args_delta,
                } => Some((call_id.clone(), name.clone(), args_delta.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 1, "buffered until the id was known");
        assert_eq!(calls[0].0, "call_late");
        assert_eq!(
            calls[0].1,
            Some("search".to_owned()),
            "fragmented name must be concatenated"
        );
        assert_eq!(calls[0].2, "{\"q\":1}");
    }

    /// SMA-547: a name fragmented across deltas that arrive AFTER the id
    /// resolves must be assembled, not truncated at the first fragment.
    ///
    /// Confirmed to FAIL against the pre-fix code: the `name_emitted` guard
    /// suppressed "weather", yielding a tool named `get_`.
    #[test]
    fn name_fragments_after_id_are_assembled() {
        let mut t = ChatTranslator::new();
        let mut evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "call_abc", "function": {"name": "get_", "arguments": ""}}
            ]}}]
        })));
        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"name": "weather", "arguments": ""}}
            ]}}]
        }))));
        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "{\"city\":\"Berlin\"}"}}
            ]}}]
        }))));

        let calls: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    name,
                    args_delta,
                } => Some((call_id.clone(), name.clone(), args_delta.clone())),
                _ => None,
            })
            .collect();

        assert_eq!(calls.len(), 1, "one delta, once the name is known complete");
        assert_eq!(calls[0].0, "call_abc");
        assert_eq!(
            calls[0].1,
            Some("get_weather".to_owned()),
            "fragments must concatenate"
        );
        assert_eq!(calls[0].2, "{\"city\":\"Berlin\"}");
    }

    #[test]
    fn two_tool_call_indices_stay_separate() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "a", "function": {"name": "f", "arguments": "{}"}},
                {"index": 1, "id": "b", "function": {"name": "g", "arguments": "{}"}}
            ]}}]
        })));
        let ids: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn tool_call_without_index_falls_back_to_id() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"id": "only_id", "function": {"name": "f", "arguments": "{}"}}
            ]}}]
        })));
        assert!(evs.iter().any(|e| matches!(
            e, ModelEvent::ToolCallDelta { call_id, .. } if call_id == "only_id"
        )));
    }

    #[test]
    fn tool_call_with_neither_index_nor_id_is_not_silently_dropped() {
        // A backend may omit both `index` and `id` on the very first delta
        // for a tool call. The array position doubles as an implicit
        // index, so once a later delta restates that same index alongside
        // the `id`, the fragments buffered under the positional key must
        // be flushed — not silently lost because they were keyed
        // differently from every later delta.
        let mut t = ChatTranslator::new();
        let mut evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"function": {"name": "f", "arguments": "{}"}}
            ]}}]
        })));
        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "call_positional"}
            ]}}]
        }))));

        let calls: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    name,
                    args_delta,
                } => Some((call_id.clone(), name.clone(), args_delta.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            calls.len(),
            1,
            "positional-only fragments must not be dropped"
        );
        assert_eq!(calls[0].0, "call_positional");
        assert_eq!(calls[0].1, Some("f".to_owned()));
        assert_eq!(calls[0].2, "{}");
    }

    #[test]
    fn mixed_explicit_and_positional_entries_do_not_collide() {
        // Entry 0 has an explicit index (1) that numerically equals entry
        // 1's array position (1). A naive positional fallback would
        // synthesize Key::Index(1) for entry 1 too, colliding with entry
        // 0's genuine Key::Index(1) and corrupting call "y" with entry 1's
        // fragments while losing entry 1's own name entirely. The
        // index-less entry must be skipped, not merged.
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 1, "id": "y", "function": {"name": "g", "arguments": "{}"}},
                {"function": {"name": "f", "arguments": "{}"}}
            ]}}]
        })));

        let calls: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    name,
                    args_delta,
                } => Some((call_id.clone(), name.clone(), args_delta.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            calls.len(),
            1,
            "the ambiguous entry must be skipped, not merged into another call"
        );
        assert_eq!(calls[0].0, "y");
        assert_eq!(calls[0].1, Some("g".to_owned()));
        assert!(
            !calls[0].2.contains('f'),
            "call y's args must not absorb the other entry's fragment, got {:?}",
            calls[0].2
        );
    }

    #[test]
    fn unindexed_continuation_after_an_indexed_first_delta_still_joins() {
        // Regression guard on the human's ruling: a genuine continuation
        // chunk — one whose tool_calls array has no explicit index at all
        // — must still correlate by position against an earlier chunk that
        // did carry an explicit index for that same call.
        let mut t = ChatTranslator::new();
        let mut evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "a", "function": {"name": "f", "arguments": "{\""}}
            ]}}]
        })));
        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"function": {"arguments": "x\": 1}"}}
            ]}}]
        }))));

        let calls: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    args_delta,
                    ..
                } => Some((call_id.clone(), args_delta.clone())),
                _ => None,
            })
            .collect();
        assert!(calls.iter().all(|(id, _)| id == "a"));
        let joined: String = calls.iter().map(|(_, a)| a.clone()).collect();
        assert_eq!(joined, "{\"x\": 1}");
    }

    #[test]
    fn only_the_first_choice_is_read() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [
                {"index": 0, "delta": {"content": "first"}},
                {"index": 1, "delta": {"content": "second"}}
            ]
        })));
        assert_eq!(texts(&evs), vec!["first"]);
    }

    #[test]
    fn chunk_with_no_choices_key_deserializes() {
        // Error/keepalive frames omit `choices` entirely.
        let c: StreamChunk = serde_json::from_str("{}").expect("must not fail");
        assert!(c.choices.is_empty());
    }

    /// A single delta carrying id, name and non-empty args emits immediately.
    #[test]
    fn complete_single_delta_emits_name_with_no_added_latency() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "get_time", "arguments": "{}"}}
            ]}}]
        })));
        assert!(matches!(
            &evs[0],
            ModelEvent::ToolCallDelta { name: Some(n), args_delta, .. }
                if n == "get_time" && args_delta == "{}"
        ));
    }

    /// A zero-argument call flushes its name from finish(), before Finish.
    #[test]
    fn zero_argument_call_flushes_name_before_finish() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "ping"}}
            ]}, "finish_reason": "tool_calls"}]
        })));
        assert!(evs.is_empty(), "no completion signal during the stream");

        let fin = t.finish();
        assert_eq!(fin.len(), 2, "flushed name then Finish");
        assert!(matches!(
            &fin[0],
            ModelEvent::ToolCallDelta { call_id, name: Some(n), args_delta }
                if call_id == "c1" && n == "ping" && args_delta.is_empty()
        ));
        assert!(
            matches!(fin[1], ModelEvent::Finish { .. }),
            "Finish must stay terminal"
        );
    }

    /// A truncated stream flushes the name but reports no clean stop.
    #[test]
    fn truncated_stream_flushes_name_but_emits_no_finish() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "ping"}}
            ]}}]
        })));
        let fin = t.finish();
        assert_eq!(fin.len(), 1);
        assert!(matches!(
            &fin[0],
            ModelEvent::ToolCallDelta { name: Some(n), .. } if n == "ping"
        ));
    }

    /// finish() takes its buffers, so a second call is a no-op.
    #[test]
    fn finish_is_idempotent_after_draining() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "ping"}}
            ]}, "finish_reason": "stop"}]
        })));
        assert_eq!(t.finish().len(), 2);
        assert!(
            t.finish().is_empty(),
            "a second finish() must yield nothing"
        );
    }

    /// A repeat of the whole name is not a lost fragment and must not warn.
    #[test]
    fn repeated_whole_name_is_not_treated_as_a_late_fragment() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "search", "arguments": "{\"q\":"}}
            ]}}]
        })));
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"name": "search", "arguments": "1}"}}
            ]}}]
        })));
        assert!(
            t.warned_late_name.is_empty(),
            "a repeat of the emitted name must not warn"
        );
    }

    /// Round-1 CodeRabbit regression: a backend that repeats the WHOLE
    /// function name on a delta carrying no arguments must not have both
    /// copies concatenated into the buffered name. The test above
    /// (`repeated_whole_name_is_not_treated_as_a_late_fragment`) does not
    /// catch this because its first delta carries non-empty `arguments`, so
    /// the flush happens before the repeat ever arrives; here the first
    /// delta carries none, so both deltas hit the accumulation path.
    #[test]
    fn repeated_whole_name_before_any_arguments_is_not_doubled() {
        let mut t = ChatTranslator::new();
        let mut evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "search", "arguments": ""}}
            ]}}]
        })));
        assert!(evs.is_empty(), "no completion signal yet");

        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"name": "search", "arguments": "{"}}
            ]}}]
        }))));

        let calls: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    name,
                    args_delta,
                } => Some((call_id.clone(), name.clone(), args_delta.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 1, "expected exactly one ToolCallDelta");
        assert_eq!(calls[0].0, "c1");
        assert_eq!(
            calls[0].1,
            Some("search".to_owned()),
            "a repeated whole name must not be doubled to \"searchsearch\""
        );
        assert_eq!(calls[0].2, "{");
    }

    /// Companion guard on the fix above: distinct fragments arriving before
    /// any arguments must still concatenate -- this is SMA-547's actual
    /// target case, and the repeat-suppression fix must not over-suppress
    /// it.
    #[test]
    fn distinct_fragments_before_any_arguments_still_concatenate() {
        let mut t = ChatTranslator::new();
        let mut evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "get_", "arguments": ""}}
            ]}}]
        })));
        assert!(evs.is_empty(), "no completion signal yet");

        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"name": "weather", "arguments": "{}"}}
            ]}}]
        }))));

        let calls: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    name,
                    args_delta,
                } => Some((call_id.clone(), name.clone(), args_delta.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 1, "expected exactly one ToolCallDelta");
        assert_eq!(calls[0].0, "c1");
        assert_eq!(
            calls[0].1,
            Some("get_weather".to_owned()),
            "distinct fragments must still concatenate"
        );
        assert_eq!(calls[0].2, "{}");
    }

    /// A genuine late fragment is recorded once and does not rewrite the
    /// already-emitted name.
    #[test]
    fn late_name_fragment_warns_once() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "get_", "arguments": "{\"a\":"}}
            ]}}]
        })));
        for frag in ["weather", "weather"] {
            t.consume(chunk(serde_json::json!({
                "choices": [{"index": 0, "delta": {"tool_calls": [
                    {"index": 0, "function": {"name": frag, "arguments": "1"}}
                ]}}]
            })));
        }
        assert_eq!(t.warned_late_name.len(), 1, "at most one warning per call");
        assert_eq!(
            t.name_emitted.get(&Key::Index(0)).map(String::as_str),
            Some("get_"),
            "the already-emitted name is not retroactively changed"
        );
    }

    /// An entry whose id never resolved is not flushed — there is no call_id
    /// to emit under — and stays the domain of the unresolved-pending warning.
    #[test]
    fn unresolved_entries_are_not_flushed() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"name": "orphan"}}
            ]}, "finish_reason": "stop"}]
        })));
        let fin = t.finish();
        assert_eq!(fin.len(), 1, "only Finish; the orphan has no call_id");
        assert!(matches!(fin[0], ModelEvent::Finish { .. }));
    }

    /// One `call_id` reachable under both `Key::Index` and `Key::Id` must
    /// still yield exactly one name-carrying delta. Guards the dedup set in
    /// `flush_buffered_names`; without it this emits two.
    #[test]
    fn one_call_id_under_two_keys_flushes_a_single_name() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "get_"}}
            ]}}]
        })));
        // No `index` -> keys as Key::Id("c1"): a second entry for one call.
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"id": "c1", "function": {"name": "weather"}}
            ]}, "finish_reason": "tool_calls"}]
        })));

        let named: Vec<_> = t
            .finish()
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    name: Some(n),
                    ..
                } => Some((call_id.clone(), n.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(named.len(), 1, "one name-carrying delta per call_id");
        assert_eq!(
            named[0],
            ("c1".to_owned(), "get_".to_owned()),
            "Key::Index sorts before Key::Id, so the winner is deterministic"
        );
    }

    /// Pins the `already` seed in `flush_buffered_names`. Without it, a
    /// call_id that already flushed its name mid-stream under one key (here
    /// `Key::Index(0)`) can still have a *second* name emitted for the same
    /// call_id from a sibling key (`Key::Id("c1")`) that never got a chance
    /// to flush mid-stream — the seed is what stops that second emission.
    #[test]
    fn flush_does_not_re_emit_a_name_already_flushed_under_another_key() {
        let mut t = ChatTranslator::new();
        // Non-empty args complete the name mid-stream under Key::Index(0),
        // recording name_emitted[Key::Index(0)] = "get_".
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "get_", "arguments": "{"}}
            ]}}]
        })));
        // No `index` -> keys as Key::Id("c1"): a second entry for the same
        // call_id, buffered but never flushed mid-stream (no args fragment
        // on this delta to complete it).
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"id": "c1", "function": {"name": "weather"}}
            ]}}]
        })));

        let named: Vec<_> = t
            .finish()
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    name: Some(n),
                    ..
                } => Some((call_id.clone(), n.clone())),
                _ => None,
            })
            .collect();
        assert!(
            named.is_empty(),
            "Key::Id(\"c1\")'s buffered name must not re-emit for call_id \"c1\", \
             which already flushed under Key::Index(0); got {named:?}"
        );
    }

    /// Buffered pre-id args followed by a bare id-carrying delta must still
    /// emit those args. Testing the emit-nothing guard against this delta's
    /// own fragment (rather than the combined value) would swallow them.
    #[test]
    fn buffered_args_survive_a_bare_id_delta() {
        let mut t = ChatTranslator::new();
        let mut evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "{\"a\":1}"}}
            ]}}]
        })));
        assert!(evs.is_empty(), "buffered until the id arrives");

        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1"}
            ]}}]
        }))));

        assert_eq!(evs.len(), 1, "the buffered args must not be swallowed");
        assert!(matches!(
            &evs[0],
            ModelEvent::ToolCallDelta { call_id, name: None, args_delta }
                if call_id == "c1" && args_delta == "{\"a\":1}"
        ));
    }
}
