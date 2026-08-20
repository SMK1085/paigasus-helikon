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

mod check;
mod declines;
pub mod eventstream;
#[cfg(test)]
mod fakes;
mod server;

pub use check::classify;
pub use declines::DECLINED;
pub use server::{Ending, PacedServer, Script};

use std::time::Duration;

use futures_util::stream::{BoxStream, StreamExt};
use paigasus_helikon_core::{CancellationToken, FinishReason, ModelError, ModelEvent};

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
    /// The terminal event carried a different `FinishReason` than the
    /// subject's fixture declares, so the bytes served are not the ones the
    /// scenario describes (spec §7.1).
    FinishReasonMismatch {
        /// What the subject's `fixture_finish_reason` declared.
        expected: FinishReason,
        /// What the stream actually emitted.
        observed: FinishReason,
    },
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
    ///
    /// Returns whether the server was still parked on this gate. `false` means
    /// it had already run its script to the end, so nothing was being withheld
    /// — which is how a cancellation scenario silently degrades into an ungated
    /// one, and why `assert_conforms` reads this value rather than discarding
    /// it.
    pub fn release(self) -> bool {
        self.tx.send(()).is_ok()
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

    /// The `FinishReason` this subject's fixture for `scenario` declares, or
    /// `None` for scenarios that must not produce a `Finish` at all.
    ///
    /// Only three scenarios end with a `Finish`: `CleanStop` and
    /// `ToolCallCleanStop`, whose floors check this value, and
    /// `TruncatedAfterStopReason`, where assertion 3 requires the buffered
    /// stop reason to be flushed at end-of-stream. Every other scenario must
    /// return `None` — a cancelled or errored stream withholds `Finish`, and a
    /// truncation with no stop reason observed never had one to emit.
    ///
    /// Declared per subject rather than inferred suite-side because only the
    /// fixture knows what its bytes encode: a checker that assumed
    /// `CleanStop` ⇒ `Stop` would silently accept a fixture that had been
    /// transcribed with the wrong stop reason.
    fn fixture_finish_reason(&self, scenario: Scenario) -> Option<FinishReason>;

    /// Serve `scenario` and return the subject's `Model::invoke` stream.
    async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome;
}

/// How long one scenario's drain may run before the suite gives up on it.
///
/// A subject whose stream never terminates is a real bug this suite should
/// catch, and without a bound it would hang `cargo test` rather than fail it.
/// Generous on purpose: it is only ever reached on failure, so it costs nothing
/// on a green run.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a gated stream must stay silent before the harness accepts that the
/// server is parked on the gate and fires cancellation.
///
/// **Why quiescence and not a named gate edge.** The design spec's §6.1 has the
/// harness block on a specific event — a `TokenDelta` for `CancelMidGeneration`,
/// the `Usage` the fixture places after the stop-reason chunk for
/// `CancelAfterStopReason`. That does not survive contact with Anthropic, which
/// emits a `Usage` from `message_start` (`anthropic/stream.rs:79`): "cancel on
/// the first `Usage`" would fire before any stop reason was buffered, and
/// `CancelAfterStopReason` would silently degrade into `CancelMidGeneration` for
/// that subject. Silence is subject-independent, and — unlike an event
/// predicate — is itself positive evidence that the server is withholding.
///
/// **What a slow machine does to it.** Two directions, and only one is
/// dangerous. Firing *late* (the harness task is descheduled, so the window
/// elapses later in wall-clock terms) is harmless: the server is parked either
/// way and nothing else is in flight. Firing *early* would need two consecutive
/// pre-gate chunks to arrive more than this far apart, which the server does not
/// do — `feed` pushes every pre-gate chunk into an unbounded channel with no
/// await between them, so they reach the socket back-to-back over loopback.
/// Were it ever to happen, the consequence is a weaker test (cancellation at an
/// earlier truncation point), not a spurious failure — and the
/// `server_was_parked` evidence below is unaffected either way.
const GATE_QUIESCENCE: Duration = Duration::from_millis(400);

