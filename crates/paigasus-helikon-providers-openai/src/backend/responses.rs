//! Responses API backend.
//!
//! Always streams (async-openai's `create_stream` sets `stream: true`
//! automatically). The SSE stream is translated by [`ResponsesTranslator`]
//! into `ModelEvent`s.

use std::collections::HashSet;

use async_openai::traits::EventType as _;
use async_openai::types::responses::{
    CreateResponse, FunctionTool, InputItem, InputParam, OutputItem, ResponseFormatJsonSchema,
    ResponseStreamEvent, ResponseTextParam, ResponseUsage, Status,
    TextResponseFormatConfiguration, Tool, ToolChoiceOptions, ToolChoiceParam,
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
            ResponseFormat::JsonSchema { name, schema, strict } => {
                let s = if *strict { to_strict_schema(schema) } else { schema.clone() };
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
///   registers call_id + name for subsequent argument deltas
/// - `response.function_call_arguments.delta` → `ToolCallDelta` with
///   name-emission gating (name emitted once per call_id, then `None`)
/// - `response.completed` → `Usage` + `Finish { Stop }`
/// - `response.incomplete` → `Usage` + `Finish` per `incomplete_details.reason`
///   - `"max_output_tokens"` → `Finish { Length }`
///   - `"content_filter"` → `Finish { ContentFilter }`
///   - other → `Finish { Other(reason) }`
/// - `response.failed` → `Err(ModelError)` on the outer stream
/// - `error` → `Err(ModelError)` on the outer stream
///
/// All other events are dropped with a `tracing::debug!` log.
pub(crate) struct ResponsesTranslator {
    /// Tracks call_ids for which a name has already been emitted (name-emission
    /// gating: name is `Some` on the first `ToolCallDelta` for a given call_id,
    /// then `None` on subsequent deltas).
    name_emitted: HashSet<String>,
    /// Maps call_id → function name, populated by `response.output_item.added`
    /// when the item is a `function_call`.
    call_names: std::collections::HashMap<String, String>,
}

impl ResponsesTranslator {
    /// Create a fresh translator for a new streaming response.
    pub(crate) fn new() -> Self {
        Self {
            name_emitted: HashSet::new(),
            call_names: std::collections::HashMap::new(),
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

            // Output item added — register function call name for later deltas.
            ResponseStreamEvent::ResponseOutputItemAdded(e) => {
                if let OutputItem::FunctionCall(fc) = e.item {
                    self.call_names.entry(fc.call_id).or_insert(fc.name);
                }
                Ok(vec![])
            }

            // Function-call argument delta with name-emission gating.
            ResponseStreamEvent::ResponseFunctionCallArgumentsDelta(e) => {
                let already_emitted = self.name_emitted.contains(&e.item_id);
                let name = if already_emitted {
                    None
                } else {
                    self.name_emitted.insert(e.item_id.clone());
                    // Use the registered name if available; fall back to empty string
                    // so downstream consumers at least see a delta.
                    Some(
                        self.call_names
                            .get(&e.item_id)
                            .cloned()
                            .unwrap_or_default(),
                    )
                };
                Ok(vec![ModelEvent::ToolCallDelta {
                    call_id: e.item_id,
                    name,
                    args_delta: e.delta,
                }])
            }

            // Terminal: response completed.
            ResponseStreamEvent::ResponseCompleted(e) => {
                Ok(terminal_events(e.response.usage, e.response.status, None))
            }

            // Terminal: response incomplete — map reason to FinishReason.
            ResponseStreamEvent::ResponseIncomplete(e) => {
                let reason = e.response.incomplete_details.as_ref().map(|d| d.reason.as_str());
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
