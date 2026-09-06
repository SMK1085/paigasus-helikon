//! Chat Completions backend.
//!
//! Always streams (`stream: true` + `stream_options.include_usage: true` so
//! the final SSE chunk carries the full usage snapshot). The SSE stream is
//! translated by [`ChatTranslator`] into `ModelEvent`s.

use std::collections::{HashMap, HashSet};

use async_openai::types::chat::{
    ChatCompletionMessageToolCallChunk, ChatCompletionNamedToolChoice,
    ChatCompletionRequestMessage, ChatCompletionStreamOptions, ChatCompletionTool,
    ChatCompletionToolChoiceOption, ChatCompletionTools, CreateChatCompletionRequest,
    CreateChatCompletionRequestArgs, CreateChatCompletionStreamResponse,
    FinishReason as OaFinishReason, FunctionName, FunctionObject,
    ResponseFormat as OaResponseFormat, ToolChoiceOptions,
};
use async_stream::stream;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, FinishReason, ModelError, ModelEvent, ModelRequest, ToolChoice,
};

use crate::error::map_openai_error;
use crate::model::OpenAiModel;
use crate::translate::{
    request::to_chat_messages, response_format::to_openai_response_format, tools::to_strict_schema,
};

/// Entry point for Chat Completions backend. Always streams.
///
/// Builds a streaming Chat Completions request via async-openai (with
/// `stream_options.include_usage = true` so the final chunk carries
/// the full token-usage snapshot), then translates the SSE stream through
/// [`ChatTranslator`] into a `BoxStream<Result<ModelEvent, ModelError>>`.
///
/// Cancellation via [`CancellationToken`] is honoured at both the initial
/// request future and each poll of the upstream SSE stream (`tokio::select!`
/// biased toward the cancel arm).
pub(crate) async fn invoke(
    model: &OpenAiModel,
    request: ModelRequest,
    cancel: CancellationToken,
) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
    let body = build_request(model, &request, /* streaming */ true)?;
    let client = model.client.clone();

    let s = stream! {
        // `client.chat()` returns a `Chat<'_, C>` that borrows `client`.
        // We must bind it to a local so the borrow lives long enough for
        // `create_stream(body)` to be awaited.
        let chat_client = client.chat();
        let create_fut = chat_client.create_stream(body);

        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            r = create_fut => r,
        };

        let mut upstream = match response {
            Ok(s) => s,
            Err(e) => {
                yield Err(map_openai_error(e));
                return;
            }
        };

        let mut translator = ChatTranslator::new();
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                n = upstream.next() => n,
            };
            match next {
                None => {
                    // `async-openai`'s `create_stream` consumes `[DONE]`
                    // internally and ends iteration, so this is the single
                    // end-of-stream site.
                    for ev in translator.finish() {
                        yield Ok(ev);
                    }
                    return;
                }
                Some(Err(e)) => {
                    yield Err(map_openai_error(e));
                    return;
                }
                Some(Ok(chunk)) => {
                    for ev in translator.consume(chunk) {
                        yield Ok(ev);
                    }
                }
            }
        }
    };

    Ok(Box::pin(s))
}

/// Build the typed request for Chat Completions.
///
/// `streaming` controls whether `stream` + `stream_options.include_usage`
/// are set. In practice `invoke` always passes `streaming = true`; the
/// parameter exists for unit-testing the serialised request shape.
fn build_request(
    model: &OpenAiModel,
    request: &ModelRequest,
    streaming: bool,
) -> Result<CreateChatCompletionRequest, ModelError> {
    // Translate Item messages → typed async-openai messages via JSON round-trip.
    let messages_value = to_chat_messages(&request.messages);
    let messages: Vec<ChatCompletionRequestMessage> = serde_json::from_value(messages_value)
        .map_err(|e: serde_json::Error| ModelError::Other(anyhow::anyhow!(e)))?;

    let mut builder = CreateChatCompletionRequestArgs::default();
    builder.model(model.model_id.clone()).messages(messages);

    if streaming {
        builder.stream(true);
        builder.stream_options(ChatCompletionStreamOptions {
            include_usage: Some(true),
            include_obfuscation: None,
        });
    }

    // Tools: async-openai 0.41 uses `ChatCompletionTools::Function(ChatCompletionTool)`
    // as the wrapper enum; `ChatCompletionTool` holds just `function: FunctionObject`.
    if !request.tools.is_empty() {
        let tools: Vec<ChatCompletionTools> = request
            .tools
            .iter()
            .map(|td| {
                ChatCompletionTools::Function(ChatCompletionTool {
                    function: FunctionObject {
                        name: td.name.clone(),
                        description: Some(td.description.clone()),
                        parameters: Some(to_strict_schema(&td.schema)),
                        strict: Some(true),
                    },
                })
            })
            .collect();
        builder.tools(tools);
    }

    // ModelSettings passthrough.
    if let Some(t) = request.model_settings.temperature {
        builder.temperature(t);
    }
    if let Some(p) = request.model_settings.top_p {
        builder.top_p(p);
    }
    if let Some(m) = request.model_settings.max_output_tokens {
        builder.max_tokens(m);
    }
    if let Some(tc) = &request.model_settings.tool_choice {
        builder.tool_choice(translate_tool_choice(tc));
    }
    if let Some(rf) = &request.model_settings.response_format {
        if let Some(rf_value) = to_openai_response_format(rf) {
            // async-openai's `ResponseFormat` uses `#[serde(tag = "type",
            // rename_all = "snake_case")]`, which matches the JSON shape our
            // `to_openai_response_format` emits, so a serde round-trip works.
            let typed: OaResponseFormat = serde_json::from_value(rf_value)
                .map_err(|e: serde_json::Error| ModelError::Other(anyhow::anyhow!(e)))?;
            builder.response_format(typed);
        }
    }
    if request.model_settings.previous_response_id.is_some() {
        tracing::debug!(
            target: "paigasus::openai::chat",
            "previous_response_id is set but ignored on Chat Completions backend (Responses API only)"
        );
    }

    builder
        .build()
        .map_err(|e| ModelError::Other(anyhow::anyhow!(e)))
}

/// Translate a [`ToolChoice`] into async-openai's
/// [`ChatCompletionToolChoiceOption`].
///
/// In async-openai 0.41, the string variants (`"none"`, `"auto"`,
/// `"required"`) are wrapped in `ChatCompletionToolChoiceOption::Mode(
/// ToolChoiceOptions::*)`.
fn translate_tool_choice(tc: &ToolChoice) -> ChatCompletionToolChoiceOption {
    match tc {
        ToolChoice::Auto => ChatCompletionToolChoiceOption::Mode(ToolChoiceOptions::Auto),
        ToolChoice::Required => ChatCompletionToolChoiceOption::Mode(ToolChoiceOptions::Required),
        ToolChoice::None => ChatCompletionToolChoiceOption::Mode(ToolChoiceOptions::None),
        ToolChoice::Tool { name } => {
            ChatCompletionToolChoiceOption::Function(ChatCompletionNamedToolChoice {
                function: FunctionName { name: name.clone() },
            })
        }
        // ToolChoice is #[non_exhaustive]; new variants default to Auto.
        _ => ChatCompletionToolChoiceOption::Mode(ToolChoiceOptions::Auto),
    }
}

