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
/// stream.
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
    for (call_id, count) in name_counts {
        if count != 1 {
            return Some(Violation::ToolNameNotExactlyOnce { call_id, count });
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
}
