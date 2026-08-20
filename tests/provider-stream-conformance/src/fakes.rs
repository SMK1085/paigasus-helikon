//! Fake model streams that prove the checker can fail.
//!
//! Each non-conforming variant violates exactly one rule, and the tests below
//! assert that [`classify`] rejects it with the classification the design spec
//! names for that rule (§8). This runs on every CI run, so the suite cannot
//! silently decay into one that always passes.
//!
//! These fakes exercise the **checker**, not the HTTP path: `run` builds the
//! event vector in memory and classifies it, so no server, no transport and no
//! provider crate is involved. The floors and the decline cross-check in
//! `assert_conforms` are therefore not on this path — a fake can never be
//! rejected by a floor instead of by the classification it is written to
//! trigger.

use crate::{classify, Scenario, Violation};
use paigasus_helikon_core::{FinishReason, ModelError, ModelEvent};

/// The tool name every tool-bearing fake declares.
pub(crate) const TOOL_NAME: &str = "get_weather";

/// The `call_id` every tool-bearing fake uses. The tests pin the reported
/// violation against this exact string, so it is shared rather than repeated.
const CALL_ID: &str = "c1";

/// A stream shape that violates exactly one rule — plus [`Fake::Conforming`],
/// which violates none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fake {
    /// A `TokenDelta` after `Finish` (assertion 1).
    EventAfterFinish,
    /// An `Err` after `Finish` (assertion 1).
    ErrAfterFinish,
    /// Two `Finish` events (assertion 1).
    DoubleFinish,
    /// A `Usage` after `Finish` — the SMA-522 shape (assertion 2).
    UsageAfterFinish,
    /// End-of-stream after an observed stop reason with no `Finish` — the
    /// SMA-531 shape (assertion 3).
    NoFinishAfterStopReason,
    /// A `Finish` although no stop reason was observed (assertion 4).
    FinishOnTruncation,
    /// A `Finish` after cancellation (assertion 5).
    FinishOnCancel,
    /// A `Finish` after a mid-stream error (assertion 6).
    FinishAfterError,
    /// Two name-bearing deltas for one `call_id` — the SMA-550 shape
    /// (assertion 7).
    TwoNamedDeltas,
    /// Tool-call deltas that never carry a name, which is what the "exactly
    /// one" tightening of assertion 7 exists to catch.
    NoNamedDelta,
    /// A sequence that satisfies every assertion for the given scenario.
    Conforming,
}

impl Fake {
    /// Build this fake's event sequence for `scenario` and classify it.
    ///
    /// `cancelled` is derived from the scenario rather than passed in, so a
    /// fake can never be tested under a cancellation flag that contradicts the
    /// script it is emitting.
    pub async fn run(self, scenario: Scenario) -> Option<Violation> {
        let cancelled = matches!(
            scenario,
            Scenario::CancelMidGeneration | Scenario::CancelAfterStopReason
        );
        classify(&self.events(scenario), scenario, cancelled)
    }

    /// The event sequence this fake emits for `scenario`.
    ///
    /// Every non-conforming variant is built by perturbing
    /// [`Fake::Conforming`]'s sequence for the *same* scenario, so each fake
    /// differs from a passing stream by exactly the one rule it violates —
    /// there is no hand-written sequence that could accidentally break a
    /// second rule and be rejected for the wrong reason.
    fn events(self, scenario: Scenario) -> Vec<Result<ModelEvent, ModelError>> {
        let mut events = conforming(scenario);
        match self {
            Fake::Conforming => {}
            Fake::EventAfterFinish => events.push(token("trailing")),
            Fake::ErrAfterFinish => events.push(Err(ModelError::Unavailable)),
            // The scenarios these three are tested under differ, and so does
            // the rule each one breaks: appending a `Finish` to a stream that
            // already has one breaks assertion 1, to a truncated one breaks
            // assertion 4, to a cancelled one breaks assertion 5, and to an
            // errored one breaks assertion 6.
            Fake::DoubleFinish
            | Fake::FinishOnTruncation
            | Fake::FinishOnCancel
            | Fake::FinishAfterError => events.push(finish(scenario)),
            Fake::UsageAfterFinish => {
                events.retain(|e| !matches!(e, Ok(ModelEvent::Usage { .. })));
                events.push(usage());
            }
            Fake::NoFinishAfterStopReason => {
                events.retain(|e| !matches!(e, Ok(ModelEvent::Finish { .. })));
            }
            Fake::TwoNamedDeltas => {
                // Split the name across both deltas instead of emitting it
                // once, leaving everything else about the sequence intact.
                replace_tool_deltas(
                    &mut events,
                    &[
                        (Some("get_"), "{\"city\":"),
                        (Some("weather"), "\"Berlin\"}"),
                    ],
                );
            }
            Fake::NoNamedDelta => {
                replace_tool_deltas(&mut events, &[(None, "{\"city\":\"Berlin\"}")]);
            }
        }
        events
    }
}

