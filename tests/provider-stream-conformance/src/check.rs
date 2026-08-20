use crate::{Scenario, Violation};
use paigasus_helikon_core::{ModelError, ModelEvent};

/// Classify the first contract violation in `events`, if any.
///
/// Rules overlap by construction — a `Usage` after `Finish` breaks both
/// assertion 2 and assertion 1 — so the order here is part of the contract, not
/// an implementation detail. Assertions 3 to 7 are added in the next task.
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

    let _ = (scenario, cancelled); // used from the next task onward
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
}
