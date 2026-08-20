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
