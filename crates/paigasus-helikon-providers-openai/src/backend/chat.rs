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
#[derive(Default)]
struct PendingToolCall {
    name: String,
    args: String,
}

/// Accumulates Chat Completions SSE deltas and emits [`ModelEvent`]s.
///
/// Maps upstream tool-call `index` values to their `call_id` once a first
/// delta with `id` arrives; subsequent deltas for the same index reuse the
/// stored `call_id`.
pub(crate) struct ChatTranslator {
    /// index → call_id after the first delta for that tool call.
    tool_calls: HashMap<u32, String>,
    /// index → the tool name already emitted to the consumer.
    ///
    /// Holds the *value*, not just the index, so a late fragment can be
    /// told apart from a backend that repeats the whole name on every
    /// delta (SMA-547 §3).
    name_emitted: HashMap<u32, String>,
    /// Indices for which the late-name-fragment warning has already fired,
    /// so a chatty backend cannot produce one warn per argument chunk.
    warned_late_name: HashSet<u32>,
    /// index → buffered name/args.
    ///
    /// `args` is drain-once: taken on the first delta after the call_id is
    /// known and never re-prepended. `name` accumulates across every delta
    /// for the call and is cleared only when the name flushes (SMA-547 §1).
    pending: HashMap<u32, PendingToolCall>,
    /// Finish reason observed so far, emitted only by [`Self::finish`] at
    /// end-of-stream. Last observed value wins.
    finish_reason: Option<FinishReason>,
}

impl ChatTranslator {
    /// Create a fresh translator for a new streaming response.
    pub(crate) fn new() -> Self {
        Self {
            tool_calls: HashMap::new(),
            name_emitted: HashMap::new(),
            warned_late_name: HashSet::new(),
            pending: HashMap::new(),
            finish_reason: None,
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

    /// Correlate one tool-call delta and emit any completed name/args.
    ///
    /// **Divergence from `providers-litellm` (SMA-550), deliberate.** That
    /// crate canonicalizes its correlation state onto the resolved `call_id`,
    /// because its `index` is optional on the wire and one call can arrive
    /// under two different keys. This translator does not, and does not need
    /// to for that case: `ChatCompletionMessageToolCallChunk::index` is a
    /// required `u32`, so there is exactly one key space here.
    ///
    /// It is not fully aligned, though, and the gap runs the *other* way.
    /// Given two deltas carrying different `index` values but the **same**
    /// `id` — malformed, since an `id` identifies a call — litellm merges them
    /// into one call emitting one name, while this translator keeps two
    /// indexes and emits a name for each: two name-carrying `ToolCallDelta`s
    /// for one `call_id`. `flush_buffered_names` below has no `call_id`-level
    /// dedup, so nothing catches it. That shape is unobserved from any
    /// backend, which is why SMA-550 documented it here rather than changing
    /// this code. A cross-provider conformance suite asserting "at most one
    /// name-carrying delta per `call_id`" would fail here and pass for
    /// litellm; closing it needs its own ticket.
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

        // Resolve or register the call_id.
        let call_id = if let Some(id) = self.tool_calls.get(&index) {
            id.clone()
        } else if let Some(id) = tc.id.as_deref() {
            self.tool_calls.insert(index, id.to_owned());
            id.to_owned()
        } else {
            // No call_id yet — buffer both fields so neither is dropped.
            let entry = self.pending.entry(index).or_default();
            entry.name.push_str(name_frag);
            entry.args.push_str(args_frag);
            return;
        };

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

        let entry = self.pending.entry(index).or_default();
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
}