/// Buffered name and args for a tool call.
///
/// OpenAI's Chat Completions streaming spec does not strictly guarantee that
/// `tool_calls[].id` arrives before `function.name` or `function.arguments`
/// deltas for the same `index`, so both fields start out buffered here
/// regardless of whether the id is known yet. Once the id is known, `args`
/// is drain-once — taken on the first delta after the id is observed and
/// never re-prepended — while `name` keeps accumulating across every delta
/// and is cleared only when it flushes (SMA-547 §1).
///
/// Both `name` and `args` use `push_str` concatenation so that fragmented
/// deltas (e.g. `"sea"` + `"rch"` → `"search"`) are assembled correctly.
///
/// **No `Default` impl, deliberately.** Every buffer must carry the `seq` it
/// was created with, so construction goes through [`PendingToolCall::new`] via
/// [`ChatTranslator::ensure_pending`]. Deriving `Default` would let an
/// `.or_default()` call site silently mint a buffer with `seq: 0`, which would
/// corrupt the merge order in [`ChatTranslator::canonicalize`] (SMA-566). The
/// absence of the derive is what makes that a compile error rather than a
/// latent bug.
struct PendingToolCall {
    /// Monotonic creation order across all buffers in one stream.
    ///
    /// Its sole consumer is the merge in [`ChatTranslator::canonicalize`].
    /// It is deliberately **not** the end-of-stream flush order — that stays
    /// keyed on the canonical wire `index`, which is the model's declared
    /// call position. `providers-litellm` uses its `seq` for both because its
    /// `index` is optional and may be absent entirely; here it never is.
    seq: u64,
    name: String,
    args: String,
}

impl PendingToolCall {
    /// An empty buffer stamped with its creation order.
    fn new(seq: u64) -> Self {
        Self {
            seq,
            name: String::new(),
            args: String::new(),
        }
    }
}

/// Accumulates Chat Completions SSE deltas and emits [`ModelEvent`]s.
///
/// Maps upstream tool-call `index` values to their `call_id` once a first
/// delta with `id` arrives; subsequent deltas for the same index reuse the
/// stored `call_id`. One `call_id` owns exactly one correlation entry no
/// matter how many indexes resolve it — see [`Self::canonicalize`] for the
/// aliasing mechanism.
pub(crate) struct ChatTranslator {
    /// index → call_id after the first delta for that tool call.
    tool_calls: HashMap<u32, String>,
    /// Non-blank call_id → the wire index that owns its correlation state.
    ///
    /// The first index to resolve a given call_id becomes its owner; every
    /// later index resolving the same call_id aliases onto it, so one
    /// `call_id` owns exactly one entry in `pending`, `name_emitted` and
    /// `warned_late_name`. Blank ids are never inserted — see
    /// [`Self::canonicalize`].
    canonical: HashMap<String, u32>,
    /// index → the tool name already emitted to the consumer.
    ///
    /// Holds the *value*, not just the index, so a late fragment can be
    /// told apart from a backend that repeats the whole name on every
    /// delta (SMA-547 §3).
    name_emitted: HashMap<u32, String>,
    /// Indices for which the late-name-fragment warning has already fired,
    /// so a chatty backend cannot produce one warn per argument chunk.
    warned_late_name: HashSet<u32>,
    /// Wire indices for which the blank-id warning has already fired, so a
    /// backend that sends `"id": ""` on every delta warns once per call
    /// rather than once per chunk.
    warned_blank_id: HashSet<u32>,
    /// Wire indices that have already emitted a `ToolCallDelta` while their
    /// `call_id` was still blank.
    ///
    /// Gates the blank-id replacement rule below. Once a delta has gone out
    /// under `""`, upgrading the index to a real id would split one call
    /// across two `call_id`s and leave the real one with zero name-carrying
    /// deltas — an "exactly once" violation on a *non-blank* id, which is
    /// worse than the stuck blank this rule exists to fix (SMA-566).
    blank_emitted: HashSet<u32>,
    /// index → buffered name/args.
    ///
    /// `args` is drain-once: taken on the first delta after the call_id is
    /// known and never re-prepended. `name` accumulates across every delta
    /// for the call and is cleared only when the name flushes (SMA-547 §1).
    pending: HashMap<u32, PendingToolCall>,
    /// Next value handed out by [`Self::ensure_pending`]; never reused.
    next_seq: u64,
    /// Finish reason observed so far, emitted only by [`Self::finish`] at
    /// end-of-stream. Last observed value wins.
    finish_reason: Option<FinishReason>,
}

impl ChatTranslator {
    /// Create a fresh translator for a new streaming response.
    pub(crate) fn new() -> Self {
        Self {
            tool_calls: HashMap::new(),
            canonical: HashMap::new(),
            name_emitted: HashMap::new(),
            warned_late_name: HashSet::new(),
            warned_blank_id: HashSet::new(),
            blank_emitted: HashSet::new(),
            pending: HashMap::new(),
            next_seq: 0,
            finish_reason: None,
        }
    }

    /// Ensure a buffer exists for `index`, stamping a fresh `seq` on creation.
    ///
    /// Deliberately returns nothing rather than `&mut PendingToolCall`:
    /// callers then reach the buffer through `self.pending.get_mut(..)`, which
    /// borrows one field instead of all of `self` and so leaves the
    /// surrounding disjoint-field borrows of `name_emitted` and `tool_calls`
    /// intact.
    fn ensure_pending(&mut self, index: u32) {
        if !self.pending.contains_key(&index) {
            self.pending
                .insert(index, PendingToolCall::new(self.next_seq));
            self.next_seq += 1;
        }
    }

