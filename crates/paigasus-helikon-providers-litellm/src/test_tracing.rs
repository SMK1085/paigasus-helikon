//! Crate-wide WARN-event capture for this crate's `tracing` tests.
//!
//! **Why one shared, process-global subscriber rather than a
//! `with_default` per test.** `tracing` caches one `Interest` per callsite
//! for the whole process, computed against whichever dispatcher happened to
//! be current the first time that callsite was reached. A callsite first
//! reached while *no* subscriber is in scope caches as `Interest::never()`,
//! after which the macro short-circuits and no later thread-local subscriber
//! can ever observe it. Any crate whose tests both (a) install a scoped
//! subscriber to assert on a `warn!` and (b) reach that same `warn!` from
//! other tests that install nothing, therefore has a test decided by thread
//! scheduling.
//!
//! That is not hypothetical here. SMA-619 added such a test to `stream.rs`,
//! and it was measured at **52 failures in 200 runs** of this crate's lib
//! suite before turning `test (macos-latest, stable)` red in CI. Returning
//! `Interest::sometimes()` from `register_callsite` and calling
//! `rebuild_interest_cache()` only narrowed it to 35 in 200 — the rebuild
//! races the sibling tests rather than beating them.
//!
//! Installing the capture as the process-wide default before any assertion
//! runs, and never tearing it down, removes the race at its source: every
//! callsite resolves against a subscriber that enables WARN, and the cache
//! stays correct for the life of the process.
//!
//! **Why it must be shared rather than one per test module.** A process gets
//! exactly one global default. Two modules each calling
//! `set_global_default` would leave whichever lost the race silently
//! uninstalled, and its test asserting against an empty buffer. Every
//! tracing test in this crate goes through [`start`].
//!
//! Per-test isolation comes from the buffer being thread-local: libtest runs
//! each test on its own thread, so a parallel test's warns land in its own
//! buffer rather than this one's.

use std::cell::RefCell;
use std::sync::Once;

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

thread_local! {
    /// WARN-or-above events recorded on *this* thread, as
    /// `(target, rendered fields)`.
    static CAPTURED: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}

/// Renders every field of one event as `name=value;`.
struct FieldSink(String);

impl Visit for FieldSink {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let _ = write!(self.0, "{}={:?};", field.name(), value);
    }
}

/// Records every WARN-or-above event into the emitting thread's buffer.
///
/// Filtering in `enabled` rather than in `on_event` keeps unrelated
/// `debug!`/`trace!`/`info!` calls on the exercised code paths out of the
/// capture entirely, so a routine unrelated log addition cannot make a test
/// that asserts on captured events fail.
struct WarnCapture;

impl<S: tracing::Subscriber + for<'l> LookupSpan<'l>> Layer<S> for WarnCapture {
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        *metadata.level() <= tracing::Level::WARN
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut sink = FieldSink(String::new());
        event.record(&mut sink);
        let target = event.metadata().target().to_owned();
        CAPTURED.with(|c| c.borrow_mut().push((target, sink.0)));
    }
}

static INSTALL: Once = Once::new();

/// Install the capture (once per test binary) and clear this thread's buffer.
///
/// Call at the top of every test that asserts on captured events.
///
/// `set_global_default` is allowed to fail: if some future test installs its
/// own global default first, this capture is not installed and the calling
/// test's assertions fail loudly against an empty buffer rather than passing
/// vacuously. That is the intended failure direction — see the module docs.
pub(crate) fn start() {
    INSTALL.call_once(|| {
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(WarnCapture),
        );
    });
    CAPTURED.with(|c| c.borrow_mut().clear());
}

/// `(target, rendered fields)` for every WARN-or-above event on this thread
/// since [`start`].
pub(crate) fn captured() -> Vec<(String, String)> {
    CAPTURED.with(|c| c.borrow().clone())
}

/// Just the targets of [`captured`], in emission order.
pub(crate) fn targets() -> Vec<String> {
    CAPTURED.with(|c| c.borrow().iter().map(|(t, _)| t.clone()).collect())
}
