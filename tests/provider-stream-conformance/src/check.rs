use crate::{Scenario, Violation};
use paigasus_helikon_core::{ModelError, ModelEvent};

/// Classify the first contract violation in `events`, if any.
///
/// Rules overlap by construction — a `Usage` after `Finish` breaks both
/// assertion 2 and assertion 1 — so the order here is part of the contract, not
/// an implementation detail. Two overlaps among assertions 3 to 7 are worth
/// calling out: cancellation (assertion 5) is checked before the stop-reason
/// and error rules (3, 4, 6) because a cancelled stream's stop-reason
/// expectation is moot; and assertion 3 explicitly excludes streams carrying
/// an `Err` or a cancellation, because assertion 6 governs the former and
/// assertion 5 governs the latter, and both demand the opposite outcome —
/// withholding `Finish` is exactly what the contract requires of a cancelled
/// stream. Assertion 9 (nothing but `Usage` after an `Err`) runs directly
/// after assertion 6, so a `Finish` after an `Err` keeps the name
/// `FinishAfterError`.
pub fn classify(
    events: &[Result<ModelEvent, ModelError>],
    scenario: Scenario,
    cancelled: bool,
) -> Option<Violation> {
    let finish_at = events
        .iter()
        .position(|e| matches!(e, Ok(ModelEvent::Finish { .. })));

    if let Some(idx) = finish_at {
        let after = &events[idx + 1..];
        if after
            .iter()
            .any(|e| matches!(e, Ok(ModelEvent::Finish { .. })))
        {
            return Some(Violation::DuplicateFinish);
        }
        if after
            .iter()
            .any(|e| matches!(e, Ok(ModelEvent::Usage { .. })))
        {
            return Some(Violation::UsageAfterFinish);
        }
        if !after.is_empty() {
            return Some(Violation::EventAfterFinish);
        }
    }

    let has_finish = finish_at.is_some();
    let err_at = events.iter().position(Result::is_err);

    // Assertion 5: a cancelled stream must not emit Finish. Checked before
    // 3/4/6 because cancellation makes the scenario's stop-reason expectation
    // moot.
    if cancelled && has_finish {
        return Some(Violation::FinishOnCancel);
    }

    // Assertion 6: a mid-stream error must not be followed by a clean Finish.
    if finish_at.zip(err_at).is_some_and(|(f, e)| f > e) {
        return Some(Violation::FinishAfterError);
    }

    // Assertion 9: nothing follows the first `Err` except `Usage`. Checked
    // after assertion 6 so a `Finish` after an `Err` keeps the more specific
    // `FinishAfterError`. The `Usage` exemption is option B of the spec's
    // section 6.6: the tokens were consumed, so a trailing `Usage` is real
    // billing data. Any other event after it, including a later `Err`, is
    // still a violation.
    if let Some(e) = err_at {
        if events[e + 1..]
            .iter()
            .any(|ev| !matches!(ev, Ok(ModelEvent::Usage { .. })))
        {
            return Some(Violation::EventAfterError);
        }
    }

    // Assertion 4: a Finish must never appear when the scenario expects no
    // stop reason to be observed.
    if !scenario.expects_stop_reason() && has_finish {
        return Some(Violation::FinishOnTruncation);
    }

    // Assertion 3: when a stop reason is expected and the stream ended
    // without an error or a cancellation, end-of-stream with no Finish is a
    // violation. The no-`Err` guard matters: assertion 6 governs the error
    // case and requires the opposite outcome. The no-`cancelled` guard
    // matters for the same reason: assertion 5 governs the cancelled case,
    // and withholding `Finish` is exactly what the contract requires there.
    if scenario.expects_stop_reason() && err_at.is_none() && !cancelled && !has_finish {
        return Some(Violation::MissingFinish);
    }

    // Assertion 7: each call_id must carry exactly one name-bearing delta —
    // not "at most one". A translator that never emits a name for a call_id
    // satisfies "at most one" while still producing a tool call that resolves
    // to nothing on replay, which is precisely the bug this rule exists to
    // catch. Groups are built preserving first-seen `call_id` order so the
    // reported violation is deterministic.
    //
    // A blank `call_id` is graded differently: it needs at least one name,
    // not exactly one. An empty id cannot identify a call, so translators
    // refuse to merge parallel blank-id calls, and each such call carries
    // its own name under "" (SMA-566, SMA-616, SMA-617). Two names under ""
    // are therefore what the `ModelEvent::ToolCallDelta` contract requires
    // for two blank-id calls, and this assertion must not reject them.
    // A blank-id group with no name at all is still a translator defect.
    // Whether to refuse the resulting ambiguous turn is a consumer
    // decision, not a translator one: `ModelTurnAccumulator::finish()` in
    // `core` makes it, and core's `model_turn_tests` pin it (SMA-621).
    let mut name_counts: Vec<(String, usize)> = Vec::new();
    for event in events {
        if let Ok(ModelEvent::ToolCallDelta { call_id, name, .. }) = event {
            match name_counts.iter_mut().find(|(id, _)| id == call_id) {
                Some((_, count)) => {
                    if name.is_some() {
                        *count += 1;
                    }
                }
                None => name_counts.push((call_id.clone(), usize::from(name.is_some()))),
            }
        }
    }
    for (call_id, count) in &name_counts {
        let conforms = if call_id.is_empty() {
            // The "at least one" rule cannot detect a translator that
            // emits the name twice for ONE blank-id call.
            // `ModelTurnAccumulator::finish()` refuses that stream loudly
            // at runtime, so no call is lost silently.
            *count >= 1
        } else {
            *count == 1
        };
        if !conforms {
            return Some(Violation::ToolNameNotExactlyOnce {
                call_id: call_id.clone(),
                count: *count,
            });
        }
    }

    // Assertion 8: call-scoped provider data must name a call that the stream
    // streamed. A `ProviderData` event with `call_id: Some(id)` and no
    // `ToolCallDelta` for `id` could never attach to a tool call on replay.
    // Provider data without a `call_id` is not call-scoped and always passes.
    for event in events {
        if let Ok(ModelEvent::ProviderData {
            call_id: Some(call_id),
            ..
        }) = event
        {
            if !name_counts.iter().any(|(id, _)| id == call_id) {
                return Some(Violation::ProviderDataUnknownCall {
                    call_id: call_id.clone(),
                });
            }
        }
    }

    // Assertion 10: on a stream that carries a `Finish`, each call's
    // concatenated `args_delta` must parse as JSON. Blank (after trimming)
    // arguments mean an empty object, and any JSON value conforms, because
    // Gemini emits `null`. A stream without a `Finish` is truncated, so
    // half-open arguments are expected there. A blank-id group that holds
    // other than one name is an ambiguous merge of several calls; consumers
    // refuse it, so it is not parsed here. Groups keep first-seen order.
    if events
        .iter()
        .any(|e| matches!(e, Ok(ModelEvent::Finish { .. })))
    {
        let mut args: Vec<(String, String)> = Vec::new();
        for event in events {
            if let Ok(ModelEvent::ToolCallDelta {
                call_id,
                args_delta,
                ..
            }) = event
            {
                match args.iter_mut().find(|(id, _)| id == call_id) {
                    Some((_, text)) => text.push_str(args_delta),
                    None => args.push((call_id.clone(), args_delta.clone())),
                }
            }
        }
        for (call_id, text) in &args {
            if call_id.is_empty()
                && name_counts
                    .iter()
                    .find(|(id, _)| id.is_empty())
                    .is_some_and(|(_, count)| *count != 1)
            {
                continue;
            }
            let trimmed = text.trim();
            let source = if trimmed.is_empty() { "{}" } else { trimmed };
            if serde_json::from_str::<serde_json::Value>(source).is_err() {
                return Some(Violation::ToolArgsNotJson {
                    call_id: call_id.clone(),
                });
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::FinishReason;

    fn finish() -> Result<ModelEvent, ModelError> {
        Ok(ModelEvent::Finish {
            reason: FinishReason::Stop,
        })
    }
    fn token(t: &str) -> Result<ModelEvent, ModelError> {
        Ok(ModelEvent::TokenDelta { text: t.into() })
    }
    fn usage() -> Result<ModelEvent, ModelError> {
        Ok(ModelEvent::Usage {
            input_tokens: 1,
            output_tokens: 1,
            cached_input_tokens: None,
            reasoning_tokens: None,
        })
    }

    /// The SMA-522 shape: usage emitted after the terminal event.
    #[test]
    fn usage_after_finish_is_classified_as_such() {
        let evs = vec![token("hi"), finish(), usage()];
        assert_eq!(
            classify(&evs, Scenario::CleanStop, false),
            Some(Violation::UsageAfterFinish)
        );
    }

    /// A second Finish outranks the "event after Finish" rule.
    #[test]
    fn double_finish_outranks_event_after_finish() {
        let evs = vec![token("hi"), finish(), finish()];
        assert_eq!(
            classify(&evs, Scenario::CleanStop, false),
            Some(Violation::DuplicateFinish)
        );
    }

    /// An Err after Finish violates terminality just as an event does.
    #[test]
    fn err_after_finish_violates_terminality() {
        let evs = vec![token("hi"), finish(), Err(ModelError::Unavailable)];
        assert_eq!(
            classify(&evs, Scenario::CleanStop, false),
            Some(Violation::EventAfterFinish)
        );
    }

    /// A conforming clean stop has no violation.
    #[test]
    fn clean_stop_conforms() {
        let evs = vec![token("hi"), usage(), finish()];
        assert_eq!(classify(&evs, Scenario::CleanStop, false), None);
    }

    fn tool(call_id: &str, name: Option<&str>, args: &str) -> Result<ModelEvent, ModelError> {
        Ok(ModelEvent::ToolCallDelta {
            call_id: call_id.into(),
            name: name.map(str::to_owned),
            args_delta: args.into(),
        })
    }

    /// The SMA-531 shape: a stop reason was observed but no Finish was emitted.
    #[test]
    fn missing_finish_after_observed_stop_reason() {
        let evs = vec![token("hi")];
        assert_eq!(
            classify(&evs, Scenario::TruncatedAfterStopReason, false),
            Some(Violation::MissingFinish)
        );
    }

    /// Truncation with no stop reason must never be reported as a clean stop.
    #[test]
    fn finish_on_truncation_is_a_violation() {
        let evs = vec![token("hi"), finish()];
        assert_eq!(
            classify(&evs, Scenario::TruncatedMidGeneration, false),
            Some(Violation::FinishOnTruncation)
        );
    }

    /// Cancellation outranks the scenario's stop-reason expectation.
    #[test]
    fn finish_on_cancel_is_a_violation() {
        let evs = vec![token("hi"), finish()];
        assert_eq!(
            classify(&evs, Scenario::CancelAfterStopReason, true),
            Some(Violation::FinishOnCancel)
        );
    }

    /// A mid-stream error must not be followed by a clean terminal event.
    #[test]
    fn finish_after_error_is_a_violation() {
        let evs = vec![token("hi"), Err(ModelError::Unavailable), finish()];
        assert_eq!(
            classify(&evs, Scenario::ErrorAfterStopReason, false),
            Some(Violation::FinishAfterError)
        );
    }

    /// The SMA-550 shape: one call_id carrying two name-bearing deltas.
    #[test]
    fn two_named_deltas_for_one_call_id() {
        let evs = vec![
            tool("c1", Some("get_"), "{"),
            tool("c1", Some("weather"), "}"),
            Ok(ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            }),
        ];
        assert_eq!(
            classify(&evs, Scenario::ToolCallCleanStop, false),
            Some(Violation::ToolNameNotExactlyOnce {
                call_id: "c1".into(),
                count: 2
            })
        );
    }

    /// The tightening: a call that never carries a name is also a violation.
    #[test]
    fn no_named_delta_for_a_call_id() {
        let evs = vec![
            tool("c1", None, "{}"),
            Ok(ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            }),
        ];
        assert_eq!(
            classify(&evs, Scenario::ToolCallCleanStop, false),
            Some(Violation::ToolNameNotExactlyOnce {
                call_id: "c1".into(),
                count: 0
            })
        );
    }

    /// A conforming tool call passes.
    #[test]
    fn one_named_delta_conforms() {
        let evs = vec![
            tool("c1", Some("get_weather"), "{"),
            tool("c1", None, "}"),
            Ok(ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            }),
        ];
        assert_eq!(classify(&evs, Scenario::ToolCallCleanStop, false), None);
    }

    /// A cancelled stream that withholds Finish is conformant, not MissingFinish —
    /// withholding is what the contract requires of it.
    #[test]
    fn cancelled_stream_without_finish_conforms() {
        let evs = vec![token("hi")];
        assert_eq!(classify(&evs, Scenario::CancelAfterStopReason, true), None);
    }

    /// Two blank-id calls each carry their own name, as the contract requires.
    /// Confirmed to FAIL against `main` (reported count: 2).
    #[test]
    fn two_blank_id_calls_with_one_name_each_conform() {
        let evs = vec![
            tool("", Some("alpha"), "{}"),
            tool("", Some("beta"), "{}"),
            Ok(ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            }),
        ];
        assert_eq!(classify(&evs, Scenario::ToolCallCleanStop, false), None);
    }

    fn provider_data(call_id: Option<&str>) -> Result<ModelEvent, ModelError> {
        Ok(ModelEvent::ProviderData {
            provider: "test".into(),
            call_id: call_id.map(str::to_owned),
            data: serde_json::json!({ "sig": "SIG_A" }),
        })
    }

    /// Assertion 8: call-scoped data for a call that was never streamed.
    #[test]
    fn provider_data_with_unknown_call_id_is_a_violation() {
        let evs = vec![
            tool("c1", Some("get_weather"), "{}"),
            provider_data(Some("x")),
            Ok(ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            }),
        ];
        assert_eq!(
            classify(&evs, Scenario::ToolCallCleanStop, false),
            Some(Violation::ProviderDataUnknownCall {
                call_id: "x".into()
            })
        );
    }

    /// Call-scoped data that names a streamed call passes.
    #[test]
    fn provider_data_with_known_call_id_passes() {
        let evs = vec![
            tool("c1", Some("get_weather"), "{}"),
            provider_data(Some("c1")),
            Ok(ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            }),
        ];
        assert_eq!(classify(&evs, Scenario::ToolCallCleanStop, false), None);
    }

    /// A blank-id group with no name is still a translator defect.
    #[test]
    fn blank_id_with_no_name_is_a_violation() {
        let evs = vec![
            tool("", None, "{}"),
            Ok(ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            }),
        ];
        assert_eq!(
            classify(&evs, Scenario::ToolCallCleanStop, false),
            Some(Violation::ToolNameNotExactlyOnce {
                call_id: String::new(),
                count: 0
            })
        );
    }

    /// Provider data without a call id is not call-scoped.
    #[test]
    fn provider_data_without_call_id_passes() {
        let evs = vec![token("hi"), provider_data(None), finish()];
        assert_eq!(classify(&evs, Scenario::CleanStop, false), None);
    }

    /// Assertion 9: a token after an `Err` is a violation.
    #[test]
    fn token_after_err_is_a_violation() {
        let evs = vec![token("hi"), Err(ModelError::Unavailable), token("more")];
        assert_eq!(
            classify(&evs, Scenario::ErrorMidGeneration, false),
            Some(Violation::EventAfterError)
        );
    }

    /// Assertion 9, option B of the spec's section 6.6: a `Usage` after an
    /// `Err` is allowed.
    #[test]
    fn usage_after_err_conforms() {
        let evs = vec![token("hi"), Err(ModelError::Unavailable), usage()];
        assert_eq!(classify(&evs, Scenario::ErrorMidGeneration, false), None);
    }

    /// The `Usage` exemption does not reopen the stream.
    #[test]
    fn token_after_usage_after_err_is_a_violation() {
        let evs = vec![
            token("hi"),
            Err(ModelError::Unavailable),
            usage(),
            token("more"),
        ];
        assert_eq!(
            classify(&evs, Scenario::ErrorMidGeneration, false),
            Some(Violation::EventAfterError)
        );
    }

    /// A `Finish` after an `Err` keeps the more specific name (assertion 6).
    #[test]
    fn finish_after_err_keeps_its_own_name() {
        let evs = vec![token("hi"), Err(ModelError::Unavailable), finish()];
        assert_eq!(
            classify(&evs, Scenario::ErrorAfterStopReason, false),
            Some(Violation::FinishAfterError)
        );
    }

    /// Finish with `ToolCalls`, for the assertion 10 tests.
    fn tool_finish() -> Result<ModelEvent, ModelError> {
        Ok(ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        })
    }

    /// Assertion 10: truncated arguments on a finished stream.
    #[test]
    fn truncated_args_are_a_violation() {
        let evs = vec![tool("c1", Some("get_weather"), "{\"city\":"), tool_finish()];
        assert_eq!(
            classify(&evs, Scenario::ToolCallCleanStop, false),
            Some(Violation::ToolArgsNotJson {
                call_id: "c1".into()
            })
        );
    }

    /// Blank arguments are an empty object.
    #[test]
    fn blank_args_conform() {
        let evs = vec![tool("c1", Some("get_weather"), ""), tool_finish()];
        assert_eq!(classify(&evs, Scenario::ToolCallCleanStop, false), None);
    }

    /// Whitespace-only arguments are blank.
    #[test]
    fn whitespace_args_conform() {
        let evs = vec![tool("c1", Some("get_weather"), "  "), tool_finish()];
        assert_eq!(classify(&evs, Scenario::ToolCallCleanStop, false), None);
    }

    /// Any JSON value conforms, not only an object (Gemini's shape).
    #[test]
    fn non_object_json_conforms() {
        let evs = vec![tool("c1", Some("get_weather"), "null"), tool_finish()];
        assert_eq!(classify(&evs, Scenario::ToolCallCleanStop, false), None);
    }

    /// A stream without a `Finish` is not parsed.
    #[test]
    fn truncated_args_without_finish_conform() {
        let evs = vec![tool("c1", Some("get_weather"), "{\"city\":")];
        assert_eq!(
            classify(&evs, Scenario::TruncatedMidGeneration, false),
            None
        );
    }

    /// A blank-id group with two names is ambiguous and is not parsed.
    #[test]
    fn blank_id_group_with_two_names_is_not_parsed() {
        let evs = vec![
            tool("", Some("a"), "{}"),
            tool("", Some("b"), "{}"),
            tool_finish(),
        ];
        assert_eq!(classify(&evs, Scenario::ToolCallCleanStop, false), None);
    }

    /// A blank-id group with one name is parsed.
    #[test]
    fn blank_id_group_with_one_name_is_parsed() {
        let evs = vec![tool("", Some("a"), "{"), tool_finish()];
        assert_eq!(
            classify(&evs, Scenario::ToolCallCleanStop, false),
            Some(Violation::ToolArgsNotJson { call_id: "".into() })
        );
    }
}
