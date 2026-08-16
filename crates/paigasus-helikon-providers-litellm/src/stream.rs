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
//! 2. **Tool-call `name`/`arguments` are buffered until the `id` is known**,
//!    because the id is not guaranteed to arrive first, and both fields
//!    fragment across deltas.

use std::collections::{HashMap, HashSet};

use paigasus_helikon_core::{FinishReason, ModelEvent};

use crate::sse::{StreamChunk, ToolCallChunk};

/// Correlation key for a streaming tool call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    /// Correlated by `delta.tool_calls[].index`, when present.
    Index(u32),
    /// Correlated by `delta.tool_calls[].id`, when `index` is absent.
    Id(String),
    /// Correlated by position within `delta.tool_calls`, as a last resort.
    Position(usize),
}

/// Name/args fragments that arrived before the `id` was known.
#[derive(Default)]
struct Pending {
    /// Accumulated function-name fragments.
    name: String,
    /// Accumulated JSON-arguments fragments.
    args: String,
}

/// Accumulates SSE deltas and produces [`ModelEvent`]s.
///
/// One instance tracks a single streamed response. `consume` is called once
/// per chunk in order; `finish` is called once at stream end (`[DONE]` or
/// EOF) to emit the terminal event, if any.
pub(crate) struct ChatTranslator {
    /// Resolved call ids, keyed by correlation [`Key`].
    tool_calls: HashMap<Key, String>,
    /// Keys for which a tool-call `name` has already been emitted.
    name_emitted: HashSet<Key>,
    /// Name/args fragments buffered until the call's `id` is known.
    pending: HashMap<Key, Pending>,
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
            name_emitted: HashSet::new(),
            pending: HashMap::new(),
            finish_reason: None,
            warned_multi_choice: false,
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
                    for (pos, tc) in tcs.iter().enumerate() {
                        self.handle_tool_call(tc, pos, &mut out);
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

    fn handle_tool_call(&mut self, tc: &ToolCallChunk, pos: usize, out: &mut Vec<ModelEvent>) {
        let key = match (tc.index, tc.id.as_deref()) {
            (Some(i), _) => Key::Index(i),
            (None, Some(id)) => Key::Id(id.to_owned()),
            (None, None) => {
                tracing::debug!(
                    target: "paigasus::litellm::stream",
                    pos,
                    "tool-call delta has neither index nor id; correlating by position"
                );
                Key::Position(pos)
            }
        };

        let name_frag = tc.function.as_ref().and_then(|f| f.name.as_deref());
        let args_frag = tc.function.as_ref().and_then(|f| f.arguments.as_deref());

        if let Some(id) = tc.id.as_deref() {
            self.tool_calls
                .entry(key.clone())
                .or_insert_with(|| id.to_owned());
        }

        let Some(call_id) = self.tool_calls.get(&key).cloned() else {
            // No id yet — buffer both fragments.
            let slot = self.pending.entry(key).or_default();
            if let Some(n) = name_frag {
                slot.name.push_str(n);
            }
            if let Some(a) = args_frag {
                slot.args.push_str(a);
            }
            return;
        };

        let buffered = self.pending.remove(&key).unwrap_or_default();
        let mut name = buffered.name;
        if let Some(n) = name_frag {
            name.push_str(n);
        }
        let mut args = buffered.args;
        if let Some(a) = args_frag {
            args.push_str(a);
        }

        let emit_name = if self.name_emitted.contains(&key) || name.is_empty() {
            None
        } else {
            self.name_emitted.insert(key.clone());
            Some(name)
        };

        if emit_name.is_none() && args.is_empty() {
            return;
        }

        out.push(ModelEvent::ToolCallDelta {
            call_id,
            name: emit_name,
            args_delta: args,
        });
    }

    /// Emit the terminal `Finish`, if a `finish_reason` was ever observed.
    ///
    /// Emits nothing on a truncated stream: fabricating `Finish::Stop` would
    /// make a dropped connection indistinguishable from a clean completion,
    /// and `ModelTurnAccumulator` defaults to `Stop`, so the truncated text
    /// would be committed to session history as final.
    pub(crate) fn finish(&mut self) -> Vec<ModelEvent> {
        let Some(raw) = self.finish_reason.take() else {
            return Vec::new();
        };
        let reason = match raw.as_str() {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::Length,
            "tool_calls" | "function_call" => FinishReason::ToolCalls,
            "content_filter" => FinishReason::ContentFilter,
            other => FinishReason::Other(other.to_owned()),
        };
        vec![ModelEvent::Finish { reason }]
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
}
