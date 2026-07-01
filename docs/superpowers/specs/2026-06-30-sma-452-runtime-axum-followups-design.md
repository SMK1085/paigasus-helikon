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

- **Add the helper on `RunHandle`** (in `registry.rs`, which gains a `use paigasus_helikon_core::AgentEvent`). It logs at `warn` when it synthesizes, so these otherwise-silent failures (especially the panic-mid-stream case, whose real cause is lost because the writer's `JoinHandle` is never awaited) are diagnosable server-side:

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
          tracing::warn!(
              agent = %self.agent_name,
              %error,
              "run ended without a real terminal event; synthesizing a RunFailed frame for the stream subscriber"
          );
          Some(AgentEvent::RunFailed { error })
      }
  }
  ```

- **SSE (`sse_response`).** The `unfold` state carries `saw_terminal`, a `done` flag, the live event stream, the cancel `DropGuard`, **and a cloned `Arc<RunHandle>`** (cloned before the closure so `synthetic_terminal_frame` is reachable from the stream-end branch — `sse_response` currently captures only the bare `handle.log`/`handle.cancel`, so this is a new capture). Each real event is forwarded and updates `saw_terminal |= is_terminal(&ev)`. When the live stream returns `None`, call `handle.synthetic_terminal_frame(saw_terminal)`: if `Some(ev)`, yield `to_sse_event(&ev)` once and set `done = true`; the next poll returns `None`. The `DropGuard` is held in the state for the stream's whole lifetime, so client-disconnect cancellation is unaffected.

- **WebSocket (`handle_socket`).** Track `let mut saw_terminal = false;` in the select loop, setting it `true` when a delivered event `is_terminal`. In the stream-ended (`None`) arm, before sending `Message::Close(None)`, call `handle.synthetic_terminal_frame(saw_terminal)`; if `Some(frame)`, serialize and `socket.send(Message::text(..))` it (best-effort — ignore send error, then still send Close). `handle` is already an `Arc<RunHandle>` in scope. Disconnect semantics are unchanged (WS observers never cancel the run). The synthetic frame is emitted in the `sub.next() == None` arm, so it does not race the `socket.recv()` arm — a client close still wins via `break` without emitting the frame.

### Tests (Task 1)

Two failure shapes must be covered, because they exercise different branches of the new `saw_terminal` threading:

- **Start-error (zero events, `start_error = Some`)** — via the existing `FailingRunner`.
- **Terminal-less stream after real events (`start_error = None`, generic message)** — the regression-prone branch. Add a new test runner to `tests/support/mod.rs`, e.g. `PartialThenEndRunner`, whose `run_streamed` returns `Ok(streaming)` with a stream that yields exactly one non-terminal event (`AgentEvent::TokenDelta { text: "hi" }`) and then ends with **no** terminal event. This deterministically exercises "≥1 real event delivered, `saw_terminal` stayed false, generic synthetic frame appended once at the end" without depending on `TokioRunner`'s end-of-stream behavior.

Tests (in `tests/`):

- **SSE start-error frame:** server built with `FailingRunner` **and a registered agent** (mirror `start_error_returns_500_not_hang`: `ScriptedAgent { name: "echo", … }` + `.runner(FailingRunner)` — `create_run` resolves the agent at `runs.rs:154` *before* spawning the writer, so the URL's agent name must exist or it 404s before any run). `POST …/agents/echo/runs?stream=sse`; assert the parsed SSE body is exactly one event, that event is the `RunFailed` variant, and its `error` is non-empty (assert on the **variant tag**, not `RunError::MaxIterations`'s exact `Display` text, which is brittle). The HTTP response itself is a clean 200 SSE stream — the failure is in-band.
- **SSE terminal-less frame:** server with `PartialThenEndRunner` + an echo agent; `POST …?stream=sse`; assert the body is exactly `[TokenDelta { text: "hi" }, RunFailed { error: <generic> }]` in that order.
- **WS start-error frame:** server with `FailingRunner` + an echo agent. Obtain the run id via `?mode=async` (`create_async_run` returns 202 + `run_id` — the run is `create`d and registered at `runs.rs:185` before the writer start-errors, and stays in the registry until TTL/cap eviction, so the WS handshake's registry check at `events.rs:58` passes). Connect WS to `…/runs/{id}/events`; assert the single text frame is `RunFailed`, followed by a Close.
- **WS terminal-less frame:** same pattern with `PartialThenEndRunner`; assert frames `[TokenDelta, RunFailed{generic}]` then Close.
- **Unit test** on `RunHandle::synthetic_terminal_frame` covering all three branches: `saw_terminal=true → None`, `start_error=Some → RunFailed{that}`, `start_error=None → RunFailed{generic}`.

The existing happy-path exact-equality tests (`tests/runs.rs` `sse_stream_matches_local_events`, `tests/ws.rs` `ws_replays_completed_run_then_closes`) double as guards that **no** spurious synthetic frame is appended to a normally-completed run.

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
- the required-status-check list in `CONTRIBUTING.md:276` and the matching list in `CLAUDE.md:103`, so the documented set stays in sync with the ruleset.

  **Also fix the pre-existing drift in `CLAUDE.md:103` in the same edit:** that list omits `sessions-it`, which *is* a required context (`main-protection-checks.json:28`, `CONTRIBUTING.md:276`, added in SMA-330/commit c680936) — so the CLAUDE.md list is already out of sync before this change. Add **both** `sessions-it` and `build-no-default-features`.

This mirrors the precedent that the macOS test job is required because it is the *only* gate exercising the Seatbelt backend, and `sessions-it` is required because it is the *only* gate exercising the live session backends — `build-no-default-features` is the only gate exercising the no-default-features build.

### Rollout sequence (the ruleset edit is inert until applied)

Editing `main-protection-checks.json` does **not** by itself make the check required: there is no drift-check CI job (`CONTRIBUTING.md:287`); the ruleset only takes effect when a maintainer runs `scripts/apply-repo-config.sh` (which needs admin and resolves bot App IDs at apply time). And applying a *new* required context is order-sensitive — GitHub will then block **every** open PR that has no report for the new context, including the release-plz `chore: release` PR that publishes this crate's own bump and any in-flight Dependabot PRs whose branches predate the job. So the safe order is:

1. Land this PR (the `ci.yml` job + the ruleset JSON + the CONTRIBUTING/CLAUDE.md edits). The new job runs on the PR itself and on `main` after merge — confirm it reports green in both.
2. **(Maintainer / Sven, post-merge)** run `scripts/apply-repo-config.sh` to apply the updated ruleset. *I cannot run this — it needs repo-admin.* This is surfaced at GATE 2 as a required manual follow-up.
3. After applying, rebase / re-run any open PRs so the new context reports — **notably the release-plz release PR for this very change** (otherwise it deadlocks: it can't merge without a context it never ran). This ties into the standing "watch the release-plz PR after merge" practice.

Until step 2 runs, the job is a visible-but-non-blocking signal — which is acceptable as an interim state; it simply isn't yet enforcing.

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

**Stale-doc cleanup.** `read_from`'s `next_cursor` field doc (`event_log.rs:58-62`) says the subscribe path "computes its own cursor and does not read it" and carries `#[allow(dead_code)]`. After this rewrite `subscribe` *does* read `slice.next_cursor`, so update that doc comment and remove the now-unnecessary `#[allow(dead_code)]`.

