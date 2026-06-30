# SMA-452 — runtime-axum follow-ups (design)

**Status:** draft for review
**Ticket:** [SMA-452](https://linear.app/smaschek/issue/SMA-452) — *runtime-axum follow-ups: SSE/WS start-error frame, `--no-default-features` CI gate, EventLog replay perf*
**Related:** SMA-331 (PR #129, the `paigasus-helikon-runtime-axum` 0.1.0 implementation)
**Scope:** `crates/paigasus-helikon-runtime-axum` + one CI job. No public API additions; no core changes.

## Problem

Three non-blocking items were deferred from SMA-331's final review on PR #129. All are localized to the `runtime-axum` crate; the crate shipped `0.1.0` with both acceptance criteria met and the workspace gate green.

1. **SSE/WebSocket close cleanly with no terminal frame on the start-error path.** When `Runner::run_streamed` returns an outer `Err(RunError)`, the writer task records `start_error` and marks the run's `EventLog` terminal *without appending any event*. One-shot (`oneshot_response`) correctly checks `start_error` and returns HTTP 500. But `sse_response` and `handle_socket` (WS) simply subscribe to an empty-but-terminal log: the stream yields nothing and they close cleanly. SMA-331 design §6 mandates "streams emit a final synthetic error frame, then close." This is currently unreachable with the default in-memory `SessionProvider` (which never fails to start), but a custom `SessionProvider`/`Runner` can hit it.

2. **No standing `--no-default-features` CI gate.** `runtime-axum`'s only optional feature is `openapi` (`dep:utoipa`). The feature gating is correct by inspection and builds locally, but CI only compiles the crate under the `--all-features` matrix. A `#[cfg(feature = "openapi")]` regression — e.g. an un-gated `utoipa` reference — would slip through.

3. **`EventLog::subscribe` replays in O(n²).** `subscribe` drains one event per `read_from` call; each `read_from` clones the *entire* retained tail into a fresh `Vec` and then takes only its first element. For N retained events that is O(n²) clones/allocations. Bounded by the per-run cap (default 10k events) so fine in practice, but a pathological large-run replay amplifies it (mild DoS-amplification angle). The backing ring is also created with `VecDeque::with_capacity(max_events.min(64))`, so a production-sized log reallocs several times as it grows.

## Goals / non-goals

**Goals.** Fix all three items with minimal, well-bounded changes; keep every existing test green; add targeted regression tests; keep the workspace gate (`fmt`, `clippy --all-features --all-targets -D warnings`, `test --all-features`, `docs -D warnings`, doc-coverage ≥ 80%) green.

**Non-goals.** No change to the HTTP status model (one-shot start-error stays 500; the 503-for-unavailable-dependency refinement noted in §6 is out of scope). No new public API. No change to the one-shot or `?mode=async` transports beyond what the shared helper implies. No workspace-wide `--no-default-features` build (scoped to `runtime-axum`, the crate with the optional feature).

---

## Task 1 — synthetic terminal frame on streaming transports

### Decision: broad invariant

A streaming transport (SSE or WS) emits a synthetic terminal frame **whenever its subscribe stream ends without having delivered a real `RunCompleted`/`RunFailed`**. The frame is `AgentEvent::RunFailed { error }`, where `error` is the captured `start_error` if present, otherwise a generic `"run ended before producing a terminal event"`.

This is slightly broader than the ticket's literal wording (which names only the `start_error` path). It also covers the panic-mid-stream case (`PanicStreamRunner`: the writer's `TerminalGuard` marks the log terminal with no event appended and `start_error` stays `None`), where the SSE/WS streams currently also close with no terminal frame. The result is a clean, uniform invariant: **every SSE/WS subscriber stream ends with a terminal frame.** The one-shot and `?mode=async` transports are unchanged (one-shot start-error → 500; panic-mid-stream → 200 failed envelope, per the existing `panicking_stream_still_returns_not_hangs` test).

### Why reuse `AgentEvent::RunFailed`

The frame flows through the *existing* serialization paths unchanged: SSE `to_sse_event` tags it `event: run_failed` with the JSON body as `data:`; WS serializes it to a JSON text frame. Clients already handle `RunFailed` because in-stream failures (timeout/cancel) arrive the same way. No new event variant, no transport-only frame shape.

### Mechanics

- **Expose `is_terminal`.** `event_log::is_terminal(&AgentEvent) -> bool` is currently a private free function. Change it to `pub(crate)` so the two handlers can compute `saw_terminal` without duplicating the `matches!` pattern.

- **Add the helper on `RunHandle`** (in `registry.rs`, which gains a `use paigasus_helikon_core::AgentEvent`):

  ```rust
  impl RunHandle {
      /// The synthetic terminal frame a streaming transport must emit when its
      /// subscribe stream ended without delivering a real `RunCompleted`/`RunFailed`.
      ///
      /// Returns `None` when a real terminal was already delivered (`saw_terminal`).
      /// Otherwise returns `RunFailed`, sourced from `start_error` if the run failed
      /// to start, else a generic message (e.g. a stream that panicked mid-run).
      pub(crate) fn synthetic_terminal_frame(&self, saw_terminal: bool) -> Option<AgentEvent> {
          if saw_terminal {
              return None;
          }
          let error = self
              .start_error
              .lock()
              .expect("start_error mutex poisoned")
              .clone()
              .unwrap_or_else(|| "run ended before producing a terminal event".to_owned());
          Some(AgentEvent::RunFailed { error })
      }
  }
  ```

- **SSE (`sse_response`).** Carry `saw_terminal` and a `done` flag through the `unfold` state (alongside the existing event stream and the cancel `DropGuard`). Each real event is forwarded and updates `saw_terminal |= is_terminal(&ev)`. When the live stream returns `None`, call `handle.synthetic_terminal_frame(saw_terminal)`: if `Some(ev)`, yield `to_sse_event(&ev)` once and set `done = true`; the next poll returns `None`. The `DropGuard` is held in the state for the stream's whole lifetime, so client-disconnect cancellation is unaffected.

- **WebSocket (`handle_socket`).** Track `let mut saw_terminal = false;` in the select loop, setting it `true` when a delivered event `is_terminal`. In the stream-ended (`None`) arm, before sending `Message::Close(None)`, call `handle.synthetic_terminal_frame(saw_terminal)`; if `Some(frame)`, serialize and `socket.send(Message::text(..))` it (best-effort — ignore send error, then still send Close). Disconnect semantics are unchanged (WS observers never cancel the run).

### Tests (Task 1)

In `tests/` (using the existing `FailingRunner` from `tests/support/mod.rs`):

- **SSE start-error frame:** a server built with `FailingRunner`, `POST …/runs?stream=sse`; assert the parsed SSE body is exactly one `RunFailed` event carrying the runner's error string, and the response is a clean 200 SSE stream (the HTTP status of an SSE response is 200; the failure is in-band).
- **WS start-error frame:** create a run (the failing run is registered), connect WS to `…/runs/{id}/events`; assert the single text frame is `RunFailed`, followed by a Close. (Construction mirrors `async_run_survives_creator_disconnect`, but the run start-errors.)
- Optionally a unit test on `RunHandle::synthetic_terminal_frame` covering the `saw_terminal=true → None`, `start_error=Some → RunFailed{that}`, and `start_error=None → RunFailed{generic}` branches.

> Note on the WS test: the WS endpoint validates the run id against the registry *before* upgrading. A start-errored run is still `create`d and registered (the writer task records `start_error` and marks terminal but the handle remains in the registry until TTL/cap eviction), so the WS handshake succeeds and the subscriber observes the synthetic frame. The test must obtain the run id — easiest via `?mode=async` against the `FailingRunner` server (returns 202 + `run_id`), then connect WS to that id.

---

## Task 2 — `--no-default-features` CI gate

### Decision: dedicated required job

Add a single-concern job to `.github/workflows/ci.yml`:

```yaml
  build-no-default-features:
    runs-on: ubuntu-latest
    steps:
      # actions/checkout v6.0.2
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0
        with:
          persist-credentials: false
      # dtolnay/rust-toolchain master (no tagged releases)
      - uses: dtolnay/rust-toolchain@67ef31d5b988238dd797d409d6f9574278e20537
        with:
          toolchain: stable
      # Swatinem/rust-cache v2.9.1
      - uses: Swatinem/rust-cache@c19371144df3bb44fab255c43d04cbc2ab54d1c4
      - run: cargo build -p paigasus-helikon-runtime-axum --no-default-features
```

Action SHAs are **reused from the existing `ci.yml`** (they were resolved-and-pinned for SMA-331/PR #129 and remain current); this job introduces no new action, so no re-resolution is needed. The job runs on both `push: [main]` and `pull_request` like the other CI jobs (it is *not* gated behind `if: pull_request` — only `commits` is).

### Make it required

For the gate to actually block a feature-gating regression it must be a required status check. Add the bare job name `build-no-default-features` to:

- `.github/rulesets/main-protection-checks.json` (the canonical required-check declaration), and
- the required-status-check list in `CONTRIBUTING.md` and the matching list in `CLAUDE.md` ("CI" section), so the documented set stays in sync with the ruleset.

This mirrors the precedent that the macOS test job is required because it is the *only* gate exercising the Seatbelt backend — `build-no-default-features` is the only gate exercising the no-default-features build.

### Tests (Task 2)

The job *is* the test. Locally verify with `cargo build -p paigasus-helikon-runtime-axum --no-default-features` (must succeed) — and as a sanity check that the gate has teeth, confirm it currently passes (the gating is already correct; this job prevents future regressions).

---

## Task 3 — O(n) `EventLog` replay + sensible ring capacity

### Batch-drain `subscribe`

Rewrite `EventLog::subscribe` so each `read_from` call's cloned slice is fully drained from a buffer held in the `unfold` state, instead of re-reading the whole tail per event. New state: `{ cursor: u64, pending: std::vec::IntoIter<AgentEvent>, done: bool }`.

Per iteration:

1. If `done`, return `None`.
2. If `pending.next()` yields an event, return it; set `done = is_terminal(&ev)`.
3. Otherwise the buffer is empty — loop:
   - register the wakeup future **before** reading (`notify.notified()` + `enable()`) — the existing lost-wakeup ordering is preserved unchanged;
   - `let slice = log.read_from(cursor);`
   - if `!slice.events.is_empty()`: set `cursor = slice.next_cursor` (already clamps past any eviction gap, since `read_from` computes `next_cursor = max(cursor, first_seq) + events.len()`), set `pending = slice.events.into_iter()`, return the first event (`done = is_terminal(&ev)`);
   - else if `slice.terminal`: return `None` (start-error / mark-terminal-with-empty case — the streaming transports synthesize a frame at the handler layer, Task 1);
   - else: `notif.await;` and loop.

**Complexity.** Each event is cloned exactly once — when first read into a `pending` buffer at the then-current cursor; subsequent `read_from` calls start at the advanced cursor and only clone events appended since. Replay of N retained events is O(n) clones, down from O(n²). The eviction-gap fast-skip that the old per-event `cursor.max(first_seq) + 1` provided is now subsumed by `read_from`'s own `effective_cursor`/`next_cursor` clamping, so no duplicate emissions (covered by the existing `subscribe_skips_gap_on_ring_eviction` test).

The `Box::pin` / `!Unpin` pinning rationale and the `Send + Unpin` return type are unchanged.

### Ring capacity

Change `VecDeque::with_capacity(max_events.min(64))` → `VecDeque::with_capacity(max_events.min(1024))`.

Rationale: the ring never exceeds `max_events`, so `min(1024)` bounds the upfront allocation at ≤ 1024 slots (tens-to-low-hundreds of KB) while eliminating reallocation for the overwhelming majority of real runs (≤ 1024 events). A run that genuinely emits more pays only a handful of cheap amortized-O(1) reallocs up to its cap. We deliberately do **not** use `with_capacity(max_events)` directly: `max_events` is caller-configurable, and a pathological cap (e.g. 1,000,000) would force a large per-run upfront allocation even for runs that emit a few events.

### Tests (Task 3)

- **Large-replay correctness:** append e.g. 5,000 `TokenDelta` events then a terminal, `subscribe(0)`, drain, and assert the count and order are exact. Guards the batch-drain refactor (not a timing/big-O assertion, which would be flaky).
- All existing `event_log` unit tests must remain green unchanged: `read_from_cursor_returns_tail_and_terminal`, `bounded_ring_truncates_head`, `subscribe_replays_then_tails_until_terminal`, `subscribe_skips_gap_on_ring_eviction`, `subscribe_does_not_lose_fast_appended_event`.

---

## Documentation & release

- **mdBook (`docs/book/src/concepts/axum-server.md`):** if it documents the error/streaming model as "streams close cleanly on start-error," bring it into line with "streams emit a final synthetic `run_failed` frame, then close." Verify during implementation; update on the same branch if affected. `mdbook build` must stay clean.
- **Crate `README.md` (`crates/paigasus-helikon-runtime-axum/README.md`):** if it documents the streaming error behavior, update the relevant line. No install/feature/API change, so the `cargo add` snippet is untouched.
- **CONTRIBUTING.md / CLAUDE.md:** add `build-no-default-features` to the documented required-checks list (Task 2).
- **Versioning / CHANGELOG:** `runtime-axum` is an already-released crate (0.1.0) that ships through release-plz's normal flow. Task 1 is an additive behavioral refinement (new terminal frame on a previously-silent path), Task 3 is internal, Task 2 is CI-only — none touch `paigasus-helikon-core` or any public API. **No manual version bump and no manual CHANGELOG edit**: release-plz will patch-bump `runtime-axum` and regenerate its CHANGELOG from the conventional commits on merge. (The same-PR-manual-bump cascade pitfalls do not apply here because nothing is bumped manually.)

## Commit / PR shape

Feature branch `feature/sma-452-runtime-axum-follow-ups-ssews-start-error-frame-no-default`. Logical commits, each conventional-commit-typed with the `SMA-452` prefix:

- `feat(runtime-axum): SMA-452 emit synthetic run_failed frame on SSE/WS start-error close`
- `perf(runtime-axum): SMA-452 batch-drain EventLog replay and size ring capacity`
- `ci: SMA-452 add required --no-default-features build gate for runtime-axum`
- (+ a `docs`/`ci` commit for the CONTRIBUTING/CLAUDE.md/book/README touch-ups if separated)

PR title (gated by `pr-title.yml` — full Conventional Commits prefix + lowercase subject after the `SMA-###`): e.g. `feat(runtime-axum): SMA-452 add streaming start-error frame, no-default-features CI gate, O(n) replay`.

## Risk / rollback

Low risk, fully localized. Task 1 only adds a frame on a path that previously emitted nothing; no existing successful-run output changes. Task 3 is behavior-preserving (guarded by the existing subscribe tests + a new large-replay test). Task 2 is additive CI. Rollback is reverting the branch; no data migrations, no API surface.
