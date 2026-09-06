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
//!    Correlation itself is canonicalized: once a delta resolves the call's
//!    `id`, the key becomes `Key::Id(call_id)` and any fragments buffered
//!    under the pre-canonical key migrate into that slot in buffer-creation
//!    order. One `call_id` therefore owns exactly one state entry, which is
//!    what makes "at most one name-carrying delta per `call_id`" structural
//!    rather than guarded (SMA-550).

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
    /// by `Index`). Once a call's `id` resolves, `Index` is only ever a wire
    /// key again — its state has migrated to the canonical [`Key::Id`]. See
    /// [`ChatTranslator::canonicalize`].
    Index(u32),
    /// Correlated by `delta.tool_calls[].id`.
    ///
    /// Reached two ways: as the wire key when `index` is absent, and — since
    /// SMA-550 — as the *canonical* key for every call whose `id` has
    /// resolved, whether or not it also carried an `index`. See
    /// [`ChatTranslator::canonicalize`].
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
    ///
    /// Holds both wire keys (`Key::Index(i)`, so later index-only deltas keep
    /// resolving) and the canonical `Key::Id(call_id) -> call_id` self-mapping
    /// that [`Self::canonicalize`] registers. The self-mapping is load-bearing:
    /// `flush_buffered_names` and `warn_unresolved_pending` both resolve a
    /// pending key through this map.
    tool_calls: HashMap<Key, String>,
    /// Canonical key → the tool name already emitted to the consumer.
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
    /// Keyed by the canonical key once the call's `id` resolves, so one
    /// `call_id` never owns two buffers (SMA-550).
    ///
    /// `args` is drain-once: taken on the first delta after the call's `id`
    /// is known and never re-prepended. `name` accumulates across every delta
    /// and is cleared only when the name flushes (SMA-547 §1).
    pending: HashMap<Key, Pending>,
    /// Next value handed out by [`Self::ensure_pending`]; never reused.
    next_seq: u64,
    /// Wire keys for which the empty-`id` warning has already fired, so a
    /// backend that sends `"id": ""` on every delta warns once per call
    /// rather than once per chunk.
    warned_blank_id: HashSet<Key>,
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
            warned_blank_id: HashSet::new(),
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
        // An empty `id` is not an identity. A backend that sends `"id": ""` on
        // every entry would otherwise collapse every one of its parallel calls
        // into a single `Key::Id("")` slot, and all but the first would vanish
        // from the stream entirely — a strictly worse outcome than the
        // dual-keying this function exists to fix. Leave such deltas on their
        // wire key. That keeps distinct calls distinct only because a blank id
        // is filtered out when the wire key is chosen (see `handle_tool_call`)
        // — otherwise two blank-id entries with no `index` would both arrive
        // here already sharing one `Key::Id("")` slot (SMA-616).
        if call_id.is_empty() {
            if self.warned_blank_id.insert(key.clone()) {
                tracing::warn!(
                    target: "paigasus::litellm::stream",
                    ?key,
                    "tool-call delta carries an empty id; correlating by wire key \
                     instead, since an empty id cannot identify a call"
                );
            }
            return key;
        }
        // Already canonical — return without allocating. Reached only when the
        // delta was keyed by `id` with no `index`; LiteLLM itself sends `index`
        // on every delta, so its streams fall through and clone the `call_id`
        // once per delta. That clone is accepted — every emitted
        // `ToolCallDelta` already pays one.
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

            // The canonical slot has already emitted its name, so the migrating
            // fragment can never reach a consumer: the flush condition below
            // tests `name_emitted` and will not fire again for this call. It
            // must not be left sitting in `pending` either — `flush_buffered_names`
            // skips entries whose call_id already emitted, and
            // `warn_unresolved_pending` ignores entries whose key resolves,
            // which this one now does. Dropping it silently would recreate the
            // undiagnosed loss SMA-550 exists to eliminate, so drop it loudly
            // and record it the same way a late wire fragment is recorded.
            if !old.name.is_empty() {
                if let Some(emitted) = self.name_emitted.get(&canonical) {
                    if self.warned_late_name.insert(canonical.clone()) {
                        tracing::warn!(
                            target: "paigasus::litellm::stream",
                            %call_id,
                            fragment = %old.name,
                            emitted = %emitted,
                            "tool-call name fragment buffered under another correlation \
                             key arrived after the name was emitted; it cannot be \
                             recovered and is dropped"
                        );
                    }
                    return canonical;
                }
            }

            // A migrating fragment identical to what the canonical slot already
            // holds is a whole-name repeat, not a continuation — the same case
            // the wire path guards below, for the same reason: a backend that
            // resends the complete name on every delta would otherwise get
            // "search" + "search" -> "searchsearch". Pre-SMA-550 this shape
            // emitted "search" correctly, so concatenating unconditionally here
            // is a regression, not merely an unhandled edge.
            if !old.name.is_empty() && old.name != slot.name {
                if !slot.name.is_empty() {
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
                } else {
                    slot.name.push_str(&old.name);
                }
            }
            // Claimed whenever the migrating buffer is the older one, even if
            // its name was a repeat or empty: `seq` carries the call's wire
            // position for the end-of-stream flush order, independently of
            // whether any name text moved.
            if old.seq < slot.seq {
                slot.seq = old.seq;
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
        // A blank `id` is not an identity, so it must not become `Key::Id("")`
        // — two such entries would share one slot and merge into a single
        // call. Filtering it out here sends the delta to the same arms that
        // handle an absent id: positional keying, or the loud skip when
        // another entry in this array carries an explicit index. Registration
        // into `tool_calls` below still reads `tc.id` directly, so a blank id
        // is still recorded and still resolves to `""` (SMA-616).
        let key = match (tc.index, tc.id.as_deref().filter(|id| !id.is_empty())) {
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
            match self.tool_calls.get_mut(&key) {
                // First id wins, so a backend that changes a call's id
                // mid-stream cannot re-point an in-flight call. The one
                // exception is an id already recorded as empty: `canonicalize`
                // treats a blank id as "no identity yet" rather than as an
                // identity, so a real id arriving later must be allowed to
                // replace it — otherwise the blank sticks and the call reaches
                // the consumer under an empty `call_id` even though the backend
                // eventually supplied a real one.
                Some(existing) if existing.is_empty() && !id.is_empty() => {
                    *existing = id.to_owned();
                }
                Some(_) => {}
                None => {
                    self.tool_calls.insert(key.clone(), id.to_owned());
                }
            }
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
    /// entries whose resolved `call_id` already emitted a name. Since SMA-550
    /// the latter check is redundant — canonicalization gives each `call_id`
    /// one key — and is kept as a net; see the comment at its `continue`.
    fn flush_buffered_names(&mut self) -> Vec<ModelEvent> {
        // Sorted by buffer-creation order, not by `Key`. After SMA-550 every
        // resolved key is `Key::Id(call_id)`, so sorting by `Key` would mean
        // lexicographic-by-call_id — silently reordering parallel calls
        // against the wire. `seq` is unique per buffer, so this is a total,
        // deterministic order that matches first appearance.
        let mut keys: Vec<Key> = self.pending.keys().cloned().collect();
        keys.sort_by_key(|k| self.pending[k].seq);

        // Seeded from keys that already emitted, so a call flushed mid-stream
        // cannot be re-emitted here. The `.filter(|c| !c.is_empty())` is
        // redundant on its own: the loop guard further down
        // (`!call_id.is_empty() && !already.insert(...)`) already
        // short-circuits before `insert` ever runs for a blank id, so a blank
        // entry in `already` could never change what gets claimed. Kept anyway
        // so this seed states the same "blank ids are exempt" rule as the
        // guard below, in the same place — and so it matches
        // `providers-openai`'s seed, which states it identically (SMA-616).
        let mut already: HashSet<String> = self
            .name_emitted
            .keys()
            .filter_map(|k| self.tool_calls.get(k))
            .filter(|c| !c.is_empty())
            .cloned()
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
            // Claimed only once we know this key actually has a name to flush
            // — claiming earlier would, were two entries for one call_id ever
            // possible again, let an empty-name entry suppress another that
            // does have one.
            //
            // Unreachable for a non-blank call_id since SMA-550:
            // canonicalization gives each one exactly one pending key. Kept as
            // a net because it enforces the invariant at the point of
            // emission, independent of the keying discipline upstream — which
            // is precisely what the SMA-533 cross-provider suite asserts. Loud
            // rather than a bare `continue`: if the keying is ever loosened, a
            // silent drop here would recreate the exact undiagnosed loss
            // SMA-550 existed to fix.
            //
            // Blank call_ids are excluded deliberately. They bypass
            // canonicalization (an empty id cannot identify a call), so two
            // parallel blank-id calls both resolve to "". Claiming "" here
            // would drop the second call's name and blame a keying regression
            // that did not happen. The at-most-one invariant is therefore
            // scoped to non-blank call_ids; `providers-openai` carries the
            // same guard (SMA-566). Pinned by
            // `blank_ids_do_not_collapse_at_end_of_stream`.
            if !call_id.is_empty() && !already.insert(call_id.clone()) {
                tracing::error!(
                    target: "paigasus::litellm::stream",
                    %call_id,
                    ?key,
                    "two pending keys resolved to one call_id after \
                     canonicalization; dropping the second buffered name. This \
                     is a correlation-keying regression, not a backend quirk"
                );
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
            t.name_emitted
                .get(&Key::Id("c1".to_owned()))
                .map(String::as_str),
            Some("get_"),
            "the already-emitted name is not retroactively changed"
        );
        assert!(
            t.warned_late_name.contains(&Key::Id("c1".to_owned())),
            "the loss is recorded under the canonical key"
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

    /// One `call_id` reached under both `Key` variants yields exactly one
    /// name-carrying delta, and the fragments reassemble. Pre-fix this
    /// emitted `Some("get_")` and discarded `weather`; since SMA-550 both
    /// deltas share one canonical slot, so there is no second entry for the
    /// dedup set in `flush_buffered_names` to catch.
    #[test]
    fn one_call_id_under_two_keys_flushes_a_single_name() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "get_"}}
            ]}}]
        })));
        // No `index` -> would key as Key::Id("c1"); since SMA-550 that IS the
        // canonical key, so this joins the same slot rather than opening a
        // second entry for one call.
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
            ("c1".to_owned(), "get_weather".to_owned()),
            "the two keys are one slot after SMA-550, so the fragments reassemble"
        );
    }

    /// A call_id that already flushed its name mid-stream must not get a
    /// second name at end-of-stream.
    ///
    /// Since SMA-550 this holds because both deltas share one canonical slot,
    /// so `name_emitted` suppresses the second flush directly — not because
    /// of the `already` seed in `flush_buffered_names`, which this sequence no
    /// longer reaches. Kept because the invariant is what matters, not the
    /// mechanism that currently enforces it.
    #[test]
    fn flush_does_not_re_emit_a_name_already_flushed_under_another_key() {
        let mut t = ChatTranslator::new();
        // `id` is present alongside `index` on this delta, so `canonicalize`
        // resolves it immediately: the entry is recorded as
        // name_emitted[Key::Id("c1")] = "get_" from this first delta, not
        // under Key::Index(0) — there is no intermediate wire-keyed state to
        // observe here.
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "get_", "arguments": "{"}}
            ]}}]
        })));
        // No `index`, so this keys as Key::Id("c1") directly — the same
        // canonical slot the first delta already resolved to, which already
        // has an emitted name. Nothing is buffered: the fragment is dropped
        // on arrival by the canonical-keyed already-emitted short-circuit,
        // which also fires the `warned_late_name` warning for this key.
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
            "call_id \"c1\"'s late fragment must not re-emit a name: it already \
             flushed under the canonical Key::Id(\"c1\") from the first delta; \
             got {named:?}"
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
    /// `providers-openai`'s `chat.rs` emits the same single `alphabeta` here
    /// since SMA-566, via an index alias rather than this crate's `Key` enum —
    /// see the doc comment on its `handle_tool_call_chunk`. The two
    /// translators agree observably and differ structurally, because this
    /// crate's `index` is optional and `providers-openai`'s is required.
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

    /// A real `id` arriving after a blank one on the same wire key must win.
    ///
    /// Treating an empty id as "not an identity" is only half a policy: the
    /// registration in `tool_calls` is `or_insert`, so a blank id recorded
    /// first would stick, and every later delta for that call — including one
    /// carrying the real id — would keep resolving to `""`. The call would
    /// then reach the consumer under an empty `call_id` even though the
    /// backend eventually supplied a real one.
    #[test]
    fn a_real_id_replaces_a_blank_one_on_the_same_wire_key() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([{"index": 0, "id": "", "function": {"name": "foo"}}])),
                tc_chunk(
                    serde_json::json!([{"index": 0, "id": "c1", "function": {"arguments": "{}"}}]),
                ),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "foo".to_owned())],
            "the real id must replace the blank one, and the buffered name must \
             follow it onto the canonical key"
        );
    }

    /// A backend that resends the COMPLETE name on every delta must not have it
    /// doubled when two buffers merge across the key boundary.
    ///
    /// `canonicalize` originally concatenated the two names unconditionally,
    /// bypassing the SMA-547 whole-name-repeat guard the wire path applies
    /// (`slot.name != name_frag`). That was a regression against pre-SMA-550
    /// behaviour, which emitted `Some("search")` here, whereas the unguarded
    /// merge produced `Some("searchsearch")` — a name that resolves to no
    /// registered tool. Confirmed against `main` before the guard was added.
    #[test]
    fn repeated_whole_name_is_not_doubled_across_the_key_boundary() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([{"id": "c1", "function": {"name": "search"}}])),
                tc_chunk(serde_json::json!([{"index": 0, "function": {"name": "search"}}])),
                tc_chunk(
                    serde_json::json!([{"index": 0, "id": "c1", "function": {"arguments": "{}"}}]),
                ),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "search".to_owned())]);
    }

    /// A fragment migrating into a slot that already emitted its name cannot be
    /// recovered, so it must be reported rather than stranded.
    ///
    /// Before this guard it was left in `pending` under the canonical key,
    /// where `flush_buffered_names` skipped it (the call_id had already
    /// emitted) and `warn_unresolved_pending` ignored it (the key now
    /// resolves) — a silent loss with no diagnostic anywhere, which is exactly
    /// what SMA-550 set out to eliminate.
    #[test]
    fn fragment_migrating_into_an_emitted_slot_is_reported_not_stranded() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([{"index": 0, "function": {"name": "foo"}}])),
                tc_chunk(serde_json::json!([
                    {"id": "c1", "function": {"name": "bar", "arguments": "{}"}}
                ])),
                tc_chunk(
                    serde_json::json!([{"index": 0, "id": "c1", "function": {"arguments": "x"}}]),
                ),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "bar".to_owned())],
            "still at most one name-carrying delta per call_id"
        );
        assert!(
            t.warned_late_name.contains(&Key::Id("c1".to_owned())),
            "the dropped `foo` fragment must be recorded, not silently stranded"
        );
        assert!(
            t.pending
                .get(&Key::Id("c1".to_owned()))
                .is_none_or(|p| p.name.is_empty()),
            "the unrecoverable fragment must not be left sitting in `pending`"
        );
    }

    /// An empty `id` is not an identity: two parallel calls that both report
    /// `"id": ""` must stay distinct.
    ///
    /// Canonicalizing on a blank id collapsed them into one `Key::Id("")` slot
    /// and dropped the second call's name from the stream entirely — strictly
    /// worse than the dual-keying this change targets, and a regression against
    /// pre-SMA-550 behaviour, which emitted both. Confirmed against `main`.
    #[test]
    fn blank_ids_do_not_collapse_distinct_calls() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![tc_chunk(serde_json::json!([
                {"index": 0, "id": "", "function": {"name": "alpha", "arguments": "{}"}},
                {"index": 1, "id": "", "function": {"name": "beta", "arguments": "{}"}}
            ]))],
        );
        assert_eq!(
            named(&evs),
            vec![
                (String::new(), "alpha".to_owned()),
                (String::new(), "beta".to_owned()),
            ],
            "a blank id must not merge two distinct calls"
        );
    }

    /// Two parallel **zero-argument** blank-id calls must both emit their name
    /// at end-of-stream.
    ///
    /// Zero-argument is load-bearing: with arguments both calls flush
    /// mid-stream and never reach `flush_buffered_names`, so this is the only
    /// shape that exercises the `call_id` dedup net there —
    /// `blank_ids_do_not_collapse_distinct_calls` drives `"arguments": "{}"`
    /// and therefore covers the other path only.
    ///
    /// An unguarded net claims `""` for the first call and silently drops the
    /// second under an `error!` blaming a correlation-keying regression that
    /// did not happen. `openai/chat` carries the same guard (SMA-566).
    #[test]
    fn blank_ids_do_not_collapse_at_end_of_stream() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![tc_chunk(serde_json::json!([
                {"index": 0, "id": "", "function": {"name": "alpha"}},
                {"index": 1, "id": "", "function": {"name": "beta"}}
            ]))],
        );
        assert_eq!(
            named(&evs),
            vec![
                (String::new(), "alpha".to_owned()),
                (String::new(), "beta".to_owned()),
            ],
            "an empty id cannot identify a call, so the dedup net must not claim it"
        );
    }

    /// Blank-id calls with **no `index`** must not collide on one wire key.
    ///
    /// The wire key is `(tc.index, tc.id)`, and litellm's `index` is optional
    /// — so before SMA-616 an array of blank-id entries with no index gave
    /// every one of them `Key::Id("")`. They shared a single `Pending` slot,
    /// the SMA-547 whole-name-repeat guard did not fire (`"alpha" != "beta"`),
    /// and the two names concatenated into one `("", "alphabeta")` delta.
    ///
    /// This is a strictly deeper collision than the dedup net's: it happens at
    /// key construction, before `canonicalize` is ever called, so the
    /// `!call_id.is_empty()` guard in `flush_buffered_names` cannot reach it.
    /// The shape is impossible in `openai/chat`, whose `index` is a required
    /// `u32`.
    #[test]
    fn blank_ids_without_index_do_not_collapse() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![tc_chunk(serde_json::json!([
                {"id": "", "function": {"name": "alpha"}},
                {"id": "", "function": {"name": "beta"}}
            ]))],
        );
        assert_eq!(
            named(&evs),
            vec![
                (String::new(), "alpha".to_owned()),
                (String::new(), "beta".to_owned()),
            ],
            "a blank id is not an identity, so it must not become the wire key"
        );
    }
}
