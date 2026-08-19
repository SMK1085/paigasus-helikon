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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

    /// Rewrite `key` to the canonical slot for `call_id`, migrating any
    /// fragments buffered under the pre-canonical key.
    ///
    /// Every delta for one call — however it was keyed on the wire — shares a
    /// single state entry from here on. That is what makes "at most one
    /// name-carrying `ToolCallDelta` per `call_id`" hold by construction
    /// rather than by guard, and it is what lets a name fragmented across the
    /// `Key::Index` / `Key::Id` boundary reassemble instead of losing a
    /// fragment (SMA-550).
    fn canonicalize(&mut self, key: Key, call_id: &str) -> Key {
        // Already canonical — return without allocating. This is the common
        // path: LiteLLM keys every tool-call delta by `index`, so a resolved
        // stream would otherwise pay a `String` allocation per args chunk.
        if matches!(&key, Key::Id(id) if id == call_id) {
            return key;
        }
        let canonical = Key::Id(call_id.to_owned());

        if let Some(old) = self.pending.remove(&key) {
            self.ensure_pending(&canonical);
            let slot = self
                .pending
                .get_mut(&canonical)
                .expect("ensure_pending just inserted this key");

            // A resolved slot drains `args` on every delta and is removed once
            // both fields are empty, so `slot.args` is always empty here --
            // which is what makes `insert_str(0, ..)` below an assignment in
            // practice rather than a splice. The assert does not guard
            // correctness: `insert_str(0, ..)` stays order-preserving even if
            // `slot.args` were non-empty. It exists so that a future
            // relaxation of drain-once surfaces here for a deliberate
            // re-decision, instead of silently changing this from an
            // assignment to a splice unnoticed.
            debug_assert!(
                slot.args.is_empty(),
                "a resolved slot drains its args on every delta"
            );
            slot.args.insert_str(0, &old.args);

            if !old.name.is_empty() && !slot.name.is_empty() {
                tracing::warn!(
                    target: "paigasus::litellm::stream",
                    %call_id,
                    "tool-call name fragments for one call arrived under two \
                     correlation keys; merging in buffer-creation order, which \
                     may misorder them if the two keys interleaved"
                );
            }
            // Order by creation `seq`, not by which buffer is migrating.
            // Both orderings are reachable, and a plain prepend or a plain
            // append is wrong in exactly one of them (SMA-550 §Merge order).
            if old.seq < slot.seq {
                slot.name.insert_str(0, &old.name);
                slot.seq = old.seq;
            } else {
                slot.name.push_str(&old.name);
            }
        }

        // `flush_buffered_names` and `warn_unresolved_pending` both resolve a
        // pending key through `tool_calls`. Without this the canonical key
        // resolves to nothing: the call is skipped at flush AND then falsely
        // reported as an unresolved loss. Pinned by
        // `canonical_key_resolves_through_tool_calls`.
        self.tool_calls
            .entry(canonical.clone())
            .or_insert_with(|| call_id.to_owned());
        canonical
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

        // From here on, one call_id owns exactly one state entry.
        let key = self.canonicalize(key, &call_id);

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
        // Sorted by buffer-creation order, not by `Key`. After SMA-550 every
        // resolved key is `Key::Id(call_id)`, so sorting by `Key` would mean
        // lexicographic-by-call_id — silently reordering parallel calls
        // against the wire. `seq` is unique per buffer, so this is a total,
        // deterministic order that matches first appearance.
        let mut keys: Vec<Key> = self.pending.keys().cloned().collect();
        keys.sort_by_key(|k| self.pending[k].seq);

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

    /// Wrap a `tool_calls` array in the surrounding chunk envelope.
    fn tc_chunk(tool_calls: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"choices": [{"index": 0, "delta": {"tool_calls": tool_calls}}]})
    }

    /// Drive every chunk through `consume`, then `finish`, collecting all
    /// events. Collecting across both is required: the SMA-550 invariant is
    /// per-stream, and a `finish()`-only assertion passes vacuously against
    /// the pre-fix translator, whose violation is two *mid-stream* emissions.
    fn drive(t: &mut ChatTranslator, chunks: Vec<serde_json::Value>) -> Vec<ModelEvent> {
        let mut out = Vec::new();
        for c in chunks {
            out.extend(t.consume(chunk(c)));
        }
        out.extend(t.finish());
        out
    }

    /// `(call_id, name)` for every delta carrying `Some(name)`, in order.
    fn named(evs: &[ModelEvent]) -> Vec<(String, String)> {
        evs.iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    name: Some(n),
                    ..
                } => Some((call_id.clone(), n.clone())),
                _ => None,
            })
            .collect()
    }

    /// Concatenated `args_delta` for one `call_id`, in emission order.
    fn args_of(evs: &[ModelEvent], call_id: &str) -> String {
        evs.iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id: c,
                    args_delta,
                    ..
                } if c == call_id => Some(args_delta.as_str()),
                _ => None,
            })
            .collect()
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

    /// Two parallel zero-argument calls flush in wire order, not in
    /// call_id-lexicographic order.
    ///
    /// Pins the `seq` sort in `flush_buffered_names`. After SMA-550's
    /// canonicalization every resolved pending key is `Key::Id(call_id)`, so
    /// a sort by `Key` would silently reorder these two by call_id —
    /// `call_a` before `call_z` — reversing what the wire said. The ids are
    /// chosen so lexicographic and wire order disagree.
    #[test]
    fn parallel_zero_argument_calls_flush_in_wire_order() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![tc_chunk(serde_json::json!([
                {"index": 0, "id": "call_z", "function": {"name": "zulu"}},
                {"index": 1, "id": "call_a", "function": {"name": "alpha"}}
            ]))],
        );
        assert_eq!(
            named(&evs),
            vec![
                ("call_z".to_owned(), "zulu".to_owned()),
                ("call_a".to_owned(), "alpha".to_owned()),
            ],
            "wire order (index 0 then 1), not lexicographic by call_id"
        );
    }

    /// SMA-550 acceptance criterion: one `call_id` reached under both `Key`
    /// variants must not produce two name-carrying deltas — mid-stream
    /// included. Shape A.
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `Some("get_")` and then `Some("weather")`, both from `consume`.
    #[test]
    fn dual_key_call_emits_at_most_one_name_mid_stream() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"name": "get_", "arguments": "{"}}
                ])),
                tc_chunk(serde_json::json!([
                    {"id": "c1", "function": {"name": "weather", "arguments": "}"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "get_".to_owned())],
            "exactly one name-carrying delta per call_id, across consume and finish"
        );
        assert_eq!(
            args_of(&evs, "c1"),
            "{}",
            "suppressing the second name must not swallow its args"
        );
    }

    /// SMA-550 shape B: a name fragmented across the key boundary reassembles
    /// instead of losing the pre-boundary fragment.
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `Some("weather")` — `get_` is silently discarded by the EOS guard.
    #[test]
    fn name_fragments_split_across_the_key_boundary_reassemble() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"name": "get_"}}
                ])),
                tc_chunk(serde_json::json!([
                    {"id": "c1", "function": {"name": "weather", "arguments": "{}"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "get_weather".to_owned())]
        );
    }

    /// SMA-550 shape C: the id-keyed buffer is created FIRST, so the
    /// index-keyed buffer migrating into it must be *appended*.
    ///
    /// Together with `index_keyed_buffer_created_first_merges_in_order` this
    /// is why `Pending` carries a `seq`: a naive `insert_str(0, ..)` prepend
    /// passes that test and yields `Some("weathget_er")` here.
    ///
    /// Confirmed to FAIL against the pre-fix translator (`Some("weather")`).
    #[test]
    fn id_keyed_buffer_created_before_the_index_keyed_one_merges_in_order() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([{"id": "c1", "function": {"name": "get_"}}])),
                tc_chunk(serde_json::json!([{"index": 0, "function": {"name": "weath"}}])),
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"name": "er", "arguments": "{}"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "get_weather".to_owned())]
        );
    }

    /// SMA-550 shape B′: the index-keyed buffer is created FIRST, so it must
    /// be *prepended* when it migrates. Mirror of the test above; a naive
    /// `push_str` append yields `Some("weathget_er")` here.
    ///
    /// Confirmed to FAIL against the pre-fix translator (`Some("get_er")`).
    #[test]
    fn index_keyed_buffer_created_first_merges_in_order() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([{"index": 0, "function": {"name": "get_"}}])),
                tc_chunk(serde_json::json!([{"id": "c1", "function": {"name": "weath"}}])),
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"name": "er", "arguments": "{}"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "get_weather".to_owned())]
        );
    }

    /// SMA-550 shape D: two entries in one array carrying different `index`
    /// values but the same `id` are one call, and yield one name.
    ///
    /// The merged name `alphabeta` is not "correct" in any deep sense — the
    /// input is malformed, since an `id` identifies a call — but it is one
    /// name for one call_id, which is the invariant under test.
    /// `providers-openai` emits TWO names here; see the divergence comment in
    /// its `chat.rs`.
    ///
    /// Confirmed to FAIL against the pre-fix translator (`Some("beta")`).
    #[test]
    fn two_indexes_with_one_id_merge_into_a_single_call() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![tc_chunk(serde_json::json!([
                {"index": 0, "id": "c1", "function": {"name": "alpha"}},
                {"index": 1, "id": "c1", "function": {"name": "beta", "arguments": "{}"}}
            ]))],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "alphabeta".to_owned())]);
    }

    /// SMA-550 shape E, the accepted residual: when the two keys interleave
    /// at *fragment* level, no buffer-level order can reconstruct the wire
    /// sequence, so the merge misorders the fragments -- though it loses
    /// none of them. The third chunk's fragment ("IE") is deliberately
    /// distinct from the second's ("ID"): a repeat there would be swallowed
    /// by the pre-existing SMA-547 whole-name-repeat guard before the merge
    /// ever runs, which would make this pass for the wrong reason and mask
    /// a real ordering bug in `canonicalize`.
    ///
    /// This is deliberately pinned rather than left undefined. It is still
    /// strictly better than the pre-fix translator, which emits
    /// `Some("IDXIDX")` and silently discards the `Id`-keyed fragments
    /// entirely: the merge here is lossless and carries a `warn!`. Do not
    /// "fix" the misordering without adding per-fragment sequencing and
    /// re-deciding the trade-off.
    #[test]
    fn interleaved_dual_keying_is_lossless_and_misordered() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([{"index": 0, "function": {"name": "IDX"}}])),
                tc_chunk(serde_json::json!([{"id": "c1", "function": {"name": "ID"}}])),
                tc_chunk(serde_json::json!([{"id": "c1", "function": {"name": "IE"}}])),
                tc_chunk(serde_json::json!([{"index": 0, "function": {"name": "IDX"}}])),
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"arguments": "{}"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "IDXIDXIDIE".to_owned())],
            "the merge loses nothing, but the wire order is not reconstructable \
             from two independently-ordered buffers"
        );
    }

    /// The canonical key must be registered in `tool_calls`.
    ///
    /// Not defensive padding: `flush_buffered_names` resolves a pending key
    /// through `tool_calls` and would skip a canonicalized call entirely,
    /// and `warn_unresolved_pending` would then report that same call as an
    /// unresolved loss on every healthy stream. Pinned so a future "this
    /// self-mapping looks redundant" cleanup fails loudly.
    #[test]
    fn canonical_key_resolves_through_tool_calls() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(tc_chunk(serde_json::json!([
            {"index": 0, "id": "c1", "function": {"name": "get_weather"}}
        ]))));
        assert_eq!(
            t.tool_calls
                .get(&Key::Id("c1".to_owned()))
                .map(String::as_str),
            Some("c1"),
            "canonicalize must register the canonical key"
        );
    }

    /// SMA-550's secondary defect: the fragment dropped in shape A must be
    /// reported. Pre-fix it was recorded under no key at all, because the
    /// losing key never emitted and so never reached the late-name check.
    #[test]
    fn dual_key_late_fragment_is_reported() {
        let mut t = ChatTranslator::new();
        drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"name": "get_", "arguments": "{"}}
                ])),
                tc_chunk(serde_json::json!([
                    {"id": "c1", "function": {"name": "weather", "arguments": "}"}}
                ])),
            ],
        );
        assert!(
            t.warned_late_name.contains(&Key::Id("c1".to_owned())),
            "the dropped `weather` fragment must be recorded under the canonical key"
        );
        assert_eq!(t.warned_late_name.len(), 1, "at most one warning per call");
    }

    /// Mutation guard for `slot.seq = old.seq;` in `canonicalize` (SMA-550
    /// review, Finding 1). Pins that a migrated buffer inherits the
    /// *migrating* buffer's wire position for `flush_buffered_names`'
    /// `seq`-sort, not the canonical slot's own (later) creation seq.
    ///
    /// `c1`'s `Index(0)`-keyed buffer is created first (seq 0, chunk 1), but
    /// only resolves into its canonical `Id("c1")` slot in chunk 3 -- by
    /// which point `c2`'s canonical `Id("c2")` slot already exists with an
    /// earlier seq (seq 1, chunk 2). Without `slot.seq = old.seq`, the
    /// migrated `c1` slot would keep the canonical slot's just-created seq
    /// (2) instead, sorting after `c2` at flush and reversing wire order.
    #[test]
    fn migrated_buffer_keeps_its_wire_position_in_the_flush_order() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([{"index": 0, "function": {"name": "alpha"}}])),
                tc_chunk(serde_json::json!([
                    {"index": 1, "id": "c2", "function": {"name": "bravo"}}
                ])),
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"name": "_x"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![
                ("c1".to_owned(), "alpha_x".to_owned()),
                ("c2".to_owned(), "bravo".to_owned()),
            ],
            "the migrated buffer must keep its original wire position, not the \
             canonical slot's own (later) creation order"
        );
    }
}