/// Run every [`Scenario`] against `subject` and panic on the first thing that
/// does not hold.
///
/// Three guards run in order for each served scenario, and each exists because
/// of a specific way this suite could go quietly useless:
///
/// 1. **The stop-reason declaration is cross-checked.** Assertions 3 and 4 are
///    conditioned on whether a stop reason was observed, and that cannot be
///    inferred from the emitted events — a provider wrongly suppressing
///    `Finish` looks identical to one that correctly never saw a stop reason.
/// 2. **The positive-evidence floors run before the assertions.** A stream that
///    emits nothing satisfies "ends with `Finish`" trivially, so a miswired
///    adapter serving the wrong fixture would otherwise pass every assertion by
///    producing nothing at all.
/// 3. **Then, and only then, the events are classified** by the crate's
///    `classify` function.
///
/// Afterwards the observed declines are compared against the pinned
/// [`DECLINED`] table in both directions.
pub async fn assert_conforms(subject: &impl StreamUnderTest) {
    let name = subject.name();
    let mut declined: Vec<(Scenario, &'static str)> = Vec::new();

    for &scenario in Scenario::ALL {
        let cancel = CancellationToken::new();
        let (stream, gate) = match subject.stream(scenario, cancel.clone()).await {
            Outcome::Declined(reason) => {
                declined.push((scenario, reason));
                continue;
            }
            Outcome::Served { stream, gate } => (stream, gate),
        };

        let declared = subject.encodes_stop_reason(scenario);
        let expected = scenario.expects_stop_reason();
        if declared != expected {
            fail(
                name,
                scenario,
                &Violation::StopReasonDeclarationMismatch { expected, declared },
                "the subject's fixture does not encode what this scenario requires, so \
                 assertions 3 and 4 would be checked against the wrong condition",
            );
        }

        let cancelled = is_cancel_scenario(scenario);
        if cancelled && gate.is_none() {
            fail(
                name,
                scenario,
                &Violation::InsufficientEvidence(
                    "a cancellation scenario was served without a gate, so the whole body was \
                     already on its way and cancellation could not truncate anything",
                ),
                "give the script a `gate_after` and hand the `GateHandle` back in \
                 `Outcome::Served`",
            );
        }

        let Ok(drained) =
            tokio::time::timeout(DRAIN_TIMEOUT, drain(stream, gate, scenario, cancel)).await
        else {
            fail(
                name,
                scenario,
                &Violation::Timeout,
                "the stream never ended. For a cancellation scenario, check that the driver \
                 selects on the cancellation token while awaiting the next chunk; otherwise \
                 check that the script's ending actually closes the body",
            );
        };

        if let Some(violation) = floor_violation(
            scenario,
            &drained.events,
            subject.fixture_tool_name(),
            subject.fixture_finish_reason(scenario),
        ) {
            fail(
                name,
                scenario,
                &violation,
                &format!("observed: {}", summarise(&drained.events)),
            );
        }

        if let Some(violation) = drained.cancel_violation() {
            fail(
                name,
                scenario,
                &violation,
                &format!("observed: {}", summarise(&drained.events)),
            );
        }

        if let Some(violation) = classify(&drained.events, scenario, cancelled) {
            fail(
                name,
                scenario,
                &violation,
                &format!("observed: {}", summarise(&drained.events)),
            );
        }
    }

    assert_declines_match(name, &declined);
}

/// Whether `scenario` cancels the stream part-way through.
fn is_cancel_scenario(scenario: Scenario) -> bool {
    matches!(
        scenario,
        Scenario::CancelMidGeneration | Scenario::CancelAfterStopReason
    )
}

/// Everything one scenario's drain observed.
struct Drained {
    /// The events the subject's stream produced, in order.
    events: Vec<Result<ModelEvent, ModelError>>,
    /// Present only for a cancellation scenario.
    cancel: Option<CancelEvidence>,
}

/// Proof that a cancellation scenario actually cancelled something.
///
/// Dropping a [`GateHandle`] releases the body, so a cancellation scenario can
/// silently degrade into an ungated one — the whole script plays out, the
/// stream ends on its own, and the assertions pass without cancellation ever
/// having been exercised. Both fields below are recorded so that degradation
/// fails loudly instead.
struct CancelEvidence {
    /// The stream fell quiet, so the harness fired the cancellation token.
    /// `false` means the stream ended by itself first.
    fired: bool,
    /// The server was still parked on the gate when the stream ended, so the
    /// rest of the script was never delivered.
    server_was_parked: bool,
}

impl Drained {
    /// The missing-evidence violation for a cancellation scenario, if any.
    fn cancel_violation(&self) -> Option<Violation> {
        let evidence = self.cancel.as_ref()?;
        if !evidence.fired {
            return Some(Violation::InsufficientEvidence(
                "the stream ended on its own before cancellation was fired, so this scenario \
                 tested no cancellation at all",
            ));
        }
        if !evidence.server_was_parked {
            return Some(Violation::InsufficientEvidence(
                "the server had already run its script to the end when the stream finished, so \
                 cancellation truncated nothing",
            ));
        }
        None
    }
}

/// Drain one scenario's stream, cancelling part-way through when the scenario
/// calls for it.
///
/// For a cancellation scenario the sequence is: read every event the server
/// sent before the gate; wait for the stream to fall quiet, which is what
/// proves the server is withholding; fire the token; read whatever the driver
/// still emits; and only then release the gate — the release is what reports
/// whether the server really was parked on it.
///
/// The gate is deliberately *not* released before cancelling. Releasing first
/// would put the rest of the script on the wire in a race with the token, so a
/// correct driver could emit `Finish` from an already-delivered terminator
/// before it ever observed the cancellation, and the suite would report
/// `FinishOnCancel` against provider code that did nothing wrong.
async fn drain(
    mut stream: BoxStream<'static, Result<ModelEvent, ModelError>>,
    gate: Option<GateHandle>,
    scenario: Scenario,
    cancel: CancellationToken,
) -> Drained {
    let mut events = Vec::new();

    if !is_cancel_scenario(scenario) {
        // Safety valve: a non-cancel scenario has nothing to synchronise on, so
        // an unreleased gate would stall the body until the timeout.
        if let Some(gate) = gate {
            gate.release();
        }
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        return Drained {
            events,
            cancel: None,
        };
    }

    let mut fired = false;
    loop {
        match tokio::time::timeout(GATE_QUIESCENCE, stream.next()).await {
            Ok(Some(event)) => events.push(event),
            // The stream ended by itself: the gate withheld nothing.
            Ok(None) => break,
            Err(_) => {
                // The stream has fallen silent, so the server is parked on the
                // gate. Fire the token while STILL HOLDING the gate. Do not
                // "simplify" this by releasing first: that puts the rest of the
                // script on the wire in a race with the token, so a correct
                // driver can emit `Finish` from an already-delivered terminator
                // before it ever observes the cancellation — and the suite would
                // report `FinishOnCancel` against provider code that did nothing
                // wrong, intermittently, depending on machine load. Holding is
                // also what makes the truncation provable further down.
                cancel.cancel();
                fired = true;
                break;
            }
        }
    }

    if fired {
        while let Some(event) = stream.next().await {
            events.push(event);
        }
    }

    let server_was_parked = gate.is_some_and(GateHandle::release);
    Drained {
        events,
        cancel: Some(CancelEvidence {
            fired,
            server_was_parked,
        }),
    }
}

/// The positive-evidence floor for `scenario`, per spec §7.1.
///
/// Each floor asserts only the *vacuity* direction — too little evidence to
/// have tested anything. Excess is left to `classify`, which has a specific
/// name for it: two `Finish` events are a `DuplicateFinish`, and two
/// name-bearing deltas for one call are a `ToolNameNotExactlyOnce` (the exact
/// SMA-550 shape). A floor that also rejected the excess would mask both behind
/// `InsufficientEvidence`, which is the wrong diagnosis for a stream that
/// emitted too much.
///
/// The one exception is the `Err` count, because `classify` has no rule for a
/// stream that errors twice and would otherwise let it through unremarked.
///
/// The `FinishReason` check also lives here rather than in `classify`, and
/// must: `classify` has no fixture knowledge and its signature is fixed, while
/// the reason a `Finish` should carry is exactly a property of the bytes the
/// subject transcribed.
fn floor_violation(
    scenario: Scenario,
    events: &[Result<ModelEvent, ModelError>],
    tool_name: &str,
    finish_reason: Option<FinishReason>,
) -> Option<Violation> {
    match scenario {
        Scenario::CleanStop
        | Scenario::TruncatedAfterStopReason
        | Scenario::TruncatedMidGeneration
        | Scenario::ErrorMidGeneration
        | Scenario::ErrorAfterStopReason
        | Scenario::CancelMidGeneration
        | Scenario::CancelAfterStopReason => {
            if !events
                .iter()
                .any(|e| matches!(e, Ok(ModelEvent::TokenDelta { .. })))
            {
                return Some(Violation::InsufficientEvidence(
                    "no TokenDelta: a stream that emitted no text passes every ordering \
                     assertion vacuously",
                ));
            }
        }
        Scenario::FragmentedToolName | Scenario::ToolCallCleanStop => {
            if !events
                .iter()
                .any(|e| matches!(e, Ok(ModelEvent::ToolCallDelta { .. })))
            {
                return Some(Violation::InsufficientEvidence(
                    "no ToolCallDelta: a tool scenario that emitted no tool call passes \
                     assertion 7 vacuously",
                ));
            }
            if !events.iter().any(|e| {
                matches!(e, Ok(ModelEvent::ToolCallDelta { name: Some(name), .. }) if name == tool_name)
            }) {
                return Some(Violation::InsufficientEvidence(
                    "no ToolCallDelta carried the tool name this subject declares, so the \
                     fixture served is not the one the scenario describes",
                ));
            }
        }
    }

    if matches!(scenario, Scenario::CleanStop | Scenario::ToolCallCleanStop) {
        // The first `Finish` on purpose: a second one is a `DuplicateFinish`,
        // which `classify` names precisely, and checking the reason on the
        // first is what tells a wrong fixture from a wrong driver.
        let observed = events.iter().find_map(|event| match event {
            Ok(ModelEvent::Finish { reason }) => Some(reason.clone()),
            _ => None,
        });
        let Some(observed) = observed else {
            return Some(Violation::InsufficientEvidence(
                "no Finish: a clean stop that never terminated proves nothing about terminality",
            ));
        };
        let Some(expected) = finish_reason else {
            return Some(Violation::InsufficientEvidence(
                "this scenario must end with a Finish, but the subject declares no \
                 FinishReason for it, so its fixture is not the one the scenario describes",
            ));
        };
        if observed != expected {
            return Some(Violation::FinishReasonMismatch { expected, observed });
        }
    }

    if matches!(
        scenario,
        Scenario::ErrorMidGeneration | Scenario::ErrorAfterStopReason
    ) && events.iter().filter(|e| e.is_err()).count() != 1
    {
        return Some(Violation::InsufficientEvidence(
            "an error scenario must yield exactly one Err: none means the aborted body was \
             read as a clean end-of-stream, more than one means the stream kept going after \
             it failed",
        ));
    }

    None
}

/// Compare the declines this subject actually produced against [`DECLINED`],
/// and panic on any difference in either direction.
///
/// Both directions matter. An unexpected decline is the escape hatch this
/// pinning exists to close. An expected decline that stopped happening matters
/// just as much: the subject now serves that scenario, so the table is lying
/// about the provider — and the row would go on excusing a future regression
/// that reintroduced the decline.
fn assert_declines_match(subject: &str, observed: &[(Scenario, &'static str)]) {
    let expected: Vec<(Scenario, &'static str)> = DECLINED
        .iter()
        .filter(|(name, _, _)| *name == subject)
        .map(|(_, scenario, reason)| (*scenario, *reason))
        .collect();

    let mut problems: Vec<String> = Vec::new();

    for (scenario, reason) in observed {
        match expected.iter().find(|(s, _)| s == scenario) {
            None => problems.push(format!(
                "  unexpected decline of {scenario:?} ({reason:?}). Declining is for a wire \
                 shape that cannot physically occur — if that is the case here, add a \
                 reviewed row to DECLINED; otherwise serve the scenario."
            )),
            Some((_, pinned)) if pinned != reason => problems.push(format!(
                "  decline reason for {scenario:?} drifted: DECLINED says {pinned:?}, the \
                 subject said {reason:?}"
            )),
            Some(_) => {}
        }
    }

    for (scenario, reason) in &expected {
        if !observed.iter().any(|(s, _)| s == scenario) {
            problems.push(format!(
                "  {scenario:?} is pinned as declined ({reason:?}) but the subject served it. \
                 The row is stale: remove it, so it cannot excuse a future regression."
            ));
        }
    }

    assert!(
        problems.is_empty(),
        "conformance: {subject} — decline set does not match DECLINED (spec §6.2):\n{}",
        problems.join("\n")
    );
}

/// Panic naming the subject, the scenario and the violation.
///
/// Tasks registering a new subject read these while transcribing fixtures, so
/// the message carries what to look at, not just what failed.
fn fail(subject: &str, scenario: Scenario, violation: &Violation, detail: &str) -> ! {
    panic!("conformance: {subject} / {scenario:?} — {violation:?}\n  {detail}");
}

/// A one-line summary of what a stream emitted, for failure output.
fn summarise(events: &[Result<ModelEvent, ModelError>]) -> String {
    if events.is_empty() {
        return "<nothing>".to_owned();
    }
    events
        .iter()
        .map(|event| match event {
            Ok(ModelEvent::TokenDelta { .. }) => "TokenDelta".to_owned(),
            Ok(ModelEvent::ReasoningDelta { .. }) => "ReasoningDelta".to_owned(),
            Ok(ModelEvent::ToolCallDelta { call_id, name, .. }) => match name {
                Some(name) => format!("ToolCallDelta({call_id}, name={name})"),
                None => format!("ToolCallDelta({call_id})"),
            },
            Ok(ModelEvent::Usage { .. }) => "Usage".to_owned(),
            Ok(ModelEvent::Finish { reason }) => format!("Finish({reason:?})"),
            Ok(other) => format!("{other:?}"),
            Err(err) => format!("Err({err:?})"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{conforming, TOOL_NAME};
    use std::sync::Mutex;
    use tokio::sync::oneshot;

    /// What the fake subject does with the gate it hands back for a
    /// cancellation scenario. Each variant is one way the cancel scenarios can
    /// silently stop testing anything.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Gate {
        /// The server is parked on the gate, still withholding the rest of the
        /// script. The only correct state.
        Parked,
        /// The server ran its script to the end without ever pausing, so the
        /// gate withheld nothing.
        Finished,
        /// No gate at all.
        Absent,
    }

    /// Builds one scenario's substitute event sequence.
    type EventsFn = dyn Fn() -> Vec<Result<ModelEvent, ModelError>> + Send + Sync;

    /// What the fake subject declares as its `CleanStop` fixture's
    /// `FinishReason`. The fixture always emits `Stop`, so only `Truthful`
    /// matches it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FinishDecl {
        /// `Some(Stop)` — what the fixture really encodes.
        Truthful,
        /// `Some(ToolCalls)` — a transcription that does not match the bytes.
        Wrong,
        /// `None`, for a scenario that must end with a `Finish`.
        Absent,
    }

    /// A `StreamUnderTest` built from in-memory event vectors, so the harness
    /// itself can be tested without a server or a provider.
    ///
    /// `assert_conforms` takes `&self`, so the receiver halves of the gates it
    /// hands out are parked in a `Mutex` to keep them alive for exactly as long
    /// as the subject is.
    struct Subject {
        name: &'static str,
        declines: &'static [(Scenario, &'static str)],
        gate: Gate,
        /// Whether a cancellation scenario's stream stays open until the token
        /// fires. `false` makes it end on its own, the way an ungated one does.
        holds_open: bool,
        /// Replaces the conforming sequence for one scenario. A constructor
        /// rather than a vector, because `ModelError` is not `Clone`.
        substitute: Option<(Scenario, Box<EventsFn>)>,
        /// This scenario's stream never ends.
        stalls_on: Option<Scenario>,
        /// Declare the opposite of what each scenario requires.
        lies_about_stop_reason: bool,
        /// What this subject claims its `CleanStop` fixture's `FinishReason` is.
        finish_reason: FinishDecl,
        held: Mutex<Vec<oneshot::Receiver<()>>>,
    }

    impl Subject {
        /// A subject that conforms on every scenario and declines none.
        fn conforming(name: &'static str) -> Self {
            Self {
                name,
                declines: &[],
                gate: Gate::Parked,
                holds_open: true,
                substitute: None,
                stalls_on: None,
                lies_about_stop_reason: false,
                finish_reason: FinishDecl::Truthful,
                held: Mutex::new(Vec::new()),
            }
        }

        /// The events this subject emits for `scenario`.
        fn events(&self, scenario: Scenario) -> Vec<Result<ModelEvent, ModelError>> {
            match &self.substitute {
                Some((s, build)) if *s == scenario => build(),
                _ => conforming(scenario),
            }
        }
    }

    /// A stream of `events`, optionally staying open afterwards.
    fn stream_of(
        events: Vec<Result<ModelEvent, ModelError>>,
        open_until: Option<CancellationToken>,
        forever: bool,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        let head = futures_util::stream::iter(events);
        if forever {
            return head.chain(futures_util::stream::pending()).boxed();
        }
        match open_until {
            None => head.boxed(),
            Some(cancel) => head
                .chain(
                    futures_util::stream::once(async move { cancel.cancelled().await })
                        .filter_map(|()| std::future::ready(None)),
                )
                .boxed(),
        }
    }

    #[async_trait::async_trait]
    impl StreamUnderTest for Subject {
        fn name(&self) -> &'static str {
            self.name
        }

        fn encodes_stop_reason(&self, scenario: Scenario) -> bool {
            scenario.expects_stop_reason() != self.lies_about_stop_reason
        }

        fn fixture_tool_name(&self) -> &'static str {
            TOOL_NAME
        }

        /// Mirrors what `fakes::conforming` emits: `ToolCalls` for the tool
        /// scenarios, `Stop` for the other two that end with a `Finish`.
        fn fixture_finish_reason(&self, scenario: Scenario) -> Option<FinishReason> {
            match scenario {
                Scenario::CleanStop => match self.finish_reason {
                    FinishDecl::Truthful => Some(FinishReason::Stop),
                    FinishDecl::Wrong => Some(FinishReason::ToolCalls),
                    FinishDecl::Absent => None,
                },
                Scenario::TruncatedAfterStopReason => Some(FinishReason::Stop),
                Scenario::ToolCallCleanStop => Some(FinishReason::ToolCalls),
                _ => None,
            }
        }

        async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome {
            if let Some((_, reason)) = self.declines.iter().find(|(s, _)| *s == scenario) {
                return Outcome::Declined(reason);
            }

            let events = self.events(scenario);
            let stalls = self.stalls_on == Some(scenario);

            if !is_cancel_scenario(scenario) {
                return Outcome::Served {
                    stream: stream_of(events, None, stalls),
                    gate: None,
                };
            }

            let open_until = self.holds_open.then_some(cancel);
            let stream = stream_of(events, open_until, stalls);
            let gate = match self.gate {
                Gate::Absent => None,
                Gate::Parked => {
                    let (tx, rx) = oneshot::channel();
                    self.held
                        .lock()
                        .expect("gate-receiver mutex should not be poisoned")
                        .push(rx);
                    Some(GateHandle { tx })
                }
                Gate::Finished => {
                    let (tx, _rx) = oneshot::channel();
                    Some(GateHandle { tx })
                }
            };
            Outcome::Served { stream, gate }
        }
    }

    /// The baseline: a subject that does everything right passes, so every
    /// rejection below is caused by the one thing that test changes.
    #[tokio::test]
    async fn a_conforming_subject_passes() {
        assert_conforms(&Subject::conforming("openai/chat")).await;
    }

    /// A cancellation scenario served without a gate has nothing withheld, so
    /// the whole script is already on the wire and cancellation cannot truncate
    /// it. That must fail rather than pass quietly.
    #[tokio::test]
    #[should_panic(expected = "served without a gate")]
    async fn a_cancellation_scenario_without_a_gate_fails() {
        let subject = Subject {
            gate: Gate::Absent,
            ..Subject::conforming("openai/chat")
        };
        assert_conforms(&subject).await;
    }

    /// A cancellation scenario whose stream ends by itself never reached the
    /// cancellation at all.
    #[tokio::test]
    #[should_panic(expected = "ended on its own before cancellation")]
    async fn a_cancellation_scenario_that_ends_on_its_own_fails() {
        let subject = Subject {
            holds_open: false,
            ..Subject::conforming("openai/chat")
        };
        assert_conforms(&subject).await;
    }

    /// The degradation the gate exists to prevent: the server ran its script to
    /// the end, so cancellation truncated nothing even though it fired.
    #[tokio::test]
    #[should_panic(expected = "already run its script to the end")]
    async fn a_cancellation_scenario_whose_server_finished_its_script_fails() {
        let subject = Subject {
            gate: Gate::Finished,
            ..Subject::conforming("openai/chat")
        };
        assert_conforms(&subject).await;
    }

    /// The vacuous pass §7.1 exists to catch: a stream that emits nothing
    /// satisfies every ordering assertion.
    #[tokio::test]
    #[should_panic(expected = "no TokenDelta")]
    async fn a_stream_that_emits_nothing_fails_its_floor() {
        let subject = Subject {
            substitute: Some((Scenario::CleanStop, Box::new(Vec::new))),
            ..Subject::conforming("openai/chat")
        };
        assert_conforms(&subject).await;
    }

    /// A tool scenario that emits a tool call under a different name than the
    /// subject declares was served the wrong fixture.
    #[tokio::test]
    #[should_panic(expected = "carried the tool name this subject declares")]
    async fn a_tool_call_under_the_wrong_name_fails_its_floor() {
        let renamed = || {
            vec![Ok(ModelEvent::ToolCallDelta {
                call_id: "c1".into(),
                name: Some("get_wether".into()),
                args_delta: "{}".into(),
            })]
        };
        let subject = Subject {
            substitute: Some((Scenario::FragmentedToolName, Box::new(renamed))),
            ..Subject::conforming("openai/chat")
        };
        assert_conforms(&subject).await;
    }

    /// The floors must not swallow a violation `classify` can name precisely:
    /// two name-bearing deltas is the SMA-550 shape, and it has to be reported
    /// as `ToolNameNotExactlyOnce`, not as insufficient evidence.
    #[tokio::test]
    #[should_panic(expected = "ToolNameNotExactlyOnce")]
    async fn two_named_deltas_are_classified_not_floored() {
        let twice = || {
            vec![
                Ok(ModelEvent::ToolCallDelta {
                    call_id: "c1".into(),
                    name: Some(TOOL_NAME.into()),
                    args_delta: "{".into(),
                }),
                Ok(ModelEvent::ToolCallDelta {
                    call_id: "c1".into(),
                    name: Some(TOOL_NAME.into()),
                    args_delta: "}".into(),
                }),
            ]
        };
        let subject = Subject {
            substitute: Some((Scenario::FragmentedToolName, Box::new(twice))),
            ..Subject::conforming("openai/chat")
        };
        assert_conforms(&subject).await;
    }

    /// A stream that never terminates is a real bug, and it must fail the suite
    /// rather than hang it. Time is paused, so the ten-second bound elapses as
    /// soon as the runtime goes idle.
    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "Timeout")]
    async fn a_stream_that_never_ends_fails_with_a_timeout() {
        let subject = Subject {
            stalls_on: Some(Scenario::CleanStop),
            ..Subject::conforming("openai/chat")
        };
        assert_conforms(&subject).await;
    }

    /// Spec §7.1: the single `Finish` must carry the `FinishReason` the fixture
    /// declares. A stream that terminates with the wrong reason is serving
    /// bytes other than the ones the subject claims — which a
    /// variant-only check (`matches!(.., Finish { .. })`) would wave through.
    #[tokio::test]
    #[should_panic(expected = "FinishReasonMismatch { expected: ToolCalls, observed: Stop }")]
    async fn a_finish_carrying_the_wrong_reason_fails_its_floor() {
        let subject = Subject {
            finish_reason: FinishDecl::Wrong,
            ..Subject::conforming("openai/chat")
        };
        assert_conforms(&subject).await;
    }

    /// Declaring no `FinishReason` for a scenario that must end with a `Finish`
    /// would turn the check above into a no-op, so it is itself a failure.
    #[tokio::test]
    #[should_panic(expected = "declares no FinishReason")]
    async fn no_declared_finish_reason_for_a_clean_stop_fails() {
        let subject = Subject {
            finish_reason: FinishDecl::Absent,
            ..Subject::conforming("openai/chat")
        };
        assert_conforms(&subject).await;
    }

    /// A subject whose declaration contradicts the scenario would have
    /// assertions 3 and 4 checked against the wrong condition.
    #[tokio::test]
    #[should_panic(expected = "StopReasonDeclarationMismatch")]
    async fn a_subject_that_misdeclares_its_stop_reason_fails() {
        let subject = Subject {
            lies_about_stop_reason: true,
            ..Subject::conforming("openai/chat")
        };
        assert_conforms(&subject).await;
    }

    /// Direction one of the pinned decline set: a decline that is not in the
    /// table is the escape hatch the pinning exists to close.
    #[tokio::test]
    #[should_panic(expected = "unexpected decline of CleanStop")]
    async fn an_unpinned_decline_fails() {
        let subject = Subject {
            declines: &[(Scenario::CleanStop, "did not feel like it")],
            ..Subject::conforming("openai/chat")
        };
        assert_conforms(&subject).await;
    }

    /// Direction two, which matters just as much: a pinned decline that stopped
    /// happening means the table is now lying about the provider. `bedrock`
    /// pins both `FragmentedToolName` and `CancelAfterStopReason`; this subject
    /// serves the second.
    #[tokio::test]
    #[should_panic(expected = "CancelAfterStopReason is pinned as declined")]
    async fn a_pinned_decline_that_stopped_happening_fails() {
        let subject = Subject {
            declines: &[(
                Scenario::FragmentedToolName,
                "name arrives whole in toolUse start",
            )],
            ..Subject::conforming("bedrock")
        };
        assert_conforms(&subject).await;
    }

    /// The reason string is pinned too, so a subject cannot keep the row and
    /// quietly change what it claims.
    #[tokio::test]
    #[should_panic(expected = "drifted")]
    async fn a_drifted_decline_reason_fails() {
        let subject = Subject {
            declines: &[
                (Scenario::FragmentedToolName, "some other excuse"),
                (
                    Scenario::CancelAfterStopReason,
                    "no observable event between MessageStop and Metadata",
                ),
            ],
            ..Subject::conforming("bedrock")
        };
        assert_conforms(&subject).await;
    }
}
