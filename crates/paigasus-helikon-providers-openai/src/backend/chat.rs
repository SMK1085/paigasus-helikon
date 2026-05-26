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
                None => return,
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

    // Tools: async-openai 0.40 uses `ChatCompletionTools::Function(ChatCompletionTool)`
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
/// In async-openai 0.40, the string variants (`"none"`, `"auto"`,
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

/// Buffered name and args that arrived before the `tool_calls[].id` was known.
///
/// OpenAI's Chat Completions streaming spec does not strictly guarantee that
/// `tool_calls[].id` arrives before `function.name` or `function.arguments`
/// deltas for the same `index`. Both fields are buffered here and flushed
/// into the first [`ModelEvent::ToolCallDelta`] once the id is observed.
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
    /// Indices for which `name` has already been emitted to the consumer.
    name_emitted: HashSet<u32>,
    /// index → buffered (name, args) that arrived before the call_id was known.
    pending: HashMap<u32, PendingToolCall>,
}

impl ChatTranslator {
    /// Create a fresh translator for a new streaming response.
    pub(crate) fn new() -> Self {
        Self {
            tool_calls: HashMap::new(),
            name_emitted: HashSet::new(),
            pending: HashMap::new(),
        }
    }

    /// Consume one upstream SSE chunk and produce zero or more [`ModelEvent`]s.
    ///
    /// Event ordering within a chunk follows the "Usage before Finish" contract
    /// stated in [`paigasus_helikon_core::Model::invoke`]:
    /// 1. `TokenDelta` / `ToolCallDelta` (generation deltas)
    /// 2. `Usage` (when `chunk.usage` is present — final chunk only)
    /// 3. `Finish` (terminal; always last)
    pub(crate) fn consume(&mut self, chunk: CreateChatCompletionStreamResponse) -> Vec<ModelEvent> {
        let mut out: Vec<ModelEvent> = Vec::new();
        let mut finish_event: Option<ModelEvent> = None;

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

            // Stash finish reason — emitted last (after Usage) below.
            if let Some(reason) = choice.finish_reason {
                let mapped = match reason {
                    OaFinishReason::Stop => FinishReason::Stop,
                    OaFinishReason::Length => FinishReason::Length,
                    OaFinishReason::ToolCalls => FinishReason::ToolCalls,
                    OaFinishReason::ContentFilter => FinishReason::ContentFilter,
                    OaFinishReason::FunctionCall => FinishReason::Other("function_call".to_owned()),
                    // OaFinishReason has no #[non_exhaustive] in 0.40 but guard for robustness.
                    #[allow(unreachable_patterns)]
                    other => FinishReason::Other(format!("{other:?}")),
                };
                finish_event = Some(ModelEvent::Finish { reason: mapped });
            }
        }

        // Usage arrives on the final chunk (after `include_usage: true`).
        // Emitted after generation deltas but BEFORE the Finish event, per
        // the ordering contract ("Usage MAY appear anywhere; Finish is terminal").
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

        // Append Finish last (terminal event).
        if let Some(finish) = finish_event {
            out.push(finish);
        }

        out
    }

    fn handle_tool_call_chunk(
        &mut self,
        tc: &ChatCompletionMessageToolCallChunk,
        out: &mut Vec<ModelEvent>,
    ) {
        let index = tc.index;
        let call_id_known = self.tool_calls.contains_key(&index);

        // Resolve or register the call_id.
        let call_id = if call_id_known {
            self.tool_calls[&index].clone()
        } else if let Some(id) = tc.id.as_deref() {
            self.tool_calls.insert(index, id.to_owned());
            id.to_owned()
        } else {
            // No call_id known yet and none on this delta — buffer both name
            // and args so neither is silently dropped. They will be flushed
            // into the first ToolCallDelta once the id arrives.
            let entry = self.pending.entry(index).or_default();
            if let Some(fname) = tc.function.as_ref().and_then(|f| f.name.as_deref()) {
                entry.name.push_str(fname);
            }
            if let Some(adelta) = tc.function.as_ref().and_then(|f| f.arguments.as_deref()) {
                entry.args.push_str(adelta);
            }
            return;
        };

        // Flush any name/args buffered before the call_id arrived.
        let PendingToolCall {
            name: buffered_name,
            args: buffered_args,
        } = self.pending.remove(&index).unwrap_or_default();

        // Emit name on the first delta that has it (and only once per index).
        // Prefer a buffered name (may be a concatenation of multiple pre-id
        // fragments) over the current chunk's name.
        let name_to_emit = if self.name_emitted.contains(&index) {
            None
        } else if !buffered_name.is_empty() {
            self.name_emitted.insert(index);
            Some(buffered_name)
        } else if let Some(fname) = tc.function.as_ref().and_then(|f| f.name.as_deref()) {
            self.name_emitted.insert(index);
            Some(fname.to_owned())
        } else {
            None
        };

        // Prepend any buffered args to the current chunk's args delta.
        let current_delta = tc
            .function
            .as_ref()
            .and_then(|f| f.arguments.as_deref())
            .unwrap_or("");
        let args_delta = if buffered_args.is_empty() {
            current_delta.to_owned()
        } else {
            buffered_args + current_delta
        };

        out.push(ModelEvent::ToolCallDelta {
            call_id,
            name: name_to_emit,
            args_delta,
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
}
