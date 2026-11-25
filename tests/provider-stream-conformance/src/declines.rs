//! The pinned decline set (`DECLINED`) and the pinned uncaptured set
//! (`UNCAPTURED`).

use crate::Scenario;

/// Every (subject, scenario) pair that is expected to be declined, with its
/// reason. Mirrors the table in the design spec, §6.2.
///
/// The suite fails when the observed decline set differs from this in either
/// direction. Adding or removing a row is therefore a reviewed diff to a table,
/// never a one-line string literal in a match arm.
pub const DECLINED: &[(&str, Scenario, &str)] = &[
    // The tool name arrives whole in a single upstream event, so there is no
    // fragment to split.
    (
        "anthropic",
        Scenario::FragmentedToolName,
        "name arrives whole in content_block_start",
    ),
    (
        "gemini",
        Scenario::FragmentedToolName,
        "name arrives whole in functionCall",
    ),
    (
        "bedrock",
        Scenario::FragmentedToolName,
        "name arrives whole in toolUse start",
    ),
    (
        "openai/responses",
        Scenario::FragmentedToolName,
        "name arrives whole in output_item.added",
    ),
    // The window between "stop reason buffered" and "Finish emitted" lies
    // strictly between MessageStop and Metadata, with no event emitted in
    // between, so no gate edge exists.
    (
        "bedrock",
        Scenario::CancelAfterStopReason,
        "no observable event between MessageStop and Metadata",
    ),
    // terminal_events builds Usage and Finish from one upstream event, so
    // "stop reason observed but no Finish yet" is not a reachable state.
    (
        "openai/responses",
        Scenario::TruncatedAfterStopReason,
        "stop reason and Finish are the same event",
    ),
    (
        "openai/responses",
        Scenario::ErrorAfterStopReason,
        "stop reason and Finish are the same event",
    ),
    (
        "openai/responses",
        Scenario::CancelAfterStopReason,
        "stop reason and Finish are the same event",
    ),
    // The scenario serves an Anthropic thinking block, which no other
    // subject has.
    (
        "gemini",
        Scenario::ThinkingToolCallCleanStop,
        "the scenario serves an anthropic thinking block",
    ),
    (
        "bedrock",
        Scenario::ThinkingToolCallCleanStop,
        "the scenario serves an anthropic thinking block",
    ),
    (
        "litellm",
        Scenario::ThinkingToolCallCleanStop,
        "the scenario serves an anthropic thinking block",
    ),
    (
        "openai/chat",
        Scenario::ThinkingToolCallCleanStop,
        "the scenario serves an anthropic thinking block",
    ),
    (
        "openai/responses",
        Scenario::ThinkingToolCallCleanStop,
        "the scenario serves an anthropic thinking block",
    ),
    // The request never turns thinking on, so the wire carries no thought
    // parts.
    (
        "gemini",
        Scenario::ReasoningCleanStop,
        "the request never enables thought parts, so none arrive on the wire",
    ),
    // Chat Completions has no field for reasoning text.
    (
        "openai/chat",
        Scenario::ReasoningCleanStop,
        "the Chat Completions wire carries no reasoning text",
    ),
];

/// Every (subject, scenario) pair whose wire shape can occur for the provider
/// but has no fixture that meets the provenance rule yet. The fields are the
/// subject, the scenario, the reason and the follow-up Linear issue.
///
/// Like [`DECLINED`], the suite checks it in both directions. A row closes only
/// when a capture removes it or a reviewed edit moves it to [`DECLINED`].
pub const UNCAPTURED: &[(&str, Scenario, &str, &str)] = &[
    // A reasoning stream can occur for each of these subjects, but no
    // capture of one exists yet.
    (
        "bedrock",
        Scenario::ReasoningCleanStop,
        "no captured reasoning stream",
        "SMA-858",
    ),
    (
        "litellm",
        Scenario::ReasoningCleanStop,
        "no captured reasoning stream",
        "SMA-858",
    ),
    (
        "openai/responses",
        Scenario::ReasoningCleanStop,
        "no captured reasoning stream",
        "SMA-858",
    ),
    // Parallel tool calls can occur for each of these subjects, but no
    // capture of one exists yet. The repo fixtures for `anthropic` and
    // `openai/chat` are hand-authored; the others have only inline or typed
    // events.
    (
        "anthropic",
        Scenario::ParallelToolCallCleanStop,
        "no captured parallel tool-call stream",
        "SMA-859",
    ),
    (
        "bedrock",
        Scenario::ParallelToolCallCleanStop,
        "no captured parallel tool-call stream",
        "SMA-859",
    ),
    (
        "litellm",
        Scenario::ParallelToolCallCleanStop,
        "no captured parallel tool-call stream",
        "SMA-859",
    ),
    (
        "openai/chat",
        Scenario::ParallelToolCallCleanStop,
        "no captured parallel tool-call stream",
        "SMA-859",
    ),
    (
        "openai/responses",
        Scenario::ParallelToolCallCleanStop,
        "no captured parallel tool-call stream",
        "SMA-859",
    ),
];

