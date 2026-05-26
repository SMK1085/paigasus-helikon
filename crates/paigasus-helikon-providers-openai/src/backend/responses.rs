//! Responses API backend.
//!
//! Always streams (async-openai's `create_stream` sets `stream: true`
//! automatically). The SSE stream is translated by [`ResponsesTranslator`]
//! into `ModelEvent`s.

use std::collections::{HashMap, HashSet};

use async_openai::traits::EventType as _;
use async_openai::types::responses::{
    CreateResponse, FunctionTool, InputItem, InputParam, OutputItem, ResponseFormatJsonSchema,
    ResponseStreamEvent, ResponseTextParam, ResponseUsage, Status, TextResponseFormatConfiguration,
    Tool, ToolChoiceOptions, ToolChoiceParam,
};
use async_stream::stream;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, FinishReason, ModelError, ModelEvent, ModelRequest, ResponseFormat,
    ToolChoice,
};

use crate::error::map_openai_error;
use crate::model::OpenAiModel;
use crate::translate::{request::to_responses_input, tools::to_strict_schema};

/// Entry point for the Responses API backend. Always streams.
///
/// Builds a streaming Responses request via async-openai, translates
/// the SSE stream through [`ResponsesTranslator`] into a
/// `BoxStream<Result<ModelEvent, ModelError>>`.
///
/// Cancellation via [`CancellationToken`] is honoured at both the initial
/// request future and each poll of the upstream SSE stream (`tokio::select!`
/// biased toward the cancel arm).
pub(crate) async fn invoke(
    model: &OpenAiModel,
    request: ModelRequest,
    cancel: CancellationToken,
) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
    let body = build_request(model, &request)?;
    let client = model.client.clone();

    let s = stream! {
        let responses_client = client.responses();
        let create_fut = responses_client.create_stream(body);

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

        let mut translator = ResponsesTranslator::new();
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
                Some(Ok(event)) => match translator.consume(event) {
                    Ok(events) => {
                        for ev in events {
                            yield Ok(ev);
                        }
                    }
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
        }
    };

    Ok(Box::pin(s))
}

/// Build the typed request for the Responses API.
fn build_request(
    model: &OpenAiModel,
    request: &ModelRequest,
) -> Result<CreateResponse, ModelError> {
    // Translate Item messages → Responses API InputParam via JSON round-trip.
    let input_value = to_responses_input(&request.messages);
    let input_items: Vec<InputItem> = serde_json::from_value(input_value)
        .map_err(|e: serde_json::Error| ModelError::Other(anyhow::anyhow!(e)))?;

    let mut body = CreateResponse {
        model: Some(model.model_id.clone()),
        input: InputParam::Items(input_items),
        ..Default::default()
    };

    // Tools.
    if !request.tools.is_empty() {
        let tools: Vec<Tool> = request
            .tools
            .iter()
            .map(|td| {
                Tool::Function(FunctionTool {
                    name: td.name.clone(),
                    description: Some(td.description.clone()),
                    parameters: Some(to_strict_schema(&td.schema)),
                    strict: Some(true),
                    defer_loading: None,
                })
            })
            .collect();
        body.tools = Some(tools);
    }

    // ModelSettings passthrough.
    if let Some(t) = request.model_settings.temperature {
        body.temperature = Some(t);
    }
    if let Some(p) = request.model_settings.top_p {
        body.top_p = Some(p);
    }
    if let Some(m) = request.model_settings.max_output_tokens {
        body.max_output_tokens = Some(m);
    }

    // Tool choice.
    if let Some(tc) = &request.model_settings.tool_choice {
        body.tool_choice = Some(translate_tool_choice(tc));
    }

    // Response format → Responses API `text.format` field.
    // Build typed TextResponseFormatConfiguration directly (the shapes differ
    // between Chat Completions and Responses API, so we cannot reuse
    // `to_openai_response_format` here).
    if let Some(rf) = &request.model_settings.response_format {
        let format = match rf {
            ResponseFormat::Text => None,
            ResponseFormat::JsonObject => Some(TextResponseFormatConfiguration::JsonObject),
            ResponseFormat::JsonSchema {
                name,
                schema,
                strict,
            } => {
                let s = if *strict {
                    to_strict_schema(schema)
                } else {
                    schema.clone()
                };
                Some(TextResponseFormatConfiguration::JsonSchema(
                    ResponseFormatJsonSchema {
                        name: name.clone(),
                        schema: s,
                        strict: Some(*strict),
                        description: None,
                    },
                ))
            }
            // Future variants from #[non_exhaustive]; default to no constraint.
            _ => None,
        };
        if let Some(fmt) = format {
            let mut text_param = body.text.take().unwrap_or(ResponseTextParam {
                format: TextResponseFormatConfiguration::Text,
                verbosity: None,
            });
            text_param.format = fmt;
            body.text = Some(text_param);
        }
    }

    // previous_response_id — thread through unmodified.
    if let Some(id) = &request.model_settings.previous_response_id {
        body.previous_response_id = Some(id.clone());
    }

    Ok(body)
}

