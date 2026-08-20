//! The pinned decline set.

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
];

/// The subject names registered in `tests/conformance.rs` — one `mod` per
/// subject, each holding a `StreamUnderTest` impl whose `name()` returns one
/// of these strings.
///
/// [`DECLINED`] is checked against this list because a typo'd subject name in
/// the table would otherwise be silently invisible: `assert_declines_match`
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
}
