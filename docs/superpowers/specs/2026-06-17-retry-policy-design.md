# SMA-393 — Retry policy for transient model errors

**Status:** Design approved (2026-06-17)
**Linear:** [SMA-393](https://linear.app/smaschek/issue/SMA-393)
**Branch:** `feature/sma-393-retry-policy-for-transient-model-errors-retryingmodel`
**Related:** SMA-321 (TokioRunner — deferred this), SMA-320 (structured-output repair — different failure class)

## Problem

Per ADR-10 (*no silent auto-retry inside the loop*), retrying transient transport/provider
errors is an opt-in, application-layer concern. The `ModelError` rustdoc already promises
`RunConfig::retry_policy` ("lands with the runner ticket"), but SMA-321 deliberately omitted
that field rather than ship an inert knob (SMA-321 review, H4). So today there is no retry
support at all.

This ticket adds the retry mechanism. It is distinct from SMA-320 (one-shot *repair* of
structured-output schema-validation failures) — that is a different failure class.

## Key constraints discovered in the code

1. **The runner cannot reach the model.** `TokioRunner` wraps the agent's *event stream* with
   cancel/timeout at the boundary (`controlled()` in `runtime-tokio/src/lib.rs`); it never calls
   `model.invoke()` and only ever holds `&dyn Agent`. So a `RunConfig` knob could not be
   auto-applied by the runner — it cannot wrap the agent's model.

2. **Sleeping requires a runtime; core must stay runtime-free.** The model is invoked deep in the
   **core** loop driver (`agent.rs:917`), which cannot sleep — this is exactly why `RunConfig::timeout`
   is documented `[runner-scoped]`. Backoff timers (`tokio::time`) must stay out of core.

3. **`LlmAgent` is generic over its model.** `LlmAgent<Ctx, M, …>` holds `Arc<M>` where `M: Model`,
   so a decorator `RetryingModel<M>` that wraps any `Model` (and is itself a `Model`) composes
   cleanly at model-construction time.

4. **Providers surface transient errors *in-stream*, not as the outer `Err`.** Both `OpenAiModel`
   and the Anthropic model return `Ok(stream)` and yield `Transport` / `RateLimited{429}` /
   `Unavailable{503}` as the **first in-stream `Err` item** — the HTTP send happens lazily on first
   poll. The outer `invoke()` `Err` is reserved for request-construction failures (non-retryable).
   A decorator that retried only the outer error would retry nothing useful; it must **peek the
   stream**.

## Decision: decorator-only, no `RunConfig` field

Because (1)+(2) make an auto-applied `RunConfig::retry_policy` architecturally impossible without
either an `Agent`-trait model-rewrap seam or putting retry back "inside the loop" (against ADR-10),
we resolve the SMA-321 H4 tension by applying the *principle* ("no inert knob") to its conclusion:

- Ship the **mechanism** — `RetryPolicy` + `RetryingModel<M>` — in `paigasus-helikon-runtime-tokio`.
- **Do not** add `RunConfig::retry_policy`.
- Reconcile the docs: rewrite the `ModelError` rustdoc (core) and the two provider `error.rs`
  module docstrings that promise `RunConfig::retry_policy` to point at `RetryingModel`/`RetryPolicy`.

This keeps retry as a composition-layer concern (a `Model` wrapping a `Model`), which is precisely
ADR-10's "application-layer, not inside the loop."

### Deviation from the ticket & the ergonomic shift it implies

This is a **deliberate deviation** from the ticket's literal scope ("Add `RunConfig::retry_policy`
and wire it"), approved during brainstorming. SMA-393 should be reconciled to record the decision
(strike the "add the field" line); until then, this spec is the authoritative in-repo record. The
consequence to document for users:

- **Retry is configured differently from every other run knob.** `timeout` / `max_turns` /
  `parallel_tool_call_limit` are per-run `RunConfig`; retry is **model-construction-time** — wrap the
  model in `RetryingModel` before building the agent. So retry cannot be varied per-invocation, and a
  user scanning `RunConfig` for retry options will not find it. The doc reconciliation (pointing
  `ModelError` + the provider docs at `RetryingModel`) is the discoverability mitigation; it must be
  prominent (README + mdBook resilience page), and must state the one-line "why": the runner only
  holds `&dyn Agent` and can't reach the model, and core can't sleep.
- **Future seam (noted, not built):** a later `Agent`-trait model-rewrap seam could enable
  `RunConfig`-driven retry if per-invocation variation is ever wanted. Out of scope here.

## Architecture

All new code in **`paigasus-helikon-runtime-tokio`**, in a new `retry` module, re-exported from
`lib.rs`. New dependency: `fastrand` (MIT/Apache-2.0, zero transitive deps — already covered by the
deny allowlist) for jitter, added to `[workspace.dependencies]` and the crate.

### `RetryPolicy` — pure data + deterministic math

```rust
pub struct RetryPolicy {
    max_attempts: u32,    // total tries incl. the first; >= 1
    base_delay: Duration,
    multiplier: f64,
    max_delay: Duration,  // per-attempt cap
    jitter: bool,
}
```

- **Defaults** (`new()` / `Default`): `max_attempts = 3`, `base_delay = 500ms`, `multiplier = 2.0`,
  `max_delay = 30s`, `jitter = true`. Builder methods: `.max_attempts(n)`, `.base_delay(d)`,
  `.multiplier(f)`, `.max_delay(d)`, `.jitter(bool)`.
- **Retryable set** (hardcoded predicate): `RateLimited`, `Unavailable`, `Transport` → retryable;
  `ContextLengthExceeded`, `Refused`, `Other` → not. (Hardcoded by decision; not caller-configurable
  in this ticket — the set is the ticket's prescription.)
- **The one math method**, with the RNG injected as a fraction so it is fully unit-testable without
  randomness or a clock:

  ```rust
  /// `jitter_fraction` in [0.0, 1.0). `None` when no retry is warranted.
  pub fn next_delay(&self, attempt: u32, err: &ModelError, jitter_fraction: f64) -> Option<Duration>
  ```

  Returns `None` when `!retryable(err)` **or** no attempts remain (`attempt + 1 >= max_attempts`).
  Otherwise:
  - `ceiling = min(base_delay * multiplier^attempt, max_delay)`
  - `jittered = if jitter { ceiling * jitter_fraction } else { ceiling }`
  - for `RateLimited { retry_after_ms: Some(ms) }` → `max(jittered, Duration::from_millis(ms))`
    (honors "wait ≥ the hinted delay")
  - else → `jittered`

  `attempt` is 0-based: `0` = the delay to wait after the first failure.

### `RetryingModel<M>` — the decorator

```rust
pub struct RetryingModel<M> { inner: M, policy: RetryPolicy }
impl<M> RetryingModel<M> { pub fn new(inner: M, policy: RetryPolicy) -> Self { … } }

#[async_trait]
impl<M: Model> Model for RetryingModel<M> {
    async fn invoke(&self, request: ModelRequest, cancel: CancellationToken)
        -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError>;
    fn capabilities(&self) -> ModelCapabilities { self.inner.capabilities() }
    fn provider(&self) -> &str { self.inner.provider() }
    fn model(&self) -> &str { self.inner.model() }
}
```

Like the real providers, the work happens inside a lazily-polled `async_stream::stream!`, so the
decorator's outer `invoke()` itself returns `Ok`. Per attempt (0-based):

1. Call `inner.invoke(request.clone(), cancel.clone())` — `ModelRequest: Clone`, so a retry re-sends
   the identical request.
2. **Peek the first item** — this is the retry watermark:
   - `Some(Err(e))` → `policy.next_delay(attempt, &e, fastrand::f64())`:
     - `Some(delay)` → back off (see cancellation) then retry the next attempt.
     - `None` → yield the `Err(e)` and end (non-retryable, or attempts exhausted).
   - `Some(Ok(ev))` → content/usage has started; yield `ev`, then forward the remainder of the inner
     stream **verbatim**. A later mid-stream `Err` is **not** retried — already-emitted output
     cannot be un-emitted (and usage cannot be double-counted).
   - `None` → empty stream; forward.
   - Outer `Err(e)` from step 1 goes through the same `next_delay` path (non-retryable build errors
     are forwarded).

**Semantic note (benign):** because the decorator's own `invoke()` always returns `Ok(stream)`
(lazy), a `RetryingModel` **never** returns an outer `invoke()` `Err`. A wrapped provider's outer
build error (the providers do `build_body(...)?` *before* their `stream!`) is re-surfaced as the
*first in-stream `Err`* instead. This is functionally equivalent for the driver: both the outer-Err
site (`agent.rs:923`) and the in-stream-Err site (`agent.rs:990`) map to `AgentError::Model(e) →
RunFailed`. Verified against the code.

### Cancellation

The backoff sleep (and the invoke/peek await) are wrapped in `tokio::select!` against
`cancel.cancelled()`. On cancellation the stream ends promptly **without** a `Finish` event, matching
the provider convention for cancellation.

### Disabled by default

Retry is purely opt-in: not wrapping the model = today's behavior (ADR-10 preserved). `max_attempts(1)`
yields a pure passthrough (one try, never retries). There is no global enable/disable knob.

## Testing (TDD)

`tokio = ["full"]` is already on, so the sleep and `tokio::time::pause()` (deterministic backoff in
tests) need no manifest change. A `MockModel` test double yields its first-item `Err` for the first
N−1 invokes then succeeds (matching the real providers' in-stream error shape), counting invokes.

| Test | AC | Assertion |
|------|----|-----------|
| N−1 `Unavailable` then success | AC1 | run completes; exactly N invokes |
| `Refused` (non-retryable) | AC1 | exactly 1 invoke; error forwarded |
| `RateLimited { retry_after_ms }` | AC2 | under paused time, elapsed ≥ hint |
| cancel during backoff | AC3 | stream ends promptly, no long wait |
| mid-stream `Err` after first `Ok` | — | not retried; 1 invoke; error forwarded |
| `next_delay` pure-math table | AC4 | fraction 0.0 / ~1.0, cap, hint, exhaustion → `None` |

CI gates (`fmt`, `clippy --all-features`, `test --all-features`, `doc`, doc-coverage) must stay green.

## Docs & release

- Update `crates/paigasus-helikon-runtime-tokio/README.md` (its crates.io page) with the
  `RetryingModel` / `RetryPolicy` surface. State that retry is opt-in by **wrapping the model** (not a
  `RunConfig` knob), and that it covers **connection establishment** — once a stream has begun
  emitting content a mid-stream drop is *not* retried (see Out of scope). Same clarification on the
  mdBook resilience page.
- Update the relevant mdBook page (`docs/book/src/*`) — resilience / runtime section.
- Doc-only edits across **three** crates, all of which carry the stale `RunConfig::retry_policy`
  promise (verified): `ModelError` rustdoc (`core/src/model.rs`), `providers-openai/src/error.rs`
  module docstring, and `providers-anthropic/src/error.rs` module docstring — each repointed at
  `RetryingModel` / `RetryPolicy`.
- `fastrand` added to `[workspace.dependencies]` + `runtime-tokio` deps.
- Normal additive `feat` → release-plz bumps `runtime-tokio`. **Release wrinkle (accounts for all
  three doc-edited crates):** a squash-merged `feat(runtime-tokio)` PR that also touches
  `core/src` + both `providers-*/src` folds those doc-only edits into the release attribution and can
  re-release `core`, `providers-openai`, **and** `providers-anthropic` (all doc-only, harmless;
  cf. the squash-merge-folds-docs pattern). **Decision for the plan:** either (a) knowingly accept the
  three harmless re-releases in one `feat` PR, or (b) isolate the doc reconciliation into a separate
  `docs(...)`/`chore(...)` change (a non-bumping commit type, landed via a path that preserves it)
  so only `runtime-tokio` bumps. Recommend (a) for a small ticket unless churn-aversion argues for (b).

## Out of scope

- `RunConfig::retry_policy` field (decided against — see Decision).
- Caller-configurable retryable-variant set.
- Retrying after content has begun streaming (impossible without duplicating output).
- Retry for non-tokio runners (Temporal / AgentCore) — those land with their own runtime crates.