/// Translate a [`ToolChoice`] into async-openai's Responses API
/// [`ToolChoiceParam`].
fn translate_tool_choice(tc: &ToolChoice) -> ToolChoiceParam {
    match tc {
        ToolChoice::Auto => ToolChoiceParam::Mode(ToolChoiceOptions::Auto),
        ToolChoice::Required => ToolChoiceParam::Mode(ToolChoiceOptions::Required),
        ToolChoice::None => ToolChoiceParam::Mode(ToolChoiceOptions::None),
        ToolChoice::Tool { name } => {
            use async_openai::types::responses::ToolChoiceFunction;
            ToolChoiceParam::Function(ToolChoiceFunction { name: name.clone() })
        }
        // ToolChoice is #[non_exhaustive]; new variants default to Auto.
        _ => ToolChoiceParam::Mode(ToolChoiceOptions::Auto),
    }
}

/// Accumulates Responses API SSE events and emits [`ModelEvent`]s.
///
/// The covered event types are:
///
/// - `response.output_text.delta` → `TokenDelta`
/// - `response.refusal.delta` → `TokenDelta` (refusal is the model's text)
/// - `response.reasoning_summary_text.delta` → `ReasoningDelta`
/// - `response.reasoning_text.delta` → `ReasoningDelta`
/// - `response.output_item.added` (when item is a function call) →
///   registers `item.id` → `(item.call_id, item.name)` for subsequent argument deltas;
///   also flushes any argument deltas that arrived before this event (out-of-order case).
/// - `response.function_call_arguments.delta` → `ToolCallDelta` with
///   name-emission gating (name emitted once per call_id, then `None`). If
///   `output_item.added` has not yet registered the item_id, the delta is buffered
///   in `pending_args` and flushed when the registration eventually arrives.
/// - `response.completed` → `Usage` + `Finish { Stop }`
/// - `response.incomplete` → `Usage` + `Finish` per `incomplete_details.reason`
///   - `"max_output_tokens"` → `Finish { Length }`
///   - `"content_filter"` → `Finish { ContentFilter }`
///   - other → `Finish { Other(reason) }`
/// - `response.failed` → `Err(ModelError)` on the outer stream
/// - `error` → `Err(ModelError)` on the outer stream
///
/// All other events are dropped with a `tracing::debug!` log.
///
/// ## id vs call_id
///
/// The Responses API distinguishes two identifiers on function-call items:
/// - `item.id` — internal item identifier; matches `function_call_arguments.delta.item_id`
///   and is used as the correlator between `OutputItemAdded` and subsequent delta events.
/// - `item.call_id` — stable identifier for tool submission; this is what downstream
///   consumers (tool runners, conversation history) must use when referencing the call.
///
/// `item_to_call` maps the internal `item_id` → `(call_id, name)` so that
/// `ToolCallDelta.call_id` always carries the stable call_id.
pub(crate) struct ResponsesTranslator {
    /// Tracks item_ids (internal correlator) for which a name has already been
    /// emitted (name-emission gating: name is `Some` on the first `ToolCallDelta`
    /// for a given item_id, then `None` on subsequent deltas).
    name_emitted: HashSet<String>,
    /// Maps internal `item_id` → `(stable call_id, function name)`.
    ///
    /// Populated by `response.output_item.added` when the item is a function call.
    /// Keyed by `item.id` (the correlator used in `function_call_arguments.delta`),
    /// not by `item.call_id` (the stable downstream identifier).
    item_to_call: HashMap<String, (String, String)>,
    /// Buffered argument deltas that arrived (via `function_call_arguments.delta`)
    /// before `output_item.added` registered the corresponding `item_id` mapping.
    ///
    /// Keyed by `item_id`. Flushed as a single `ToolCallDelta` (with the real
    /// `call_id` and `name`) the moment `output_item.added` registers the item.
    pending_args: HashMap<String, String>,
}