    /// Consume one upstream SSE chunk and produce zero or more [`ModelEvent`]s.
    ///
    /// `Usage` is emitted inline as it arrives. `Finish` is **never** emitted
    /// here — the finish reason is buffered and released by [`Self::finish`]
    /// at end-of-stream, because `usage` arrives on a chunk *after* the one
    /// carrying `finish_reason`. Only `Finish` is positionally constrained by
    /// the contract in `paigasus_helikon_core::Model::invoke`; `Usage` may
    /// appear anywhere.
    pub(crate) fn consume(&mut self, chunk: CreateChatCompletionStreamResponse) -> Vec<ModelEvent> {
        let mut out: Vec<ModelEvent> = Vec::new();

        for choice in &chunk.choices {
            // Text deltas.
            if let Some(content) = choice.delta.content.as_deref() {
                if !content.is_empty() {
                    out.push(ModelEvent::TokenDelta {
                        text: content.to_owned(),
                    });
                }
            }

            // Tool-call deltas.
            if let Some(tcs) = choice.delta.tool_calls.as_ref() {
                for tc in tcs {
                    self.handle_tool_call_chunk(tc, &mut out);
                }
            }

            // Buffer the finish reason — emitted by `finish()` at end-of-stream,
            // never inline, because `usage` arrives on a LATER chunk.
            if let Some(reason) = choice.finish_reason {
                let mapped = match reason {
                    OaFinishReason::Stop => FinishReason::Stop,
                    OaFinishReason::Length => FinishReason::Length,
                    OaFinishReason::ToolCalls => FinishReason::ToolCalls,
                    OaFinishReason::ContentFilter => FinishReason::ContentFilter,
                    OaFinishReason::FunctionCall => FinishReason::Other("function_call".to_owned()),
                    // OaFinishReason has no #[non_exhaustive] in 0.41 but guard for robustness.
                    #[allow(unreachable_patterns)]
                    other => FinishReason::Other(format!("{other:?}")),
                };
                if let Some(prev) = self.finish_reason.as_ref() {
                    if *prev != mapped {
                        tracing::debug!(
                            target: "paigasus::openai::chat",
                            previous = ?prev,
                            replacement = ?mapped,
                            "second distinct finish_reason observed; last wins"
                        );
                    }
                }
                self.finish_reason = Some(mapped);
            }
        }

        // Usage arrives on the final chunk (after `include_usage: true`) and
        // is emitted inline as it arrives. Finish is deferred to finish() at
        // end-of-stream — see this function's doc comment.
        if let Some(u) = chunk.usage.as_ref() {
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

    /// Emit any tool name still buffered at end-of-stream.
    ///
    /// Reached by the zero-argument shape, where no `arguments` fragment ever
    /// arrives to signal the name is complete. This is a correctness
    /// requirement, not a diagnostic: the agent loop dispatches on the
    /// presence of an `Item::ToolCall` and reads the tool to run from its
    /// `name`, so a name never emitted becomes an empty name that resolves to
    /// no tool.
    ///
    /// Only calls whose id resolved are flushed — an entry with no `call_id`
    /// has nothing to emit under. Emitted names are recorded, so a second
    /// call yields nothing.
    fn flush_buffered_names(&mut self) -> Vec<ModelEvent> {
        let mut indices: Vec<u32> = self.pending.keys().copied().collect();
        indices.sort_unstable();

        // Seeded from indexes that already emitted, so a call flushed
        // mid-stream cannot be re-emitted here under a second index. The
        // `.filter(|c| !c.is_empty())` below is redundant on its own: the loop
        // guard further down (`!call_id.is_empty() && !already.insert(...)`)
        // already short-circuits before `insert` ever runs for a blank id, so
        // a blank entry in `already` could never change what gets claimed.
        // Kept anyway so this seed states the same "blank ids are exempt"
        // rule as the guard below, in the same place — and so it matches
        // `providers-litellm`'s seed, which states it identically (SMA-616).
        let mut already: HashSet<String> = self
            .name_emitted
            .keys()
            .filter_map(|i| self.tool_calls.get(i))
            .filter(|c| !c.is_empty())
            .cloned()
            .collect();

        let mut out = Vec::new();
        for index in indices {
            if self.name_emitted.contains_key(&index) {
                continue;
            }
            let Some(call_id) = self.tool_calls.get(&index).cloned() else {
                continue;
            };
            let Some(entry) = self.pending.get_mut(&index) else {
                continue;
            };
            if entry.name.is_empty() {
                continue;
            }
            // Claimed only once this index is known to have a name to flush —
            // claiming earlier would let an empty-name entry suppress another
            // that does have one.
            //
            // Unreachable for a non-blank call_id since SMA-566: aliasing
            // gives each one exactly one pending index. Kept as a net because
            // it enforces the invariant at the point of emission, independent
            // of the keying discipline upstream — which is precisely what the
            // SMA-533 cross-provider suite asserts. Loud rather than a bare
            // `continue`: if the keying is ever loosened, a silent drop here
            // would recreate the exact undiagnosed loss SMA-550 existed to fix.
            //
            // Blank call_ids are excluded deliberately. They bypass
            // canonicalization (an empty id cannot identify a call), so two
            // parallel blank-id calls both resolve to "". Claiming "" here
            // would drop the second call's name and blame a keying regression
            // that did not happen. The at-most-one invariant is therefore
            // scoped to non-blank call_ids; `providers-litellm` carries the
            // same guard (SMA-616). Pinned by
            // `blank_ids_do_not_collapse_at_end_of_stream`.
            if !call_id.is_empty() && !already.insert(call_id.clone()) {
                tracing::error!(
                    target: "paigasus::openai::chat",
                    %call_id,
                    index,
                    "two pending indexes resolved to one call_id after \
                     canonicalization; dropping the second buffered name. This is \
                     a correlation-keying regression, not a backend quirk"
                );
                continue;
            }
            let name = std::mem::take(&mut entry.name);
            self.name_emitted.insert(index, name.clone());
            out.push(ModelEvent::ToolCallDelta {
                call_id,
                name: Some(name),
                args_delta: String::new(),
            });
        }
        out
    }

    /// Emit any buffered tool name, then the terminal `Finish`.
    ///
    /// Returns only the flushed names when no `finish_reason` was ever
    /// observed, so a truncated stream is never reported as a clean stop, and
    /// nothing at all when an earlier call already drained both buffers.
    /// `Finish` stays last, preserving core's terminal-event contract.
    pub(crate) fn finish(&mut self) -> Vec<ModelEvent> {
        let mut out = self.flush_buffered_names();
        let Some(reason) = self.finish_reason.take() else {
            tracing::debug!(
                target: "paigasus::openai::chat",
                "stream ended without a finish_reason; emitting no Finish"
            );
            return out;
        };
        out.push(ModelEvent::Finish { reason });
        out
    }

    /// Resolve the wire `index` to the canonical index owning `call_id`.
    ///
    /// The first index to resolve a given `call_id` owns its state; every
    /// later index resolving the same `call_id` aliases onto it. That is what
    /// makes "exactly one name-carrying `ToolCallDelta` per `call_id`" hold by
    /// construction rather than by guard (SMA-566).
    ///
    /// `providers-litellm` achieves the same property with a `Key { Index, Id }`
    /// enum, because its `index` is optional and one call can arrive under two
    /// different key spaces. Here `ChatCompletionMessageToolCallChunk::index`
    /// is a required `u32`, so there is only ever one key space and
    /// canonicalization is a many-to-one map *within* it. The two crates agree
    /// observably and differ structurally, for that reason.
    fn canonicalize(&mut self, index: u32, call_id: &str) -> u32 {
        // An empty `id` is not an identity. A backend that sends `"id": ""` on
        // every entry would otherwise collapse every one of its parallel calls
        // into a single slot, and all but the first would vanish from the
        // stream entirely — a strictly worse outcome than the dual-index
        // keying this function exists to fix. Leave such deltas on their wire
        // index, which keeps distinct calls distinct.
        if call_id.is_empty() {
            if self.warned_blank_id.insert(index) {
                tracing::warn!(
                    target: "paigasus::openai::chat",
                    index,
                    "tool-call delta carries an empty id; correlating by wire index \
                     instead, since an empty id cannot identify a call"
                );
            }
            return index;
        }

        let owner = *self.canonical.entry(call_id.to_owned()).or_insert(index);
        if owner == index {
            // The common path: this index owns the call. Every well-formed
            // stream takes it on every delta, and no migration ever runs.
            return index;
        }

        let Some(old) = self.pending.remove(&index) else {
            return owner;
        };
        self.ensure_pending(owner);
        let slot = self
            .pending
            .get_mut(&owner)
            .expect("ensure_pending just inserted this index");

        // A resolved slot drains `args` on every delta and is removed once
        // both fields are empty, so `slot.args` is always empty here — which
        // makes `insert_str(0, ..)` an assignment in practice rather than a
        // splice. The assert does not guard correctness: `insert_str(0, ..)`
        // stays order-preserving either way. It exists so that a future
        // relaxation of drain-once surfaces here for a deliberate
        // re-decision.
        debug_assert!(
            slot.args.is_empty(),
            "a resolved slot drains its args on every delta"
        );
        slot.args.insert_str(0, &old.args);

        // The canonical slot has already emitted its name, so this fragment
        // can never reach a consumer: the flush condition tests
        // `name_emitted` and will not fire again for this call. It must not be
        // left sitting in `pending` either — `flush_buffered_names` skips
        // entries whose index already emitted. Dropping it silently would
        // recreate the undiagnosed loss SMA-550 exists to eliminate, so drop
        // it loudly and record it the same way a late wire fragment is
        // recorded.
        if !old.name.is_empty() {
            if let Some(emitted) = self.name_emitted.get(&owner) {
                if self.warned_late_name.insert(owner) {
                    tracing::warn!(
                        target: "paigasus::openai::chat",
                        %call_id,
                        fragment = %old.name,
                        emitted = %emitted,
                        "tool-call name fragment buffered under another wire index \
                         arrived after the name was emitted; it cannot be recovered \
                         and is dropped"
                    );
                }
                // Write-only here: once `name_emitted[owner]` is set,
                // `slot.name` can never merge again, so this particular
                // `seq` is never read afterwards. It is defensive symmetry
                // with the tail copy below (which mutation testing proved
                // live), not a necessity of this branch — kept so the two
                // copies stay identical rather than one silently drifting.
                if old.seq < slot.seq {
                    slot.seq = old.seq;
                }
                return owner;
            }
        }

        // A migrating fragment identical to what the canonical slot already
        // holds is a whole-name repeat, not a continuation — the same case the
        // wire path guards, for the same reason: a backend that resends the
        // complete name under a second index would otherwise get
        // "search" + "search" -> "searchsearch".
        if !old.name.is_empty() && old.name != slot.name {
            if !slot.name.is_empty() {
                tracing::warn!(
                    target: "paigasus::openai::chat",
                    %call_id,
                    "tool-call name fragments for one call arrived under two wire \
                     indexes; merging in buffer-creation order, which may misorder \
                     them if the two indexes interleaved"
                );
            }
            // Order by creation `seq`, not by which buffer is migrating. Both
            // orderings are reachable, and a plain prepend or a plain append
            // is wrong in exactly one of them.
            if old.seq < slot.seq {
                slot.name.insert_str(0, &old.name);
            } else {
                slot.name.push_str(&old.name);
            }
        }
        // Claimed whenever the migrating buffer is the older one, even if its
        // name was a repeat or empty: `seq` must stay accurate for any
        // subsequent merge into this same slot.
        if old.seq < slot.seq {
            slot.seq = old.seq;
        }
        owner
    }

    /// Correlate one tool-call delta and emit any completed name/args.
    ///
    /// **Aligned with `providers-litellm` since SMA-566; the implementations
    /// differ, the behaviour does not.** Both translators guarantee exactly
    /// one name-carrying `ToolCallDelta` per non-blank `call_id`, and both
    /// merge the malformed shape — two deltas carrying different `index`
    /// values but the same `id` — into a single call rather than emitting two
    /// names.
    ///
    /// They reach it differently, for a reason rooted in the wire. litellm's
    /// `index` is optional, so one call can arrive under two key *spaces*, and
    /// it canonicalizes with a `Key { Index(u32), Id(String) }` enum. Here
    /// `ChatCompletionMessageToolCallChunk::index` is a required `u32`, so
    /// there is exactly one key space and canonicalization is a many-to-one
    /// map within it: [`Self::canonicalize`] aliases every index resolving a
    /// given `call_id` onto the first index that owned it. Keeping the `u32`
    /// key is what lets `flush_buffered_names` go on sorting by the model's
    /// declared call position rather than by a synthetic creation counter.
    ///
    /// The one remaining asymmetry is deliberate and ticketed: this crate
    /// gates the blank→real `call_id` upgrade on `blank_emitted`, so a call
    /// that has already emitted under `""` keeps the blank rather than
    /// splitting across two ids; litellm upgrades unconditionally (SMA-619).
    /// The end-of-stream dedup net is no longer asymmetric — both crates
    /// exempt blank `call_id`s from it (SMA-616).
    fn handle_tool_call_chunk(
        &mut self,
        tc: &ChatCompletionMessageToolCallChunk,
        out: &mut Vec<ModelEvent>,
    ) {
        let index = tc.index;
        // Both signals test the post-`unwrap_or("")` effective fragment, so a
        // backend sending `"name": ""` behaves like one omitting the field.
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

        // Captured before the match so the guard below reads a plain `bool`
        // rather than borrowing `self` while `tool_calls` is borrowed mutably.
        let blank_already_emitted = self.blank_emitted.contains(&index);

        // Register or replace the call_id for this wire index.
        if let Some(id) = tc.id.as_deref() {
            match self.tool_calls.get_mut(&index) {
                // First id wins, so a backend that changes a call's id
                // mid-stream cannot re-point an in-flight call. The one
                // exception is an id already recorded as empty: `canonicalize`
                // treats a blank id as "no identity yet" rather than as an
                // identity, so a real id arriving later must be allowed to
                // replace it — otherwise the blank sticks and the call reaches
                // the consumer under an empty `call_id` even though the
                // backend eventually supplied a real one.
                //
                // The upgrade is withheld once this index has already emitted
                // a delta under the blank id. Replacing then would split one
                // call across two `call_id`s: the name would have gone out
                // under `""` and every later delta under the real id, leaving
                // the real id with zero name-carrying deltas. That is an
                // "exactly once" violation on a *non-blank* `call_id` — worse
                // than the stuck blank, and one the pre-SMA-566 translator did
                // not have. Keeping the blank keeps the call whole.
                Some(existing)
                    if existing.is_empty() && !id.is_empty() && !blank_already_emitted =>
                {
                    *existing = id.to_owned();
                }
                Some(_) => {}
                None => {
                    self.tool_calls.insert(index, id.to_owned());
                }
            }
        }

        let Some(call_id) = self.tool_calls.get(&index).cloned() else {
            // No call_id yet — buffer both fields so neither is dropped.
            self.ensure_pending(index);
            let entry = self
                .pending
                .get_mut(&index)
                .expect("ensure_pending just inserted this index");
            entry.name.push_str(name_frag);
            entry.args.push_str(args_frag);
            return;
        };

        // From here on, one call_id owns exactly one state entry.
        let index = self.canonicalize(index, &call_id);

        // A name fragment arriving after the name was emitted cannot be
        // recovered — the event is already downstream. Warn once per call,
        // and not at all when the fragment merely repeats what was emitted.
        if let Some(emitted) = self.name_emitted.get(&index) {
            if !name_frag.is_empty()
                && !emitted.starts_with(name_frag)
                && self.warned_late_name.insert(index)
            {
                tracing::warn!(
                    target: "paigasus::openai::chat",
                    %call_id,
                    fragment = %name_frag,
                    emitted = %emitted,
                    "tool-call name fragment arrived after the name was emitted; \
                     it cannot be recovered and is dropped"
                );
            }
        }

        // Once a name has flushed, nothing reads `entry.name` again for the
        // life of the stream — skip the accumulation so a backend that
        // repeats the whole name on every delta doesn't grow an unread
        // `String` for no reason (SMA-547 §4). Captured from the same
        // `name_emitted` state the flush condition below re-derives, so it
        // cannot change which deltas flush.
        let already_emitted = self.name_emitted.contains_key(&index);

        self.ensure_pending(index);
        let entry = self
            .pending
            .get_mut(&index)
            .expect("ensure_pending just inserted this index");
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
        if !already_emitted && entry.name != name_frag {
            entry.name.push_str(name_frag);
        }
        // `args` is drain-once: buffered pre-id args are prepended exactly
        // once and never re-emitted on later deltas.
        let mut args_out = std::mem::take(&mut entry.args);
        args_out.push_str(args_frag);

        // The name is complete when either signal appears: this delta carried
        // arguments, or this delta carried no name fragment. Note this tests
        // `args_frag` (this delta's own contribution), not `args_out`.
        let flush = !self.name_emitted.contains_key(&index)
            && !entry.name.is_empty()
            && (!args_frag.is_empty() || name_frag.is_empty());

        let name_to_emit = if flush {
            let name = std::mem::take(&mut entry.name);
            self.name_emitted.insert(index, name.clone());
            Some(name)
        } else {
            None
        };

        if entry.name.is_empty() && entry.args.is_empty() {
            self.pending.remove(&index);
        }

        // Suppress a wholly empty event. This tests `args_out`, NOT
        // `args_frag` — testing the latter would discard buffered pre-id
        // arguments on a bare id-carrying delta.
        if name_to_emit.is_none() && args_out.is_empty() {
            return;
        }

        // Record that this index has emitted under a blank id, so the
        // replacement rule above cannot later split the call in two.
        if call_id.is_empty() {
            self.blank_emitted.insert(index);
        }

        out.push(ModelEvent::ToolCallDelta {
            call_id,
            name: name_to_emit,
            args_delta: args_out,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::chat::{ChatCompletionMessageToolCallChunk, FunctionCallStream};

    fn make_chunk(
        index: u32,
        id: Option<&str>,
        name: Option<&str>,
        arguments: Option<&str>,
    ) -> ChatCompletionMessageToolCallChunk {
        ChatCompletionMessageToolCallChunk {
            index,
            id: id.map(|s| s.to_owned()),
            r#type: None,
            function: Some(FunctionCallStream {
                name: name.map(|s| s.to_owned()),
                arguments: arguments.map(|s| s.to_owned()),
            }),
        }
    }

    /// Drive every chunk through `handle_tool_call_chunk`, then `finish`,
    /// collecting all events.
    ///
    /// Collecting across both is required: the invariant is per-stream, and an
    /// assertion made only over `finish()` passes vacuously against the
    /// pre-fix translator, whose violation is two *mid-stream* emissions.
    fn drive(
        t: &mut ChatTranslator,
        chunks: Vec<ChatCompletionMessageToolCallChunk>,
    ) -> Vec<ModelEvent> {
        let mut out = Vec::new();
        for c in chunks {
            t.handle_tool_call_chunk(&c, &mut out);
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

    /// Concatenated `args_delta` across every delta carrying `want`, in order.
    fn args_of(evs: &[ModelEvent], want: &str) -> String {
        evs.iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    args_delta,
                    ..
                } if call_id == want => Some(args_delta.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Chunk 1: name arrives without an id.
    /// Chunk 2: id arrives; name should be recovered from the buffer.
    #[test]
    fn orphan_name_buffered_and_flushed_with_id() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();

        // Chunk 1: name="foo", no id — should be buffered, nothing emitted.
        t.handle_tool_call_chunk(&make_chunk(0, None, Some("foo"), None), &mut out);
        assert!(out.is_empty(), "no event expected before id arrives");

        // Chunk 2: id="call_abc", name=None, args="{}" — id arrives, flush buffer.
        t.handle_tool_call_chunk(&make_chunk(0, Some("call_abc"), None, Some("{}")), &mut out);
        assert_eq!(out.len(), 1, "expected exactly one ToolCallDelta");
        match &out[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "call_abc");
                assert_eq!(
                    name.as_deref(),
                    Some("foo"),
                    "buffered name must be emitted"
                );
                assert_eq!(args_delta, "{}");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }

        // Subsequent chunk: name should NOT be re-emitted.
        let mut out2 = Vec::new();
        t.handle_tool_call_chunk(&make_chunk(0, None, None, Some("extra")), &mut out2);
        assert_eq!(out2.len(), 1);
        match &out2[0] {
            ModelEvent::ToolCallDelta { name, .. } => {
                assert!(name.is_none(), "name must not be re-emitted");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }
    }

    /// Chunk 1: first name fragment ("sea"), no id.
    /// Chunk 2: second name fragment ("rch"), still no id.
    /// Chunk 3: id arrives; both name fragments must be concatenated ("search").
    #[test]
    fn orphan_name_fragments_concatenate_before_id() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();

        // Chunk 1: name fragment "sea", no id — buffer, no emission.
        t.handle_tool_call_chunk(&make_chunk(0, None, Some("sea"), None), &mut out);
        assert!(out.is_empty(), "no emission until id arrives");

        // Chunk 2: name fragment "rch", still no id — append to buffer.
        t.handle_tool_call_chunk(&make_chunk(0, None, Some("rch"), None), &mut out);
        assert!(out.is_empty(), "no emission until id arrives");

        // Chunk 3: id arrives with a first args fragment; flush buffer.
        t.handle_tool_call_chunk(&make_chunk(0, Some("c1"), None, Some("{")), &mut out);
        assert_eq!(out.len(), 1, "expected exactly one ToolCallDelta");
        match &out[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(
                    name.as_deref(),
                    Some("search"),
                    "fragmented name must be concatenated"
                );
                assert_eq!(args_delta, "{");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }
    }

    /// Chunk 1: first name fragment ("sea"), no id.
    /// Chunk 2: id arrives AND carries a name continuation ("rch").
    /// Both fragments must be concatenated in order → "search".
    ///
    /// Since SMA-547 the name is held while fragments keep arriving, so the
    /// emission moves to `finish()` — neither delta carries arguments, and
    /// both carry a name fragment, so no mid-stream signal ever says the name
    /// is complete.
    #[test]
    fn orphan_name_concatenates_with_id_bearing_name() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();

        t.handle_tool_call_chunk(&make_chunk(0, None, Some("sea"), None), &mut out);
        assert!(out.is_empty(), "no emission until id arrives");

        t.handle_tool_call_chunk(&make_chunk(0, Some("c1"), Some("rch"), None), &mut out);
        assert!(out.is_empty(), "name may still be growing");

        let fin = t.finish();
        assert_eq!(fin.len(), 1, "flushed name only; no finish_reason was seen");
        match &fin[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(
                    name.as_deref(),
                    Some("search"),
                    "buffered + id-chunk name must be concatenated"
                );
                assert_eq!(args_delta, "");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// SMA-547: a name fragmented across deltas that arrive AFTER the id
    /// resolves must be assembled, not truncated at the first fragment.
    ///
    /// Confirmed to FAIL against the pre-fix code: the `name_emitted` guard
    /// suppressed "weather", yielding a tool named `get_`.
    #[test]
    fn name_fragments_after_id_are_assembled() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();

        // Delta 1: id + first name fragment, empty args → held.
        t.handle_tool_call_chunk(
            &make_chunk(0, Some("call_abc"), Some("get_"), Some("")),
            &mut out,
        );
        assert!(out.is_empty(), "name is incomplete; nothing to emit yet");

        // Delta 2: second name fragment, still empty args → still held.
        t.handle_tool_call_chunk(&make_chunk(0, None, Some("weather"), Some("")), &mut out);
        assert!(out.is_empty(), "name may still be growing");

        // Delta 3: args arrive, no name fragment → the name is complete.
        t.handle_tool_call_chunk(
            &make_chunk(0, None, None, Some("{\"city\":\"Berlin\"}")),
            &mut out,
        );
        assert_eq!(out.len(), 1, "expected exactly one ToolCallDelta");
        match &out[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "call_abc");
                assert_eq!(
                    name.as_deref(),
                    Some("get_weather"),
                    "fragments must concatenate"
                );
                assert_eq!(args_delta, "{\"city\":\"Berlin\"}");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }
    }

    /// Chunk 1: args arrive without an id.
    /// Chunk 2: id arrives; args should be prepended.
    #[test]
    fn orphan_args_buffered_and_prepended_with_id() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();

        t.handle_tool_call_chunk(&make_chunk(0, None, None, Some("{\"a\":")), &mut out);
        assert!(out.is_empty());

        t.handle_tool_call_chunk(
            &make_chunk(0, Some("call_xyz"), Some("bar"), Some("1}")),
            &mut out,
        );
        assert_eq!(out.len(), 1);
        match &out[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "call_xyz");
                assert_eq!(name.as_deref(), Some("bar"));
                assert_eq!(args_delta, "{\"a\":1}");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }
    }

    /// Build a stream chunk from raw JSON, so tests state the wire shape
    /// directly rather than constructing async-openai types field by field.
    fn stream_chunk(json: &str) -> CreateChatCompletionStreamResponse {
        serde_json::from_str(json).expect("fixture chunk must deserialize")
    }

    #[test]
    fn finish_is_emitted_only_at_end_of_stream() {
        let mut t = ChatTranslator::new();

        let evs = t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        ));
        assert!(
            !evs.iter().any(|e| matches!(e, ModelEvent::Finish { .. })),
            "consume must not emit Finish inline, got {evs:?}"
        );

        let fin = t.finish();
        assert_eq!(fin.len(), 1, "expected exactly one Finish, got {fin:?}");
        assert!(matches!(
            &fin[0],
            ModelEvent::Finish {
                reason: FinishReason::Stop
            }
        ));
    }

    #[test]
    fn repeated_finish_reasons_yield_one_finish_last_wins() {
        let mut t = ChatTranslator::new();

        let first = t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        ));
        assert!(
            !first.iter().any(|e| matches!(e, ModelEvent::Finish { .. })),
            "consume must not emit Finish inline, got {first:?}"
        );
        let mid = t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":1,"delta":{"content":"x"}}]}"#,
        ));
        assert!(
            mid.iter()
                .any(|e| matches!(e, ModelEvent::TokenDelta { .. })),
            "expected the interleaved TokenDelta, got {mid:?}"
        );
        let last = t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":1,"delta":{},"finish_reason":"length"}]}"#,
        ));
        assert!(
            !last.iter().any(|e| matches!(e, ModelEvent::Finish { .. })),
            "consume must not emit Finish inline, got {last:?}"
        );

        let fin = t.finish();
        assert_eq!(
            fin.len(),
            1,
            "exactly one Finish per stream, not one per chunk; got {fin:?}"
        );
        assert!(
            matches!(
                &fin[0],
                ModelEvent::Finish {
                    reason: FinishReason::Length
                }
            ),
            "last observed finish_reason must win, got {fin:?}"
        );
    }

    #[test]
    fn truncated_stream_emits_no_finish() {
        let mut t = ChatTranslator::new();
        t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":0,"delta":{"content":"partial"}}]}"#,
        ));
        assert!(
            t.finish().is_empty(),
            "a stream with no finish_reason must not report a clean stop"
        );
    }

    #[test]
    fn finish_is_idempotent_after_draining() {
        let mut t = ChatTranslator::new();
        t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        ));
        assert_eq!(t.finish().len(), 1);
        assert!(
            t.finish().is_empty(),
            "finish() takes the buffer; a second call must yield nothing"
        );
    }

    /// A single delta carrying id, name and non-empty args emits immediately —
    /// the args signal means the name cannot still be growing.
    #[test]
    fn complete_single_delta_emits_name_with_no_added_latency() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();
        t.handle_tool_call_chunk(
            &make_chunk(0, Some("c1"), Some("get_time"), Some("{}")),
            &mut out,
        );
        assert_eq!(out.len(), 1);
        match &out[0] {
            ModelEvent::ToolCallDelta {
                name, args_delta, ..
            } => {
                assert_eq!(name.as_deref(), Some("get_time"));
                assert_eq!(args_delta, "{}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// A zero-argument call whose sole delta carries no arguments at all has
    /// no mid-stream completion signal; the name flushes from finish(),
    /// ordered BEFORE Finish.
    #[test]
    fn zero_argument_call_flushes_name_before_finish() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();
        t.handle_tool_call_chunk(&make_chunk(0, Some("c1"), Some("ping"), None), &mut out);
        assert!(out.is_empty(), "no completion signal yet");

        t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ));

        let fin = t.finish();
        assert_eq!(fin.len(), 2, "flushed name then Finish");
        match &fin[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(name.as_deref(), Some("ping"));
                assert_eq!(args_delta, "");
            }
            other => panic!("expected the flushed name first, got {other:?}"),
        }
        assert!(
            matches!(fin[1], ModelEvent::Finish { .. }),
            "Finish must stay terminal, got {:?}",
            fin[1]
        );
    }

    /// A truncated stream still flushes the name — the tool call is real, and
    /// the caller learns of the truncation from the ABSENT Finish, not from a
    /// missing name.
    #[test]
    fn truncated_stream_flushes_name_but_emits_no_finish() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();
        t.handle_tool_call_chunk(&make_chunk(0, Some("c1"), Some("ping"), None), &mut out);

        let fin = t.finish();
        assert_eq!(fin.len(), 1, "the flushed name, and no Finish");
        assert!(matches!(
            &fin[0],
            ModelEvent::ToolCallDelta { name: Some(n), .. } if n == "ping"
        ));
    }

    /// finish() takes its buffers, so a second call is a no-op.
    #[test]
    fn finish_name_flush_is_idempotent() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();
        t.handle_tool_call_chunk(&make_chunk(0, Some("c1"), Some("ping"), None), &mut out);
        assert_eq!(t.finish().len(), 1);
        assert!(
            t.finish().is_empty(),
            "a second finish() must yield nothing"
        );
    }

    /// Buffered pre-id args followed by a bare id-carrying delta must still
    /// emit those args. Testing the emit-nothing guard against this delta's
    /// own fragment (rather than the combined value) would swallow them.
    #[test]
    fn buffered_args_survive_a_bare_id_delta() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();

        t.handle_tool_call_chunk(&make_chunk(0, None, None, Some("{\"a\":1}")), &mut out);
        assert!(out.is_empty(), "buffered until the id arrives");

        // Bare id: no name, no args.
        t.handle_tool_call_chunk(&make_chunk(0, Some("c1"), None, None), &mut out);
        assert_eq!(out.len(), 1, "the buffered args must not be swallowed");
        match &out[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(name.as_deref(), None, "no name fragment was ever sent");
                assert_eq!(args_delta, "{\"a\":1}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// A late fragment cannot be recovered, but a backend that merely repeats
    /// the whole name on every delta is not an error and must not warn.
    #[test]
    fn repeated_whole_name_is_not_treated_as_a_late_fragment() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();

        t.handle_tool_call_chunk(
            &make_chunk(0, Some("c1"), Some("search"), Some("{\"q\":")),
            &mut out,
        );
        t.handle_tool_call_chunk(&make_chunk(0, None, Some("search"), Some("1}")), &mut out);

        assert_eq!(out.len(), 2);
        match &out[1] {
            ModelEvent::ToolCallDelta {
                name, args_delta, ..
            } => {
                assert_eq!(name.as_deref(), None, "name is emitted once per call");
                assert_eq!(args_delta, "1}");
            }
            other => panic!("unexpected: {other:?}"),
        }
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
        let mut out = Vec::new();

        // Delta 1: id + whole name, empty args -> held, no completion signal.
        t.handle_tool_call_chunk(
            &make_chunk(0, Some("c1"), Some("search"), Some("")),
            &mut out,
        );
        assert!(out.is_empty(), "no completion signal yet");

        // Delta 2: the SAME whole name repeated, now with args -> flush.
        t.handle_tool_call_chunk(&make_chunk(0, None, Some("search"), Some("{")), &mut out);
        assert_eq!(out.len(), 1, "expected exactly one ToolCallDelta");
        match &out[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(
                    name.as_deref(),
                    Some("search"),
                    "a repeated whole name must not be doubled to \"searchsearch\""
                );
                assert_eq!(args_delta, "{");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }
    }

    /// Companion guard on the fix above: distinct fragments arriving before
    /// any arguments must still concatenate -- this is SMA-547's actual
    /// target case, and the repeat-suppression fix must not over-suppress
    /// it.
    #[test]
    fn distinct_fragments_before_any_arguments_still_concatenate() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();

        t.handle_tool_call_chunk(&make_chunk(0, Some("c1"), Some("get_"), Some("")), &mut out);
        assert!(out.is_empty(), "no completion signal yet");

        t.handle_tool_call_chunk(&make_chunk(0, None, Some("weather"), Some("{}")), &mut out);
        assert_eq!(out.len(), 1, "expected exactly one ToolCallDelta");
        match &out[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(
                    name.as_deref(),
                    Some("get_weather"),
                    "distinct fragments must still concatenate"
                );
                assert_eq!(args_delta, "{}");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }
    }

    /// A genuine late fragment is dropped (it cannot be un-emitted) but is
    /// recorded so the loss is visible, and only once per call.
    #[test]
    fn late_name_fragment_warns_once() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();

        t.handle_tool_call_chunk(
            &make_chunk(0, Some("c1"), Some("get_"), Some("{\"a\":")),
            &mut out,
        );
        t.handle_tool_call_chunk(&make_chunk(0, None, Some("weather"), Some("1")), &mut out);
        t.handle_tool_call_chunk(&make_chunk(0, None, Some("weather"), Some("}")), &mut out);

        assert!(t.warned_late_name.contains(&0), "the loss must be recorded");
        assert_eq!(t.warned_late_name.len(), 1, "at most one warning per call");
        assert_eq!(
            t.name_emitted.get(&0).map(String::as_str),
            Some("get_"),
            "the already-emitted name is not retroactively changed"
        );
    }

    /// SMA-566 acceptance criterion: two deltas carrying different `index`
    /// values but the same `id` are one call, and yield one name.
    ///
    /// The merged name `alphabeta` is not "correct" in any deep sense — the
    /// input is malformed, since an `id` identifies a call — but it is one
    /// name for one `call_id`, which is the invariant under test, and it is
    /// character-for-character what `providers-litellm` emits for this input
    /// (`stream.rs`'s `two_indexes_with_one_id_merge_into_a_single_call`).
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `Some("beta")` from `handle_tool_call_chunk` and then `Some("alpha")`
    /// from `finish`.
    #[test]
    fn two_indexes_with_one_id_merge_into_a_single_call() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some("c1"), Some("alpha"), None),
                make_chunk(1, Some("c1"), Some("beta"), Some("{}")),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "alphabeta".to_owned())],
            "exactly one name-carrying delta per call_id, across both entry points"
        );
        assert_eq!(
            args_of(&evs, "c1"),
            "{}",
            "merging the names must not swallow the args"
        );
    }

    /// The same invariant with the flush happening mid-stream rather than at
    /// `finish`, so the test cannot pass for the wrong reason.
    ///
    /// The expected name is `get_`, **not** `get_weather`: the first delta
    /// carries arguments, so the name flushes immediately and is already
    /// downstream when the second index aliases in. The migrating fragment
    /// cannot be recovered (design spec §4 row 1b).
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `Some("get_")` and then `Some("weather")`.
    #[test]
    fn dual_index_call_emits_at_most_one_name_mid_stream() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some("c1"), Some("get_"), Some("{")),
                make_chunk(1, Some("c1"), Some("weather"), Some("}")),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "get_".to_owned())]);
        assert_eq!(
            args_of(&evs, "c1"),
            "{}",
            "suppressing the second name must not swallow its args"
        );
    }

    /// A blank `id` is not an identity. Canonicalizing on it would collapse
    /// every parallel blank-id call into one slot and all but the first would
    /// vanish — strictly worse than the defect being fixed. Such deltas stay
    /// on their wire index instead.
    ///
    /// Passes against the pre-fix translator too: index keying already keeps
    /// them distinct. This is a regression guard for the alias map, not a
    /// defect proof.
    #[test]
    fn blank_ids_do_not_collapse_distinct_calls() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some(""), Some("alpha"), Some("{}")),
                make_chunk(1, Some(""), Some("beta"), Some("{}")),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![
                (String::new(), "alpha".to_owned()),
                (String::new(), "beta".to_owned()),
            ],
            "two blank-id calls must both emit; an empty id cannot merge them"
        );
    }

    /// A fragment buffered under a second index, before that index's `id`
    /// resolved, must survive the alias rather than being stranded.
    ///
    /// This is the *prepend* branch of the merge: the migrating buffer was
    /// created first, so its fragment belongs in front. Its mirror is
    /// `owner_index_buffered_first_appends_on_merge`; a naive unconditional
    /// append yields `"alphabeta"` here.
    ///
    /// Confirmed to FAIL against the translator as it stood on `main` before
    /// SMA-566, which emits `Some("beta")` and then `Some("alpha")`.
    #[test]
    fn fragment_buffered_under_a_second_index_is_not_stranded() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(1, None, Some("beta"), Some("{\"x\":")),
                make_chunk(0, Some("c1"), Some("alpha"), None),
                make_chunk(1, Some("c1"), None, Some("1}")),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "betaalpha".to_owned())]);
        assert_eq!(
            args_of(&evs, "c1"),
            "{\"x\":1}",
            "args buffered under the non-owner index before its call_id resolved \
             must survive the alias onto the owning index, not be truncated"
        );
    }

    /// The *append* branch, mirror of the test above: here the canonical slot
    /// was created first, so the migrating fragment belongs behind it. A naive
    /// unconditional prepend yields `"betaalpha"` here.
    ///
    /// Together these two are why `PendingToolCall` carries a `seq`: both
    /// orderings are reachable, and a plain prepend or a plain append is wrong
    /// in exactly one of them.
    ///
    /// Confirmed to FAIL against the translator as it stood on `main` before
    /// SMA-566, which emits `Some("beta")` and then `Some("alpha")`.
    #[test]
    fn owner_index_buffered_first_appends_on_merge() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some("c1"), Some("alpha"), None),
                make_chunk(1, None, Some("beta"), None),
                make_chunk(1, Some("c1"), None, Some("{}")),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "alphabeta".to_owned())]);
    }

    /// A migrating fragment identical to what the canonical slot already holds
    /// is a whole-name repeat, not a continuation — the same case the SMA-547
    /// wire-path guard handles, for the same reason. Without this, a backend
    /// that resends the complete name under a second index yields
    /// `"searchsearch"`, which resolves to no registered tool.
    ///
    /// Confirmed to FAIL against the translator as it stood on `main` before
    /// SMA-566, which emits `Some("search")` twice.
    #[test]
    fn repeated_whole_name_is_not_doubled_across_the_alias_boundary() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(1, None, Some("search"), None),
                make_chunk(0, Some("c1"), Some("search"), None),
                make_chunk(1, Some("c1"), None, Some("{}")),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "search".to_owned())]);
    }

    /// Pins `slot.seq = old.seq` in `canonicalize`. When the migrating buffer
    /// is the older one, the canonical slot must inherit its creation order —
    /// otherwise a *third* index aliasing into the same call merges on the
    /// wrong side.
    ///
    /// Without that line the third merge prepends instead of appending and
    /// yields `"GBAx"`.
    ///
    /// Confirmed to FAIL against the translator as it stood on `main` before
    /// SMA-566, which emits `Some("G")`, `Some("A")` and `Some("Bx")` — three
    /// names for one call_id.
    #[test]
    fn migrated_buffer_donates_its_creation_order() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(1, None, Some("B"), None),
                make_chunk(2, None, Some("G"), None),
                make_chunk(0, Some("c1"), Some("A"), None),
                make_chunk(1, Some("c1"), Some("x"), None),
                make_chunk(2, Some("c1"), None, Some("{}")),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "BAxG".to_owned())]);
    }

    /// The accepted residual: when two indexes for one call interleave at
    /// *fragment* level, no buffer-level order reconstructs the wire sequence,
    /// so the merge misorders the fragments — though it loses none of them.
    ///
    /// Wire order is `AA BB CC DD`; the merge yields `AADDBBCC`. This is
    /// pinned deliberately rather than left undefined, matching
    /// `providers-litellm`'s `interleaved_dual_keying_is_lossless_and_misordered`.
    /// It is still strictly better than the pre-fix translator, which emits
    /// two separate names. Do not "fix" the misordering without adding
    /// per-fragment sequencing and re-deciding the trade-off.
    ///
    /// Confirmed to FAIL against the translator as it stood on `main` before
    /// SMA-566, which emits `Some("AADD")` and then `Some("BBCC")`.
    #[test]
    fn interleaved_aliasing_is_lossless_and_misordered() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(1, None, Some("AA"), None),
                make_chunk(0, Some("c1"), Some("BB"), None),
                make_chunk(0, None, Some("CC"), None),
                make_chunk(1, None, Some("DD"), None),
                make_chunk(1, Some("c1"), None, Some("{}")),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "AADDBBCC".to_owned())]);
    }

    /// A fragment migrating into a slot that has already emitted its name
    /// cannot reach a consumer — the event is downstream and the flush
    /// condition will not fire again for this call. It must be dropped
    /// *loudly* and recorded, never silently: a silent drop here is exactly
    /// the undiagnosed loss SMA-550 existed to eliminate.
    ///
    /// Confirmed to FAIL against the translator as it stood on `main` before
    /// SMA-566, which emits `Some("get_")` and then `Some("beta")`.
    #[test]
    fn fragment_migrating_into_an_emitted_slot_is_recorded_not_stranded() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(1, None, Some("beta"), None),
                make_chunk(0, Some("c1"), Some("get_"), Some("{}")),
                make_chunk(1, Some("c1"), None, None),
            ],
        );
        assert!(
            !t.pending.contains_key(&0),
            "the unrecoverable fragment must not be left sitting in `pending` \
             once it has been recorded and dropped"
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "get_".to_owned())]);
        assert!(
            t.warned_late_name.contains(&0),
            "the unrecoverable fragment must be recorded against the canonical index"
        );
    }

    /// A real `id` must NOT replace a blank one once this index has already
    /// emitted a delta under the blank.
    ///
    /// Replacing then splits one call across two `call_id`s: `"alpha"` has
    /// already gone out under `""`, so every later delta would arrive under
    /// `"c1"` with no name — leaving a *non-blank* `call_id` with zero
    /// name-carrying deltas. That is an "exactly once" violation on a real id,
    /// which the translator on `main` did not have (it kept everything under
    /// `""`). Withholding the upgrade keeps the call whole and matches `main`
    /// on this shape.
    ///
    /// The counterpart to `a_real_id_replaces_a_blank_one_on_the_same_wire_index`,
    /// where nothing had been emitted yet and the upgrade is correct.
    #[test]
    fn a_real_id_does_not_replace_a_blank_one_after_the_index_emitted() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some(""), Some("alpha"), Some("{}")),
                make_chunk(0, Some("c1"), None, Some("[]")),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![(String::new(), "alpha".to_owned())],
            "the name stays under the blank id it was emitted with"
        );
        assert_eq!(
            args_of(&evs, "c1"),
            "",
            "no delta may arrive under the real id, or it would carry no name"
        );
        assert_eq!(
            args_of(&evs, ""),
            "{}[]",
            "every delta for this call stays under the one call_id"
        );
    }

    /// A real `id` arriving after a blank one on the same wire index must win.
    ///
    /// Registration is otherwise first-id-wins, so a backend that changes a
    /// call's id mid-stream cannot re-point an in-flight call. A blank id is
    /// the one exception: `canonicalize` treats it as "no identity yet" rather
    /// than as an identity, so a real id arriving later must be allowed to
    /// replace it — otherwise the blank sticks and the call reaches the
    /// consumer under an empty `call_id` the agent loop cannot submit a
    /// result against.
    ///
    /// Confirmed to FAIL against the translator as it stood on `main` before
    /// SMA-566, which emits `call_id: ""`.
    #[test]
    fn a_real_id_replaces_a_blank_one_on_the_same_wire_index() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some(""), Some("foo"), None),
                make_chunk(0, Some("c1"), None, Some("{}")),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "foo".to_owned())],
            "the real id must replace the blank one, and the buffered name must survive"
        );
    }

    /// Two parallel **zero-argument** blank-id calls must both emit their name
    /// at end-of-stream.
    ///
    /// Zero-argument is load-bearing: with arguments both calls flush
    /// mid-stream and never reach `flush_buffered_names`, so this is the only
    /// shape that exercises the call_id dedup net there. An unguarded net
    /// claims `""` for the first call and silently drops the second, which is
    /// why the net is written against this test rather than the other way
    /// round. `providers-litellm` carries the same guard and a test of the
    /// same name (SMA-616).
    ///
    /// Passes both before and after the fix — a guard, not a defect proof.
    #[test]
    fn blank_ids_do_not_collapse_at_end_of_stream() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some(""), Some("alpha"), None),
                make_chunk(1, Some(""), Some("beta"), None),
            ],
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

    /// End-of-stream flush order follows the wire `index` — the model's
    /// declared call position — not the lexicographic order of `call_id`.
    ///
    /// Passes both before and after the fix. It pins the decision to keep
    /// `flush_buffered_names`'s index sort: aliasing could have moved
    /// `pending` onto a synthetic creation counter (as `providers-litellm`
    /// does, because its `index` is optional), which would silently reorder
    /// parallel calls whenever arrival order differed from index order.
    #[test]
    fn flush_order_follows_the_wire_index() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some("c_z"), Some("zulu"), None),
                make_chunk(1, Some("c_a"), Some("alpha"), None),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![
                ("c_z".to_owned(), "zulu".to_owned()),
                ("c_a".to_owned(), "alpha".to_owned()),
            ],
            "wire order (index 0 then 1), not lexicographic by call_id"
        );
    }
}
