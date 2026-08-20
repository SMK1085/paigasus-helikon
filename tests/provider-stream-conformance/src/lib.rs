//! Cross-provider conformance suite for the `Model::invoke` stream
//! event-ordering contract.
//!
//! This internal (never-published) crate hosts a provider-agnostic checker and
//! a paced HTTP server. Each subject in `tests/conformance.rs` serves its own
//! captured wire bytes through that server and hands back the stream from its
//! real `Model::invoke`, so the suite exercises the production driver and the
//! production translator together — not a reimplementation of either.
//!
//! See `docs/superpowers/specs/2026-08-19-sma-533-stream-conformance-design.md`.
#![forbid(unsafe_code)]

/// Event stream classification and violation detection.
pub mod check;
pub mod eventstream;
/// Fake model streams for testing.
pub mod fakes;
mod server;

pub use check::classify;
pub use server::{Ending, PacedServer, Script};

use futures_util::stream::BoxStream;
use paigasus_helikon_core::{CancellationToken, ModelError, ModelEvent};

/// One wire script, run against every subject that can express it.
///
/// The `a`/`b` pairs differ only in whether the script lets the translator
/// observe a stop reason before the stream ends. That distinction is the whole
/// point: with no stop reason buffered there is nothing for a broken driver to
/// wrongly flush, so the `a` variants cannot fail assertions 5 and 6 on their
/// own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Deltas, stop reason, usage, terminator, clean EOF.
    CleanStop,
    /// Stop reason observed, then the body ends cleanly with no terminator.
    TruncatedAfterStopReason,
    /// Body ends cleanly mid-generation; no stop reason is ever observed.
    TruncatedMidGeneration,
    /// Body aborted mid-generation; no stop reason is ever observed.
    ErrorMidGeneration,
    /// Stop reason observed, then the body is aborted.
    ErrorAfterStopReason,
    /// Cancelled mid-generation; no stop reason is ever observed.
    CancelMidGeneration,
    /// Stop reason observed, then cancelled before end-of-stream.
    CancelAfterStopReason,
    /// A tool call whose name arrives split across two or more deltas.
    FragmentedToolName,
    /// One complete tool call followed by a tool-use stop reason.
    ToolCallCleanStop,
}

impl Scenario {
    /// Every scenario, in table order.
    pub const ALL: &'static [Scenario] = &[
        Scenario::CleanStop,
        Scenario::TruncatedAfterStopReason,
        Scenario::TruncatedMidGeneration,
        Scenario::ErrorMidGeneration,
        Scenario::ErrorAfterStopReason,
        Scenario::CancelMidGeneration,
        Scenario::CancelAfterStopReason,
        Scenario::FragmentedToolName,
        Scenario::ToolCallCleanStop,
    ];

    /// Whether this scenario's script must let the translator observe a stop
    /// reason. Cross-checked against each subject's own declaration so a
    /// mis-transcribed fixture cannot make assertion 3 pass vacuously.
    pub fn expects_stop_reason(self) -> bool {
        matches!(
            self,
            Scenario::CleanStop
                | Scenario::TruncatedAfterStopReason
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelAfterStopReason
                | Scenario::ToolCallCleanStop
        )
    }
}

/// A contract violation, classified. Ordering matters — see `classify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// More than one `Finish` was emitted (assertion 1).
    DuplicateFinish,
    /// A `Usage` was emitted after `Finish` (assertion 2).
    UsageAfterFinish,
    /// Any other event, or an `Err`, was emitted after `Finish` (assertion 1).
    EventAfterFinish,
    /// End-of-stream after an observed stop reason emitted no `Finish`
    /// (assertion 3).
    MissingFinish,
    /// A `Finish` was emitted although no stop reason was observed
    /// (assertion 4).
    FinishOnTruncation,
    /// A `Finish` was emitted after cancellation (assertion 5).
    FinishOnCancel,
    /// A `Finish` was emitted after a mid-stream error (assertion 6).
    FinishAfterError,
    /// A `call_id` carried a number of name-bearing deltas other than one, or
    /// the name did not match the fixture's declared tool name (assertion 7).
    ToolNameNotExactlyOnce {
        /// The call whose name emission was wrong.
        call_id: String,
        /// How many deltas for that `call_id` carried `Some(name)`.
        count: usize,
    },
    /// The stream did not produce the minimum evidence its scenario requires,
    /// so the assertions would have passed vacuously.
    InsufficientEvidence(&'static str),
    /// The stream did not terminate within the per-scenario timeout.
    Timeout,
    /// The subject's `encodes_stop_reason` disagreed with the scenario's own
    /// expectation, so its fixture does not match the script it claims.
    StopReasonDeclarationMismatch {
        /// What the scenario requires.
        expected: bool,
        /// What the subject declared.
        declared: bool,
    },
}

/// Released by the harness once it has observed the gate event, letting the
/// server send the remaining chunks.
pub struct GateHandle {
    /// Signalled by the harness; the server waits on the paired receiver.
    /// Named `tx` rather than `release` so it does not shadow the method below.
    pub(crate) tx: tokio::sync::oneshot::Sender<()>,
}

impl GateHandle {
    /// Let the server send the remaining chunks.
    pub fn release(self) {
        let _ = self.tx.send(());
    }
}

/// What a subject did with a scenario.
///
/// Declining is a first-class outcome carrying a mandatory reason, not an
/// `Option` a caller can silently treat as a skip.
pub enum Outcome {
    /// The subject served the scenario.
    Served {
        /// The stream returned by the subject's `Model::invoke`.
        stream: BoxStream<'static, Result<ModelEvent, ModelError>>,
        /// Present only for the cancellation scenarios.
        gate: Option<GateHandle>,
    },
    /// The wire shape cannot physically occur for this provider. The reason is
    /// printed in the report and must match the pinned decline set.
    Declined(&'static str),
}

/// One provider backend under test.
#[async_trait::async_trait]
pub trait StreamUnderTest {
    /// Stable subject name, e.g. `"openai/chat"`. Used in failure output and to
    /// match rows in the pinned decline set.
    fn name(&self) -> &'static str;

    /// Whether this subject's fixture for `scenario` encodes a stop reason.
    /// Cross-checked against the scenario's own expectation.
    fn encodes_stop_reason(&self, scenario: Scenario) -> bool;

    /// The tool name this subject's tool-call fixtures declare.
    fn fixture_tool_name(&self) -> &'static str;

    /// Serve `scenario` and return the subject's `Model::invoke` stream.
    async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome;
}