impl ResponsesTranslator {
    /// Create a fresh translator for a new streaming response.
    pub(crate) fn new() -> Self {
        Self {
            name_emitted: HashSet::new(),
            item_to_call: HashMap::new(),
            pending_args: HashMap::new(),
        }
    }

    /// Consume one upstream SSE event and produce zero or more [`ModelEvent`]s,
    /// or an error if the server emits a `response.failed` / `error` event.
    ///
    /// Event ordering follows the "Usage before Finish" contract stated in
    /// [`paigasus_helikon_core::Model::invoke`]:
    /// 1. `TokenDelta` / `ReasoningDelta` / `ToolCallDelta` (generation deltas)
    /// 2. `Usage` (when the terminal response event carries `usage`)
    /// 3. `Finish` (terminal; always last)
    pub(crate) fn consume(
        &mut self,
        event: ResponseStreamEvent,
    ) -> Result<Vec<ModelEvent>, ModelError> {
        match event {
            // Text token delta.
            ResponseStreamEvent::ResponseOutputTextDelta(e) => {
                if e.delta.is_empty() {
                    Ok(vec![])
                } else {
                    Ok(vec![ModelEvent::TokenDelta { text: e.delta }])
                }
            }

            // Refusal delta — the refusal IS the model's response text.
            ResponseStreamEvent::ResponseRefusalDelta(e) => {
                if e.delta.is_empty() {
                    Ok(vec![])
                } else {
                    Ok(vec![ModelEvent::TokenDelta { text: e.delta }])
                }
            }

            // Reasoning summary text delta.
            ResponseStreamEvent::ResponseReasoningSummaryTextDelta(e) => {
                if e.delta.is_empty() {
                    Ok(vec![])
                } else {
                    Ok(vec![ModelEvent::ReasoningDelta { text: e.delta }])
                }
            }

            // Reasoning text delta (inline reasoning, not summary).
            ResponseStreamEvent::ResponseReasoningTextDelta(e) => {
                if e.delta.is_empty() {
                    Ok(vec![])
                } else {
                    Ok(vec![ModelEvent::ReasoningDelta { text: e.delta }])
                }
            }

            // Output item added — register item_id → (call_id, name) for later deltas.
            //
            // `fc.id` is the internal item identifier that matches
            // `function_call_arguments.delta.item_id` (the correlator).
            // `fc.call_id` is the stable identifier for downstream tool execution.
            //
            // After registering, flush any argument deltas that arrived before this
            // event (out-of-order case: delta before `output_item.added`).
            ResponseStreamEvent::ResponseOutputItemAdded(e) => {
                if let OutputItem::FunctionCall(fc) = e.item {
                    if let Some(item_id) = fc.id {
                        let name = fc.name.clone();
                        let call_id = fc.call_id.clone();
                        self.item_to_call
                            .entry(item_id.clone())
                            .or_insert_with(|| (call_id.clone(), name.clone()));

                        // Flush buffered args that arrived before this event.
                        if let Some(buffered) = self.pending_args.remove(&item_id) {
                            if !buffered.is_empty() {
                                // First (and only) ToolCallDelta for these buffered args:
                                // emit name here since this is the first time we know the
                                // call_id; mark name_emitted so it won't repeat.
                                self.name_emitted.insert(item_id.clone());
                                return Ok(vec![ModelEvent::ToolCallDelta {
                                    call_id,
                                    name: Some(name),
                                    args_delta: buffered,
                                }]);
                            }
                        }
                    }
                }
                Ok(vec![])
            }

            // Function-call argument delta with name-emission gating.
            //
            // `e.item_id` is the internal correlator; look up the stable
            // `call_id` and `name` from the map built by `OutputItemAdded`.
            ResponseStreamEvent::ResponseFunctionCallArgumentsDelta(e) => {
                let already_emitted = self.name_emitted.contains(&e.item_id);
                if let Some((call_id, fn_name)) = self.item_to_call.get(&e.item_id) {
                    let name = if already_emitted {
                        None
                    } else {
                        self.name_emitted.insert(e.item_id.clone());
                        Some(fn_name.clone())
                    };
                    Ok(vec![ModelEvent::ToolCallDelta {
                        call_id: call_id.clone(),
                        name,
                        args_delta: e.delta,
                    }])
                } else {
                    // item_id not yet registered — buffer the delta until
                    // `output_item.added` arrives with the real call_id and name.
                    // Do NOT emit a synthetic ToolCallDelta (that would leak the
                    // wrong id downstream and permanently suppress the real name).
                    tracing::debug!(
                        target: "paigasus::openai::responses",
                        item_id = %e.item_id,
                        "function_call_arguments.delta arrived before output_item.added; buffering"
                    );
                    self.pending_args
                        .entry(e.item_id)
                        .or_default()
                        .push_str(&e.delta);
                    Ok(vec![])
                }
            }

            // Terminal: response completed.
            ResponseStreamEvent::ResponseCompleted(e) => {
                Ok(terminal_events(e.response.usage, e.response.status, None))
            }

            // Terminal: response incomplete — map reason to FinishReason.
            ResponseStreamEvent::ResponseIncomplete(e) => {
                let reason = e
                    .response
                    .incomplete_details
                    .as_ref()
                    .map(|d| d.reason.as_str());
                Ok(terminal_events(e.response.usage, e.response.status, reason))
            }

            // Terminal: response failed — emit error on the outer stream.
            ResponseStreamEvent::ResponseFailed(e) => {
                let msg = e
                    .response
                    .error
                    .map(|err| err.message)
                    .unwrap_or_else(|| "response.failed with no error details".to_owned());
                Err(ModelError::Other(anyhow::anyhow!("{}", msg)))
            }

            // Error event from the server.
            ResponseStreamEvent::ResponseError(e) => {
                tracing::warn!(
                    target: "paigasus::openai::responses",
                    code = e.code.as_deref().unwrap_or("unknown"),
                    message = %e.message,
                    "Responses API server error event"
                );
                Err(ModelError::Other(anyhow::anyhow!("{}", e.message)))
            }

            // All other events → drop with debug log.
            other => {
                tracing::debug!(
                    target: "paigasus::openai::responses",
                    event_type = other.event_type(),
                    "unhandled Responses API event"
                );
                Ok(vec![])
            }
        }
    }
}