/// The sequence a conforming subject emits for `scenario`.
///
/// A `Usage` and a `Finish` appear only when the scenario lets the translator
/// observe a stop reason *and* the stream is neither cancelled nor errored —
/// withholding `Finish` is what the contract requires in those two cases.
pub(crate) fn conforming(scenario: Scenario) -> Vec<Result<ModelEvent, ModelError>> {
    let mut events = vec![token("hi")];

    if matches!(
        scenario,
        Scenario::FragmentedToolName | Scenario::ToolCallCleanStop
    ) {
        events.push(tool(Some(TOOL_NAME), "{\"city\":"));
        events.push(tool(None, "\"Berlin\"}"));
    }

    let ended_early = matches!(
        scenario,
        Scenario::ErrorMidGeneration
            | Scenario::ErrorAfterStopReason
            | Scenario::CancelMidGeneration
            | Scenario::CancelAfterStopReason
    );
    if scenario.expects_stop_reason() && !ended_early {
        events.push(usage());
        events.push(finish(scenario));
    }

    if matches!(
        scenario,
        Scenario::ErrorMidGeneration | Scenario::ErrorAfterStopReason
    ) {
        events.push(Err(ModelError::Unavailable));
    }

    events
}

/// Swap every `ToolCallDelta` in `events` for `deltas`, in place, keeping the
/// position of the first one. Used to perturb only the tool-call half of a
/// conforming sequence.
fn replace_tool_deltas(
    events: &mut Vec<Result<ModelEvent, ModelError>>,
    deltas: &[(Option<&str>, &str)],
) {
    let at = events
        .iter()
        .position(|e| matches!(e, Ok(ModelEvent::ToolCallDelta { .. })))
        .expect("a tool-call fake must be run under a tool-call scenario");
    events.retain(|e| !matches!(e, Ok(ModelEvent::ToolCallDelta { .. })));
    for (offset, (name, args)) in deltas.iter().enumerate() {
        events.insert(at + offset, tool(*name, args));
    }
}

/// A text delta.
fn token(text: &str) -> Result<ModelEvent, ModelError> {
    Ok(ModelEvent::TokenDelta { text: text.into() })
}

/// A usage snapshot.
fn usage() -> Result<ModelEvent, ModelError> {
    Ok(ModelEvent::Usage {
        input_tokens: 7,
        output_tokens: 3,
        cached_input_tokens: None,
        reasoning_tokens: None,
    })
}

/// The terminal event, carrying the reason the scenario's script would encode.
fn finish(scenario: Scenario) -> Result<ModelEvent, ModelError> {
    Ok(ModelEvent::Finish {
        reason: match scenario {
            Scenario::FragmentedToolName | Scenario::ToolCallCleanStop => FinishReason::ToolCalls,
            _ => FinishReason::Stop,
        },
    })
}

/// One tool-call delta for [`CALL_ID`].
fn tool(name: Option<&str>, args_delta: &str) -> Result<ModelEvent, ModelError> {
    Ok(ModelEvent::ToolCallDelta {
        call_id: CALL_ID.into(),
        name: name.map(str::to_owned),
        args_delta: args_delta.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fake must be rejected with its own classification. A suite whose
    /// checker cannot fail is the exact failure mode that let the OpenAI bug
    /// ship past green fixtures.
    #[tokio::test]
    async fn every_fake_is_rejected_with_its_classification() {
        let cases: Vec<(Fake, Scenario, Violation)> = vec![
            (
                Fake::EventAfterFinish,
                Scenario::CleanStop,
                Violation::EventAfterFinish,
            ),
            (
                Fake::ErrAfterFinish,
                Scenario::CleanStop,
                Violation::EventAfterFinish,
            ),
            (
                Fake::DoubleFinish,
                Scenario::CleanStop,
                Violation::DuplicateFinish,
            ),
            (
                Fake::UsageAfterFinish,
                Scenario::CleanStop,
                Violation::UsageAfterFinish,
            ),
            (
                Fake::NoFinishAfterStopReason,
                Scenario::TruncatedAfterStopReason,
                Violation::MissingFinish,
            ),
            (
                Fake::FinishOnTruncation,
                Scenario::TruncatedMidGeneration,
                Violation::FinishOnTruncation,
            ),
            (
                Fake::FinishOnCancel,
                Scenario::CancelMidGeneration,
                Violation::FinishOnCancel,
            ),
            (
                Fake::FinishAfterError,
                Scenario::ErrorAfterStopReason,
                Violation::FinishAfterError,
            ),
            (
                Fake::TwoNamedDeltas,
                Scenario::ToolCallCleanStop,
                Violation::ToolNameNotExactlyOnce {
                    call_id: "c1".into(),
                    count: 2,
                },
            ),
            (
                Fake::NoNamedDelta,
                Scenario::ToolCallCleanStop,
                Violation::ToolNameNotExactlyOnce {
                    call_id: "c1".into(),
                    count: 0,
                },
            ),
        ];

        for (fake, scenario, expected) in cases {
            let observed = fake.run(scenario).await;
            assert_eq!(
                observed,
                Some(expected.clone()),
                "{fake:?} on {scenario:?} must be rejected as {expected:?}"
            );
        }
    }

    /// The conforming fake must pass every scenario it serves.
    #[tokio::test]
    async fn the_conforming_fake_passes() {
        for scenario in Scenario::ALL {
            assert_eq!(Fake::Conforming.run(*scenario).await, None, "{scenario:?}");
        }
    }

    /// The floors are the harness's own anti-vacuity guard, and the fakes never
    /// reach them — so pin here that a conforming sequence would also clear
    /// them. Without this, a floor that rejects every well-formed stream would
    /// only be discovered by Task 7's first subject.
    #[test]
    fn the_conforming_fake_clears_every_floor() {
        for scenario in Scenario::ALL {
            assert_eq!(
                crate::floor_violation(*scenario, &conforming(*scenario), TOOL_NAME),
                None,
                "{scenario:?}"
            );
        }
    }
}
