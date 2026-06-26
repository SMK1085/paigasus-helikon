//! Translate Gemini SSE chunks into core `ModelEvent`s.

use paigasus_helikon_core::{FinishReason, ModelError, ModelEvent};

use crate::sse::GeminiChunk;

/// Stateful translator from Gemini SSE chunks to core `ModelEvent`s.
pub(crate) struct StreamTranslator {
    fn_index: usize,
    saw_function_call: bool,
    finish_reason: Option<String>,
}

impl StreamTranslator {
    pub(crate) fn new() -> Self {
        Self {
            fn_index: 0,
            saw_function_call: false,
            finish_reason: None,
        }
    }

    pub(crate) fn consume(&mut self, chunk: GeminiChunk) -> Vec<Result<ModelEvent, ModelError>> {
        let mut out = Vec::new();

        if let Some(pf) = &chunk.prompt_feedback {
            if let Some(reason) = &pf.block_reason {
                out.push(Err(ModelError::Refused {
                    reason: format!("prompt blocked: {reason}"),
                }));
                return out;
            }
        }

        if let Some(cand) = chunk.candidates.into_iter().next() {
            if let Some(content) = cand.content {
                for part in content.parts {
                    if let Some(text) = part.text {
                        out.push(Ok(ModelEvent::TokenDelta { text }));
                    } else if let Some(fc) = part.function_call {
                        self.saw_function_call = true;
                        let call_id = fc.id.unwrap_or_else(|| {
                            let id = format!("fc_{}", self.fn_index);
                            self.fn_index += 1;
                            id
                        });
                        out.push(Ok(ModelEvent::ToolCallDelta {
                            call_id,
                            name: Some(fc.name),
                            args_delta: fc.args.to_string(),
                        }));
                    }
                }
            }
            if let Some(fr) = cand.finish_reason {
                self.finish_reason = Some(fr);
            }
        }

        if let Some(u) = chunk.usage_metadata {
            out.push(Ok(ModelEvent::Usage {
                input_tokens: u.prompt_token_count,
                output_tokens: u.candidates_token_count,
                cached_input_tokens: u.cached_content_token_count,
                reasoning_tokens: u.thoughts_token_count,
            }));
        }

        out
    }

    /// Emit the terminal `Finish` — only when a `finishReason` was observed.
    pub(crate) fn finish(&mut self) -> Vec<Result<ModelEvent, ModelError>> {
        let Some(reason) = self.finish_reason.take() else {
            return Vec::new();
        };
        let fr = match reason.as_str() {
            "STOP" if self.saw_function_call => FinishReason::ToolCalls,
            "STOP" => FinishReason::Stop,
            "MAX_TOKENS" => FinishReason::Length,
            "SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII" => {
                FinishReason::ContentFilter
            }
            other => FinishReason::Other(other.to_owned()),
        };
        vec![Ok(ModelEvent::Finish { reason: fr })]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(j: serde_json::Value) -> GeminiChunk {
        serde_json::from_value(j).unwrap()
    }

    #[test]
    fn text_delta_emitted() {
        let mut t = StreamTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "candidates": [ { "content": { "parts": [ { "text": "hello" } ] } } ]
        })));
        assert!(matches!(&evs[0], Ok(ModelEvent::TokenDelta { text }) if text == "hello"));
    }

    #[test]
    fn function_call_uses_native_id() {
        let mut t = StreamTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "candidates": [ { "content": { "parts": [
                { "functionCall": { "id": "fc_x", "name": "search", "args": {"q":"c"} } }
            ] } } ]
        })));
        match &evs[0] {
            Ok(ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            }) => {
                assert_eq!(call_id, "fc_x");
                assert_eq!(name.as_deref(), Some("search"));
                assert!(args_delta.contains("\"q\""));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn function_call_without_id_synthesizes() {
        let mut t = StreamTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "candidates": [ { "content": { "parts": [ { "functionCall": { "name": "x", "args": {} } } ] } } ]
        })));
        match &evs[0] {
            Ok(ModelEvent::ToolCallDelta { call_id, .. }) => assert!(!call_id.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn usage_maps_thoughts_to_reasoning_tokens() {
        let mut t = StreamTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 5,
                "cachedContentTokenCount": 2, "thoughtsTokenCount": 3 }
        })));
        match &evs[0] {
            Ok(ModelEvent::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                reasoning_tokens,
            }) => {
                assert_eq!(*input_tokens, 10);
                assert_eq!(*output_tokens, 5);
                assert_eq!(*cached_input_tokens, Some(2));
                assert_eq!(*reasoning_tokens, Some(3));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn finish_reason_stop_emitted_on_finish() {
        let mut t = StreamTranslator::new();
        let _ = t.consume(chunk(serde_json::json!({
            "candidates": [ { "content": { "parts": [ { "text": "hi" } ] }, "finishReason": "STOP" } ]
        })));
        let fin = t.finish();
        assert!(matches!(
            &fin[0],
            Ok(ModelEvent::Finish {
                reason: FinishReason::Stop
            })
        ));
    }

    #[test]
    fn finish_with_function_call_is_tool_calls() {
        let mut t = StreamTranslator::new();
        let _ = t.consume(chunk(serde_json::json!({
            "candidates": [ { "content": { "parts": [ { "functionCall": { "name": "x", "args": {} } } ] }, "finishReason": "STOP" } ]
        })));
        let fin = t.finish();
        assert!(matches!(
            &fin[0],
            Ok(ModelEvent::Finish {
                reason: FinishReason::ToolCalls
            })
        ));
    }

    #[test]
    fn truncated_stream_emits_no_finish() {
        let mut t = StreamTranslator::new();
        let _ = t.consume(chunk(serde_json::json!({
            "candidates": [ { "content": { "parts": [ { "text": "partial" } ] } } ]
        })));
        assert!(t.finish().is_empty());
    }

    #[test]
    fn blocked_prompt_is_refused() {
        let mut t = StreamTranslator::new();
        let evs = t.consume(chunk(
            serde_json::json!({ "promptFeedback": { "blockReason": "SAFETY" } }),
        ));
        assert!(matches!(&evs[0], Err(ModelError::Refused { .. })));
    }

    #[test]
    fn safety_finish_maps_to_content_filter() {
        let mut t = StreamTranslator::new();
        let _ = t.consume(chunk(
            serde_json::json!({ "candidates": [ { "finishReason": "SAFETY" } ] }),
        ));
        let fin = t.finish();
        assert!(matches!(
            &fin[0],
            Ok(ModelEvent::Finish {
                reason: FinishReason::ContentFilter
            })
        ));
    }
}