/// The scenarios that [`UNCAPTURED`] may name. Moving an older scenario into
/// the table needs a reviewed edit to this list.
#[cfg(test)]
const UNCAPTURED_SCENARIOS: &[Scenario] = &[
    Scenario::ReasoningCleanStop,
    Scenario::ParallelToolCallCleanStop,
];

/// The subject names registered in `tests/conformance.rs` — one `mod` per
/// subject, each holding a `StreamUnderTest` impl whose `name()` returns one
/// of these strings.
///
/// [`DECLINED`] and [`UNCAPTURED`] are checked against this list because a typo'd subject name in
/// the table would otherwise be silently invisible: `pinned_problems`
/// only ever runs with a real subject's name, filtering [`DECLINED`] down to
/// the rows for that one subject. A row naming a subject that does not exist
/// never matches that filter for *any* real subject, so it is never checked
/// in either direction — not flagged as an unexpected decline, and not
/// flagged as a pinned decline that stopped happening. The reverse direction
/// (a real subject declining something absent from the table) does fail
/// loudly; this is the direction that previously did not.
#[cfg(test)]
const REGISTERED_SUBJECTS: &[&str] = &[
    "anthropic",
    "gemini",
    "bedrock",
    "litellm",
    "openai/chat",
    "openai/responses",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every subject named in [`DECLINED`] must be one this suite actually
    /// registers. Misspell one (e.g. `"bedrok"`) and this fails; the row
    /// would otherwise sit in the table forever, never compared against any
    /// real subject's observed declines.
    #[test]
    fn every_declined_subject_is_registered() {
        for (subject, scenario, reason) in DECLINED {
            assert!(
                REGISTERED_SUBJECTS.contains(subject),
                "DECLINED names {subject:?} for {scenario:?} ({reason:?}), which is not a \
                 registered subject name ({REGISTERED_SUBJECTS:?}) — check for a typo"
            );
        }
    }

    /// No pair is both declined and uncaptured.
    #[test]
    fn no_pair_is_in_both_tables() {
        for (subject, scenario, _) in DECLINED {
            assert!(
                !UNCAPTURED
                    .iter()
                    .any(|(s, sc, _, _)| s == subject && sc == scenario),
                "{subject:?} / {scenario:?} is in both DECLINED and UNCAPTURED"
            );
        }
    }

    /// A typo'd subject in `UNCAPTURED` would never be compared.
    #[test]
    fn every_uncaptured_subject_is_registered() {
        for (subject, scenario, reason, _) in UNCAPTURED {
            assert!(
                REGISTERED_SUBJECTS.contains(subject),
                "UNCAPTURED names {subject:?} for {scenario:?} ({reason:?}), which is not a \
                 registered subject name ({REGISTERED_SUBJECTS:?})"
            );
        }
    }

    /// Each follow-up field is a Linear id, `SMA-` and digits.
    #[test]
    fn every_follow_up_is_a_linear_id() {
        for (subject, scenario, _, follow_up) in UNCAPTURED {
            let ok = follow_up
                .strip_prefix("SMA-")
                .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()));
            assert!(
                ok,
                "{subject:?} / {scenario:?}: follow-up {follow_up:?} is not SMA-<digits>"
            );
        }
    }

    /// No scenario may end up served by nobody.
    #[test]
    fn every_scenario_has_a_serving_subject() {
        for scenario in Scenario::ALL {
            let served = REGISTERED_SUBJECTS.iter().any(|name| {
                !DECLINED
                    .iter()
                    .any(|(s, sc, _)| s == name && sc == scenario)
                    && !UNCAPTURED
                        .iter()
                        .any(|(s, sc, _, _)| s == name && sc == scenario)
            });
            assert!(served, "{scenario:?} has no subject that serves it");
        }
    }

    /// `UNCAPTURED` names only scenarios from the explicit allow-list.
    #[test]
    fn uncaptured_rows_name_only_allow_listed_scenarios() {
        for (subject, scenario, _, _) in UNCAPTURED {
            assert!(
                UNCAPTURED_SCENARIOS.contains(scenario),
                "UNCAPTURED names {scenario:?} for {subject:?}, which is not in \
                 UNCAPTURED_SCENARIOS"
            );
        }
    }
}