/// Build the terminal `[Usage, Finish]` event pair from a response's
/// usage snapshot, status, and optional `incomplete_details.reason` string.
///
/// When `incomplete_reason` is `Some`, it overrides the status-based mapping:
/// - `"max_output_tokens"` → `Finish { Length }`
/// - `"content_filter"` → `Finish { ContentFilter }`
/// - other string → `Finish { Other(reason) }`
fn terminal_events(
    usage: Option<ResponseUsage>,
    status: Status,
    incomplete_reason: Option<&str>,
) -> Vec<ModelEvent> {
    let mut out = Vec::new();

    if let Some(u) = usage {
        out.push(ModelEvent::Usage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cached_input_tokens: Some(u.input_tokens_details.cached_tokens),
            reasoning_tokens: Some(u.output_tokens_details.reasoning_tokens),
        });
    }

    let reason = if let Some(r) = incomplete_reason {
        match r {
            "max_output_tokens" => FinishReason::Length,
            "content_filter" => FinishReason::ContentFilter,
            other => FinishReason::Other(other.to_owned()),
        }
    } else {
        match status {
            Status::Completed => FinishReason::Stop,
            Status::Failed => FinishReason::Other("failed".to_owned()),
            Status::Incomplete => FinishReason::Length,
            Status::Cancelled => FinishReason::Other("cancelled".to_owned()),
            Status::Queued => FinishReason::Other("queued".to_owned()),
            Status::InProgress => FinishReason::Other("in_progress".to_owned()),
        }
    };

    out.push(ModelEvent::Finish { reason });
    out
}