### Ring capacity — evaluated and **left at `min(64)`** (deviation from the ticket's literal text)

The ticket lists "`VecDeque::with_capacity(max_events.min(64))` reallocs ~4× for production-sized logs … size the initial capacity sensibly" as part of the fix. **On analysis we are not bumping it**, because a larger initial capacity is a net memory regression for negligible gain:

- `EventLog::new` is called **per run** (`registry.rs:87`), and the registry retains up to `max_runs` *completed* runs (default **1024**, `server.rs:104`) for the full `retention` window (default **300s**, `server.rs:103`). So the initial capacity is multiplied by ~1024 retained runs in steady state.
- Bumping `min(64) → min(1024)` would make **every** run — including a 2-event one-shot echo — preallocate a 1024-slot `VecDeque<AgentEvent>` (and `AgentEvent`'s largest variants embed `Item`/`serde_json::Value`, so each slot is non-trivial). Steady-state worst case rises from a few MB to tens of MB of mostly-empty ring capacity — a ~16× regression — while helping only runs that emit 65–1024 events (saving a few cheap amortized-O(1) doublings). The common short run did **zero** reallocs at `min(64)` already.
- The genuine perf win — eliminating the O(n²) replay — comes **entirely from the `subscribe` batch-drain rewrite above**, independent of capacity. The realloc cost the ticket cites is amortized O(1) and negligible (a handful of small-struct memcpys over a long-running stream).

`with_capacity(max_events)` is also rejected: `max_events` is caller-configurable, so a pathological cap (e.g. 1,000,000) would force a huge per-run upfront allocation. **Conclusion: keep `min(64)`.** *This is the one place the implementation diverges from the ticket text — flagged for sign-off at GATE 1.* (If a small bump is still wanted, `min(128)` is the most that's defensible: it doubles the steady-state floor to ~12 MB worst case while halving medium-run reallocs.)

### Tests (Task 3)

- **Large-replay correctness:** append e.g. 5,000 `TokenDelta` events then a terminal, `subscribe(0)`, drain, and assert the count and order are exact. Guards the batch-drain refactor (not a timing/big-O assertion, which would be flaky).
- All existing `event_log` unit tests must remain green unchanged: `read_from_cursor_returns_tail_and_terminal`, `bounded_ring_truncates_head`, `subscribe_replays_then_tails_until_terminal`, `subscribe_skips_gap_on_ring_eviction`, `subscribe_does_not_lose_fast_appended_event`.

---

## Documentation & release

- **mdBook (`docs/book/src/concepts/axum-server.md`):** the "Replayable runs" section currently documents the three transports but says **nothing** about start-error / terminal-less close behavior — so there is no stale line to patch; **add** a sentence/bullet describing the new invariant: a streaming transport (SSE/WS) whose run ends without a real terminal event emits a final synthetic `run_failed` frame, then closes. `mdbook build docs/book` must stay clean (linkcheck = error).
- **Crate `README.md` (`crates/paigasus-helikon-runtime-axum/README.md`):** check whether it documents the streaming error behavior; add/adjust a line if so. No install/feature/API change, so the `cargo add` snippet is untouched.
- **CONTRIBUTING.md:276 / CLAUDE.md:103:** add `build-no-default-features` to the documented required-checks list, and fix the pre-existing `sessions-it` omission in CLAUDE.md:103 (Task 2).
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

## Adversarial-challenge triage

A fresh Opus spec-challenger attacked this design (verdict: **APPROVE WITH CHANGES**, no blockers). It confirmed the two hard-correctness pieces are sound: the `saw_terminal == false` detector is unreachable for a normally-completed run (the terminal event is always the final append and is never ring-evicted), and the batch-drain is arithmetically identical to the old cursor logic while preserving the lost-wakeup and eviction-gap guarantees. Findings folded in:

| # | Severity | Finding | Action |
|---|----------|---------|--------|
| 1 | MAJOR | Capacity bump ignores the `max_runs` (×1024) multiplier → ~16× steady-state memory regression for marginal gain | **Folded — reversed the bump; keep `min(64)`.** The one deviation from the ticket text; flagged for GATE-1 sign-off. |
| 2 | MAJOR | "Make it required" is inert without `apply-repo-config.sh`, and applying it can deadlock open PRs (incl. the release-plz PR) | **Folded** — added the explicit rollout sequence; the apply step is a maintainer follow-up surfaced at GATE 2. |
| 3 | MAJOR | Tests only covered the zero-event start-error branch, not the regression-prone "real events then no terminal" path | **Folded** — added `PartialThenEndRunner` + SSE/WS tests asserting `[TokenDelta, RunFailed{generic}]`. |
| 4 | MINOR | CLAUDE.md:103 required-check list already missing `sessions-it` | **Folded** — fix it in the same edit. |
| 5 | MINOR | `read_from.next_cursor` doc + `#[allow(dead_code)]` go stale after Task 3 | **Folded** — update the doc, drop the allow. |
| 6 | MINOR | No observability when synthesizing; panic cause is swallowed | **Folded** — `tracing::warn!` in the helper. |
| 7 | MINOR | SSE unfold needs an `Arc<RunHandle>` capture, not just `saw_terminal` | **Folded** — spelled out in the SSE bullet. |
| 8 | MINOR | WS test needs `FailingRunner` **and** a registered agent | **Folded** — corrected the test setup. |
| 9 | MINOR | Book documents nothing to patch — must *add* the behavior | **Folded** — changed to an "add" directive. |
| Q | QUESTION | Assert on the `RunFailed` variant, not `RunError::MaxIterations`'s brittle `Display` text | **Folded** — tests assert the variant tag + non-empty error. |

Transport divergence on start-error (one-shot → 500; SSE → 200 + in-band `RunFailed`; WS → frame + Close) is intentional and matches SMA-331 §6 (failure-as-data for streams). `cargo build` (not `check`/`clippy`/`test`) is the right no-default-features gate — an un-gated `utoipa` reference is a compile error `build` catches.