#[cfg(test)]
mod tests {
    use async_openai::types::responses::{
        FunctionToolCall, OutputItem, ResponseFunctionCallArgumentsDeltaEvent,
        ResponseOutputItemAddedEvent, ResponseStreamEvent,
    };

    use super::*;

    fn delta_event(item_id: &str, delta: &str) -> ResponseStreamEvent {
        ResponseStreamEvent::ResponseFunctionCallArgumentsDelta(
            ResponseFunctionCallArgumentsDeltaEvent {
                sequence_number: 0,
                item_id: item_id.to_owned(),
                output_index: 0,
                delta: delta.to_owned(),
            },
        )
    }

    fn added_event(item_id: &str, call_id: &str, name: &str) -> ResponseStreamEvent {
        ResponseStreamEvent::ResponseOutputItemAdded(ResponseOutputItemAddedEvent {
            sequence_number: 1,
            output_index: 0,
            item: OutputItem::FunctionCall(FunctionToolCall {
                arguments: String::new(),
                call_id: call_id.to_owned(),
                namespace: None,
                name: name.to_owned(),
                id: Some(item_id.to_owned()),
                status: None,
            }),
        })
    }

    /// Baseline: `output_item.added` arrives before any deltas (happy path).
    /// The first delta should carry name=Some("search") and the real call_id.
    #[test]
    fn ordered_added_before_delta() {
        let mut t = ResponsesTranslator::new();

        let evs = t.consume(added_event("x", "c1", "search")).unwrap();
        assert!(
            evs.is_empty(),
            "added event alone should yield no ModelEvents"
        );

        let evs = t.consume(delta_event("x", "{\"q\":1}")).unwrap();
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(name.as_deref(), Some("search"));
                assert_eq!(args_delta, "{\"q\":1}");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }

        // Second delta must NOT re-emit name.
        let evs2 = t.consume(delta_event("x", "more")).unwrap();
        assert_eq!(evs2.len(), 1);
        if let ModelEvent::ToolCallDelta { name, .. } = &evs2[0] {
            assert!(name.is_none(), "name must not be re-emitted");
        }
    }

    /// Out-of-order: delta arrives before `output_item.added`.
    /// The delta must be buffered; when `output_item.added` arrives, a single
    /// `ToolCallDelta` with the correct call_id and name must be flushed.
    #[test]
    fn out_of_order_delta_before_added() {
        let mut t = ResponsesTranslator::new();

        // Delta arrives first — should be silently buffered.
        let evs = t.consume(delta_event("x", "{\"q\":")).unwrap();
        assert!(
            evs.is_empty(),
            "delta before added should be buffered, not emitted; got {evs:?}"
        );

        // `output_item.added` arrives — should flush the buffered delta as one event.
        let evs = t.consume(added_event("x", "c1", "search")).unwrap();
        assert_eq!(
            evs.len(),
            1,
            "expected flushed ToolCallDelta on added; got {evs:?}"
        );
        match &evs[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(
                    call_id, "c1",
                    "call_id must be the stable one from output_item.added"
                );
                assert_eq!(
                    name.as_deref(),
                    Some("search"),
                    "name must be emitted with flushed delta"
                );
                assert_eq!(args_delta, "{\"q\":", "buffered args must be flushed");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }
    }

    /// Multiple out-of-order deltas for the same item_id are all buffered and
    /// flushed together as a single `ToolCallDelta`.
    #[test]
    fn multiple_orphan_deltas_concatenated() {
        let mut t = ResponsesTranslator::new();

        assert!(t.consume(delta_event("x", "part1")).unwrap().is_empty());
        assert!(t.consume(delta_event("x", "part2")).unwrap().is_empty());

        let evs = t.consume(added_event("x", "c2", "fn")).unwrap();
        assert_eq!(evs.len(), 1);
        if let ModelEvent::ToolCallDelta { args_delta, .. } = &evs[0] {
            assert_eq!(args_delta, "part1part2");
        } else {
            panic!("expected ToolCallDelta, got {evs:?}");
        }
    }
}
