# Retry Policy for Transient Model Errors — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in `RetryingModel<M>` decorator (+ a pure-data `RetryPolicy`) in `paigasus-helikon-runtime-tokio` that retries transient model errors (`Unavailable`, `RateLimited`, `Transport`) with exponential backoff + jitter, cancellation-aware, disabled by default.

**Architecture:** Retry is a composition-layer concern — a `Model` that wraps a `Model` (ADR-10: not "inside the loop"). The backoff timer (`tokio::time`) lives in `runtime-tokio`, keeping `core` runtime-free. There is intentionally **no** `RunConfig::retry_policy` field: the runner only holds `&dyn Agent` and can't reach the agent's model, and core can't sleep — so the decorator is composed at model-construction time. See the spec: `docs/superpowers/specs/2026-06-17-retry-policy-design.md`.

**Tech Stack:** Rust, `tokio` (time/select), `async-stream`, `async-trait`, `fastrand` (new dep, jitter), `paigasus-helikon-core` `Model` trait.

**Key facts (verified against the code):**
- Providers (`OpenAiModel`, Anthropic) return `Ok(stream)` and yield transient errors as the **first in-stream `Err`** (HTTP send is lazy). The outer `invoke()` `Err` is only request-construction failure. So the decorator must **peek the first stream item**, not just the outer `Result`.
- `Model::invoke(&self, request: ModelRequest, cancel: CancellationToken) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError>`; `fn capabilities()`, `fn provider() -> &str`, `fn model() -> &str`. `ModelRequest: Clone`.
- Retryable `ModelError` variants: `RateLimited { retry_after_ms: Option<u64> }`, `Unavailable`, `Transport(String)`. Non-retryable: `ContextLengthExceeded`, `Refused { reason }`, `Other`.
- The returned stream must be `'static`, so the decorator stores `inner: Arc<M>` (mirrors `LlmAgent`'s `Arc<M>`) and moves an `Arc` clone into the lazy stream.
- The facade re-exports the whole crate (`pub use paigasus_helikon_runtime_tokio as runtime_tokio;`), so new public items flow through automatically — **no facade edit needed**.
- `tokio`'s `full` feature excludes `test-util`; paused-time tests add it as a dev feature.
- Allowed commit scopes include `runtime-tokio`, `core`, `providers-openai`, `providers-anthropic`, `workspace`, `plan`. Types: `feat` (Minor), `docs`/`test`/`build` (None).

---

## File map

- **Modify** `Cargo.toml` (root) — add `fastrand = "2"` to `[workspace.dependencies]`.
- **Modify** `crates/paigasus-helikon-runtime-tokio/Cargo.toml` — add `fastrand` dep; add `test-util` to the dev `tokio`.
- **Create** `crates/paigasus-helikon-runtime-tokio/src/retry.rs` — `RetryPolicy` + `RetryingModel<M>` + tests.
- **Modify** `crates/paigasus-helikon-runtime-tokio/src/lib.rs` — `pub mod retry;` + `pub use retry::{RetryPolicy, RetryingModel};`.
- **Modify** `crates/paigasus-helikon-core/src/model.rs` — repoint the `ModelError` rustdoc.
- **Modify** `crates/paigasus-helikon-providers-openai/src/error.rs` — repoint module docstring.
- **Modify** `crates/paigasus-helikon-providers-anthropic/src/error.rs` — repoint module docstring.
- **Modify** `crates/paigasus-helikon-runtime-tokio/README.md` — add a "Retrying transient errors" section.
- **Modify** `docs/book/src/concepts/model-providers.md` — add a retry subsection.

---

## Task 1: Add the `fastrand` dependency + tokio `test-util`

**Files:**
- Modify: `Cargo.toml` (root `[workspace.dependencies]`)
- Modify: `crates/paigasus-helikon-runtime-tokio/Cargo.toml`

- [ ] **Step 1: Add `fastrand` to the workspace dependency table**

In root `Cargo.toml`, under `[workspace.dependencies]`, add this line (e.g. right after the `eventsource-stream    = "0.2"` line):

```toml
fastrand              = "2"
```

- [ ] **Step 2: Add `fastrand` to the crate and `test-util` to dev `tokio`**

In `crates/paigasus-helikon-runtime-tokio/Cargo.toml`, add `fastrand` to `[dependencies]` (after the `anyhow` line):

```toml
fastrand     = { workspace = true }
```

And change the dev-dependency `tokio` line (under `[dev-dependencies]`) from:

```toml
tokio      = { workspace = true }
```

to:

```toml
tokio      = { workspace = true, features = ["test-util"] }
```

- [ ] **Step 3: Verify the workspace still resolves and builds**

Run: `cargo build -p paigasus-helikon-runtime-tokio`
Expected: builds clean (a not-yet-used `fastrand` is fine — Rust does not warn on unused deps).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/paigasus-helikon-runtime-tokio/Cargo.toml
git commit -m "build(runtime-tokio): SMA-393 add fastrand dep and tokio test-util"
```

---

## Task 2: `RetryPolicy` — pure data + deterministic backoff math (TDD)

**Files:**
- Create: `crates/paigasus-helikon-runtime-tokio/src/retry.rs`
- Modify: `crates/paigasus-helikon-runtime-tokio/src/lib.rs`

- [ ] **Step 1: Create `retry.rs` with the module doc and the failing tests**

Create `crates/paigasus-helikon-runtime-tokio/src/retry.rs` with exactly this (the `RetryPolicy` impl comes in Step 3 — for now only the module doc, the minimal imports `RetryPolicy` needs, and the tests, so it fails to compile). The decorator's extra imports are added in Task 3:

```rust
//! Opt-in retry for transient model errors: a pure-data [`RetryPolicy`] and a
//! [`RetryingModel`] decorator that retries `Model::invoke` with backoff.
//!
//! Retry is configured by wrapping a [`paigasus_helikon_core::Model`] (not via
//! `RunConfig`): the runner only holds `&dyn Agent` and can't reach the model,
//! and core can't sleep. Disabled unless you wrap. See ADR-10.

use std::time::Duration;

use paigasus_helikon_core::ModelError;

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        let p = RetryPolicy::new();
        // 3 total attempts ⇒ a retry is warranted after failures 0 and 1, not 2.
        let err = ModelError::Unavailable;
        assert!(p.next_delay(0, &err, 0.0).is_some());
        assert!(p.next_delay(1, &err, 0.0).is_some());
        assert!(p.next_delay(2, &err, 0.0).is_none(), "attempts exhausted");
    }

    #[test]
    fn non_retryable_variants_never_retry() {
        let p = RetryPolicy::new();
        assert!(p.next_delay(0, &ModelError::Refused { reason: "x".into() }, 0.0).is_none());
        assert!(p.next_delay(0, &ModelError::ContextLengthExceeded, 0.0).is_none());
        assert!(p
            .next_delay(0, &ModelError::Other(anyhow::anyhow!("x")), 0.0)
            .is_none());
    }

    #[test]
    fn retryable_variants_retry() {
        let p = RetryPolicy::new();
        assert!(p.next_delay(0, &ModelError::Unavailable, 0.0).is_some());
        assert!(p.next_delay(0, &ModelError::Transport("reset".into()), 0.0).is_some());
        assert!(p
            .next_delay(0, &ModelError::RateLimited { retry_after_ms: None }, 0.0)
            .is_some());
    }

    #[test]
    fn full_jitter_scales_between_zero_and_ceiling() {
        let p = RetryPolicy::new().base_delay(Duration::from_secs(1)).multiplier(2.0);
        let err = ModelError::Unavailable;
        // attempt 1 ⇒ ceiling = 1s * 2^1 = 2s.
        let lo = p.next_delay(1, &err, 0.0).unwrap();
        let hi = p.next_delay(1, &err, 0.999).unwrap();
        assert_eq!(lo, Duration::ZERO);
        assert!(hi <= Duration::from_secs(2) && hi > Duration::from_millis(1900));
    }

    #[test]
    fn backoff_is_capped_at_max_delay() {
        let p = RetryPolicy::new()
            .base_delay(Duration::from_secs(10))
            .multiplier(10.0)
            .max_delay(Duration::from_secs(5))
            .max_attempts(4) // attempt index 2 must be within budget, else the exhaustion guard returns None
            .jitter(false);
        // Without the cap this would be 10s * 10^2 = 1000s; cap pins it at 5s.
        assert_eq!(
            p.next_delay(2, &ModelError::Unavailable, 0.0).unwrap(),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn rate_limited_waits_at_least_the_hint() {
        let p = RetryPolicy::new().base_delay(Duration::from_millis(1)).jitter(true);
        let err = ModelError::RateLimited { retry_after_ms: Some(5_000) };
        // Even at the smallest jitter draw, the hint dominates the tiny backoff.
        let d = p.next_delay(0, &err, 0.0).unwrap();
        assert!(d >= Duration::from_millis(5_000));
    }

    #[test]
    fn max_attempts_one_is_passthrough() {
        let p = RetryPolicy::new().max_attempts(1);
        assert!(p.next_delay(0, &ModelError::Unavailable, 0.0).is_none());
    }
}
```

- [ ] **Step 2: Wire the module into `lib.rs`, then run the tests to confirm they fail**

In `crates/paigasus-helikon-runtime-tokio/src/lib.rs`, immediately after the `use paigasus_helikon_core::{ … };` import block (around line 17), add (only `RetryPolicy` for now — `RetryingModel` is added in Task 3):

```rust
pub mod retry;
pub use retry::RetryPolicy;
```

Run: `cargo test -p paigasus-helikon-runtime-tokio --lib`
Expected: FAIL to compile — `cannot find type RetryPolicy` in `retry` (the type is referenced by the tests/re-export but not yet defined).

- [ ] **Step 3: Implement `RetryPolicy`**

In `retry.rs`, insert this **above** the `#[cfg(test)] mod policy_tests` block:

```rust
/// Declarative policy for retrying transient [`ModelError`]s. Pure data — the
/// actual backoff sleep is performed by [`RetryingModel`].
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    max_attempts: u32,
    base_delay: Duration,
    multiplier: f64,
    max_delay: Duration,
    jitter: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(500),
            multiplier: 2.0,
            max_delay: Duration::from_secs(30),
            jitter: true,
        }
    }
}

impl RetryPolicy {
    /// A policy with the defaults: 3 attempts, 500ms base, ×2 growth, 30s cap, jitter on.
    pub fn new() -> Self {
        Self::default()
    }

    /// Total attempts **including the first**. `1` disables retrying (passthrough). Clamped to ≥ 1.
    pub fn max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n.max(1);
        self
    }

    /// Base backoff (the delay after the first failure, before jitter/cap).
    pub fn base_delay(mut self, d: Duration) -> Self {
        self.base_delay = d;
        self
    }

    /// Exponential growth factor applied per attempt.
    pub fn multiplier(mut self, m: f64) -> Self {
        self.multiplier = m;
        self
    }

    /// Per-attempt cap on the computed backoff.
    pub fn max_delay(mut self, d: Duration) -> Self {
        self.max_delay = d;
        self
    }

    /// Toggle full jitter (uniform in `[0, ceiling)`). On by default.
    pub fn jitter(mut self, on: bool) -> Self {
        self.jitter = on;
        self
    }

    /// Whether `err` is a transient variant worth retrying: `RateLimited`,
    /// `Unavailable`, `Transport`. Never `ContextLengthExceeded`, `Refused`, `Other`.
    pub fn is_retryable(err: &ModelError) -> bool {
        matches!(
            err,
            ModelError::RateLimited { .. } | ModelError::Unavailable | ModelError::Transport(_)
        )
    }

    /// The backoff to wait before the next attempt, or `None` if no retry is
    /// warranted (non-retryable error, or attempts exhausted).
    ///
    /// `attempt` is 0-based (`0` = the wait after the first failure).
    /// `jitter_fraction` must be in `[0.0, 1.0)`; callers pass `fastrand::f64()`.
    /// For `RateLimited { retry_after_ms: Some(ms) }` the result is at least `ms`.
    pub fn next_delay(
        &self,
        attempt: u32,
        err: &ModelError,
        jitter_fraction: f64,
    ) -> Option<Duration> {
        if !Self::is_retryable(err) || attempt + 1 >= self.max_attempts {
            return None;
        }
        let grown = self.base_delay.as_secs_f64() * self.multiplier.powi(attempt as i32);
        let cap = self.max_delay.as_secs_f64();
        let ceiling = if grown.is_finite() { grown.min(cap) } else { cap };
        let secs = if self.jitter { ceiling * jitter_fraction } else { ceiling };
        let mut delay = Duration::from_secs_f64(secs.max(0.0));
        if let ModelError::RateLimited {
            retry_after_ms: Some(ms),
        } = err
        {
            delay = delay.max(Duration::from_millis(*ms));
        }
        Some(delay)
    }
}
```

- [ ] **Step 4: Run the policy tests to verify they pass**

Run: `cargo test -p paigasus-helikon-runtime-tokio --lib policy_tests`
Expected: the crate compiles and all 7 `policy_tests::*` PASS. (The crate has no `RetryingModel` yet — that's Task 3 — but nothing references it, so the build is clean.)

- [ ] **Step 5: Lint, format, and commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-tokio --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-tokio/src/retry.rs crates/paigasus-helikon-runtime-tokio/src/lib.rs
git commit -m "feat(runtime-tokio): SMA-393 add RetryPolicy backoff/jitter math"
```

Expected: fmt clean, clippy clean (no unused imports — only `Duration`/`ModelError` are imported), commit succeeds.

---

## Task 3: `RetryingModel<M>` decorator (TDD)

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/src/retry.rs`
- Modify: `crates/paigasus-helikon-runtime-tokio/src/lib.rs` (re-export already added in Task 2)

- [ ] **Step 1: Add the decorator's imports + the scripted mock + failing behavioral tests**

First, extend the module-level imports in `retry.rs` — replace the two existing `use` lines:

```rust
use std::time::Duration;

use paigasus_helikon_core::ModelError;
```

with the full set the decorator needs:

```rust
use std::sync::Arc;
use std::time::Duration;

use async_stream::stream;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream::StreamExt as _;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};
```

Then append this second test module to the **end** of `retry.rs`:

```rust
#[cfg(test)]
mod decorator_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use paigasus_helikon_core::FinishReason;

    /// What the i-th `invoke` of [`ScriptModel`] yields.
    #[derive(Clone)]
    enum Resp {
        /// The stream's first (and only) item is this error.
        ErrFirst(ModelError),
        /// `TokenDelta("ok")` then `Finish(Stop)`.
        Ok,
        /// `TokenDelta("partial")` then this error (mid-stream).
        OkThenErr(ModelError),
    }

    /// A `Model` that replays a fixed script, one entry per `invoke`, counting calls.
    struct ScriptModel {
        calls: Arc<AtomicUsize>,
        script: Vec<Resp>,
    }

    impl ScriptModel {
        fn new(script: Vec<Resp>) -> Arc<Self> {
            Arc::new(Self {
                calls: Arc::new(AtomicUsize::new(0)),
                script,
            })
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Model for ScriptModel {
        async fn invoke(
            &self,
            _request: ModelRequest,
            _cancel: CancellationToken,
        ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
            let idx = self.calls.fetch_add(1, Ordering::SeqCst);
            let resp = self.script.get(idx).cloned().unwrap_or(Resp::Ok);
            let s = stream! {
                match resp {
                    Resp::ErrFirst(e) => yield Err(e),
                    Resp::Ok => {
                        yield Ok(ModelEvent::TokenDelta { text: "ok".to_owned() });
                        yield Ok(ModelEvent::Finish { reason: FinishReason::Stop });
                    }
                    Resp::OkThenErr(e) => {
                        yield Ok(ModelEvent::TokenDelta { text: "partial".to_owned() });
                        yield Err(e);
                    }
                }
            };
            Ok(Box::pin(s))
        }

        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities::default()
        }
    }

    fn zero_backoff() -> RetryPolicy {
        RetryPolicy::new().base_delay(Duration::ZERO).jitter(false)
    }

    async fn drain(
        mut s: BoxStream<'static, Result<ModelEvent, ModelError>>,
    ) -> Vec<Result<ModelEvent, ModelError>> {
        let mut out = Vec::new();
        while let Some(item) = s.next().await {
            out.push(item);
        }
        out
    }

    #[tokio::test]
    async fn retries_until_success() {
        let model = ScriptModel::new(vec![
            Resp::ErrFirst(ModelError::Unavailable),
            Resp::ErrFirst(ModelError::Unavailable),
            Resp::Ok,
        ]);
        let retrying = RetryingModel::shared(Arc::clone(&model), zero_backoff().max_attempts(3));
        let items = drain(
            retrying
                .invoke(ModelRequest::new(), CancellationToken::new())
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(model.calls(), 3);
        assert!(items.iter().all(Result::is_ok));
        assert!(matches!(items[0], Ok(ModelEvent::TokenDelta { ref text }) if text == "ok"));
    }

    #[tokio::test]
    async fn non_retryable_is_not_retried() {
        let model = ScriptModel::new(vec![
            Resp::ErrFirst(ModelError::Refused { reason: "policy".to_owned() }),
            Resp::Ok,
        ]);
        let retrying = RetryingModel::shared(Arc::clone(&model), zero_backoff());
        let items = drain(
            retrying
                .invoke(ModelRequest::new(), CancellationToken::new())
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(model.calls(), 1);
        assert!(matches!(items.last(), Some(Err(ModelError::Refused { .. }))));
    }

    #[tokio::test]
    async fn error_after_content_is_not_retried() {
        let model = ScriptModel::new(vec![
            Resp::OkThenErr(ModelError::Transport("reset".to_owned())),
            Resp::Ok,
        ]);
        let retrying = RetryingModel::shared(Arc::clone(&model), zero_backoff());
        let items = drain(
            retrying
                .invoke(ModelRequest::new(), CancellationToken::new())
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(model.calls(), 1, "must not retry once content has streamed");
        assert!(matches!(items[0], Ok(ModelEvent::TokenDelta { .. })));
        assert!(matches!(items[1], Err(ModelError::Transport(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_aborts_backoff_promptly() {
        let model = ScriptModel::new(vec![
            Resp::ErrFirst(ModelError::Unavailable),
            Resp::Ok,
        ]);
        // A 1-hour backoff: the test would hang if cancellation didn't abort it.
        let policy = RetryPolicy::new()
            .base_delay(Duration::from_secs(3600))
            .jitter(false);
        let cancel = CancellationToken::new();
        let retrying = RetryingModel::shared(Arc::clone(&model), policy);
        let mut stream = retrying
            .invoke(ModelRequest::new(), cancel.clone())
            .await
            .unwrap();
        cancel.cancel();
        // First poll: invoke #1 fails, enters backoff, cancellation wins → stream ends.
        assert!(stream.next().await.is_none());
        assert_eq!(model.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limit_hint_is_awaited_then_retries() {
        let model = ScriptModel::new(vec![
            Resp::ErrFirst(ModelError::RateLimited { retry_after_ms: Some(10_000) }),
            Resp::Ok,
        ]);
        let policy = RetryPolicy::new()
            .base_delay(Duration::from_millis(1))
            .jitter(false);
        let retrying = RetryingModel::shared(Arc::clone(&model), policy);
        // Under paused time the 10s hint auto-advances; the retry then succeeds.
        let items = drain(
            retrying
                .invoke(ModelRequest::new(), CancellationToken::new())
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(model.calls(), 2);
        assert!(matches!(items[0], Ok(ModelEvent::TokenDelta { ref text }) if text == "ok"));
    }

    #[tokio::test]
    async fn forwards_capabilities_provider_and_model() {
        // ScriptModel uses the defaults; this proves the decorator delegates, not overrides.
        let model = ScriptModel::new(vec![Resp::Ok]);
        let retrying = RetryingModel::shared(Arc::clone(&model), zero_backoff());
        assert_eq!(retrying.provider(), "unknown");
        assert_eq!(retrying.model(), "");
        assert_eq!(retrying.capabilities(), ModelCapabilities::default());
    }
}
```

- [ ] **Step 2: Run the decorator tests to confirm they fail**

Run: `cargo test -p paigasus-helikon-runtime-tokio --lib decorator_tests`
Expected: FAIL to compile — `cannot find … RetryingModel` / `RetryingModel::shared`.

- [ ] **Step 3: Implement `RetryingModel` and extend the re-export**

First, in `crates/paigasus-helikon-runtime-tokio/src/lib.rs`, extend the re-export from `pub use retry::RetryPolicy;` to:

```rust
pub use retry::{RetryPolicy, RetryingModel};
```

Then in `retry.rs`, insert this **between** the `RetryPolicy` impl and the `#[cfg(test)] mod policy_tests` block:

```rust
/// A [`Model`] decorator that retries transient errors per a [`RetryPolicy`],
/// with exponential backoff + jitter and cancellation-aware sleeps.
///
/// Retry covers **connection establishment**: a retryable error that is the
/// *first* item from the wrapped model (before any event has streamed out) is
/// retried. Once content has begun streaming, a later error is forwarded —
/// already-emitted output cannot be un-emitted and usage cannot be
/// double-counted. Disabled unless you wrap; `RetryPolicy::max_attempts(1)`
/// is a passthrough.
pub struct RetryingModel<M> {
    inner: Arc<M>,
    policy: RetryPolicy,
}

impl<M> RetryingModel<M> {
    /// Wrap `inner`, retrying per `policy`.
    pub fn new(inner: M, policy: RetryPolicy) -> Self {
        Self {
            inner: Arc::new(inner),
            policy,
        }
    }

    /// Wrap an already-shared `inner` (e.g. to reuse it across agents).
    pub fn shared(inner: Arc<M>, policy: RetryPolicy) -> Self {
        Self { inner, policy }
    }
}

/// Sleep for `d`, racing cancellation. Returns `false` if cancellation fired first.
async fn backoff(cancel: &CancellationToken, d: Duration) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => false,
        () = tokio::time::sleep(d) => true,
    }
}

#[async_trait]
impl<M: Model + 'static> Model for RetryingModel<M> {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let inner = Arc::clone(&self.inner);
        let policy = self.policy.clone();

        let s = stream! {
            let mut attempt: u32 = 0;
            loop {
                // (1) invoke, racing cancellation.
                let invoked = tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    r = inner.invoke(request.clone(), cancel.clone()) => r,
                };

                let mut model_stream = match invoked {
                    Ok(s) => s,
                    Err(e) => {
                        // Outer build-error path; routed through the same policy.
                        match policy.next_delay(attempt, &e, fastrand::f64()) {
                            Some(d) => {
                                if !backoff(&cancel, d).await {
                                    return;
                                }
                                attempt += 1;
                                continue;
                            }
                            None => {
                                yield Err(e);
                                return;
                            }
                        }
                    }
                };

                // (2) Peek the first item — the retry watermark.
                let first = tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    x = model_stream.next() => x,
                };

                match first {
                    None => return, // empty stream
                    Some(Ok(ev)) => {
                        // Content started: forward this event and the rest verbatim.
                        yield Ok(ev);
                        loop {
                            let next = tokio::select! {
                                biased;
                                () = cancel.cancelled() => return,
                                x = model_stream.next() => x,
                            };
                            match next {
                                Some(item) => yield item,
                                None => return,
                            }
                        }
                    }
                    Some(Err(e)) => match policy.next_delay(attempt, &e, fastrand::f64()) {
                        Some(d) => {
                            tracing::debug!(
                                attempt = attempt + 1,
                                delay_ms = d.as_millis() as u64,
                                "retrying model invoke after transient error"
                            );
                            if !backoff(&cancel, d).await {
                                return;
                            }
                            attempt += 1;
                            continue;
                        }
                        None => {
                            yield Err(e);
                            return;
                        }
                    },
                }
            }
        };

        Ok(Box::pin(s))
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.inner.capabilities()
    }

    fn provider(&self) -> &str {
        self.inner.provider()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }
}
```

- [ ] **Step 4: Run the full crate test suite**

Run: `cargo test -p paigasus-helikon-runtime-tokio`
Expected: all tests PASS (the 7 `policy_tests` + the 6 `decorator_tests` + the crate's existing `tests/` integration tests). If `cancellation_aborts_backoff_promptly` hangs, the `backoff`/`select!` cancellation wiring is wrong — fix before continuing.

- [ ] **Step 5: Lint + format**

Run: `cargo fmt --all` then `cargo clippy -p paigasus-helikon-runtime-tokio --all-targets -- -D warnings`
Expected: no diffs from fmt; clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-runtime-tokio/src/retry.rs crates/paigasus-helikon-runtime-tokio/src/lib.rs
git commit -m "feat(runtime-tokio): SMA-393 add RetryingModel retry decorator"
```

---

## Task 4: Reconcile the stale `RunConfig::retry_policy` docs

Three crates promise `RunConfig::retry_policy` in docs; repoint them at the decorator. Use **plain backticks** (not `[ ]` intra-doc links) — these crates don't depend on `runtime-tokio`, so a link would break `RUSTDOCFLAGS="-D warnings" cargo doc`.

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs`
- Modify: `crates/paigasus-helikon-providers-openai/src/error.rs`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/error.rs`

- [ ] **Step 1: Repoint the `ModelError` rustdoc in core**

In `crates/paigasus-helikon-core/src/model.rs`, replace this doc block (directly above `pub enum ModelError`):

```rust
/// Per ADR-10 (*No silent auto-retry inside the loop*), the runner never
/// retries on these — retries are an application-layer concern configured
/// via `RunConfig::retry_policy` (lands with the runner ticket).
```

with:

```rust
/// Per ADR-10 (*No silent auto-retry inside the loop*), the runner never
/// retries on these — retries are an application-layer concern. Wrap a
/// [`Model`] in `RetryingModel` (with a `RetryPolicy`) from
/// `paigasus-helikon-runtime-tokio` to retry the transient variants
/// (`Unavailable`, `RateLimited`, `Transport`) with backoff.
```

- [ ] **Step 2: Repoint the OpenAI provider module docstring**

In `crates/paigasus-helikon-providers-openai/src/error.rs`, replace:

```rust
//! Per ADR-10 ("no silent auto-retry in the loop"), the loop never
//! retries on `ModelError`; the application configures retries via
//! `RunConfig::retry_policy`. Auth failures (401/403) map to
```

with:

```rust
//! Per ADR-10 ("no silent auto-retry in the loop"), the loop never
//! retries on `ModelError`; the application configures retries by wrapping
//! the model in `RetryingModel` (with a `RetryPolicy`) from
//! `paigasus-helikon-runtime-tokio`. Auth failures (401/403) map to
```

- [ ] **Step 3: Repoint the Anthropic provider module docstring**

In `crates/paigasus-helikon-providers-anthropic/src/error.rs`, replace:

```rust
//! Per ADR-10 ("no silent auto-retry in the loop"), the runner never
//! retries; the application configures retries via
//! `RunConfig::retry_policy`. Auth failures (401/403) map to `Refused`;
```

with:

```rust
//! Per ADR-10 ("no silent auto-retry in the loop"), the runner never
//! retries; the application configures retries by wrapping the model in
//! `RetryingModel` (with a `RetryPolicy`) from
//! `paigasus-helikon-runtime-tokio`. Auth failures (401/403) map to `Refused`;
```

- [ ] **Step 4: Verify docs build clean (no broken links)**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
Expected: success, no warnings.

- [ ] **Step 5: Verify no stale references remain**

Run: `grep -rn "RunConfig::retry_policy" crates/ docs/`
Expected: no matches (the spec/plan use the term in prose but not as a backticked promise; if those lines appear, that's fine — confirm none are in crate source docstrings).

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/model.rs crates/paigasus-helikon-providers-openai/src/error.rs crates/paigasus-helikon-providers-anthropic/src/error.rs
git commit -m "docs: SMA-393 repoint stale RunConfig::retry_policy refs at RetryingModel"
```

> **Release note (from the spec, L3):** a squash-merge of the PR folds these doc-only edits into the `feat(runtime-tokio)` attribution, so release-plz may re-release `core`, `providers-openai`, and `providers-anthropic` (all doc-only, harmless). Accepted for this ticket; flag it in the PR description.

---

## Task 5: User-facing docs — README + mdBook

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/README.md`
- Modify: `docs/book/src/concepts/model-providers.md`

- [ ] **Step 1: Add a Retries section to the runtime-tokio README**

In `crates/paigasus-helikon-runtime-tokio/README.md`, insert this section between the `## Example` section and the `## Links` section:

````markdown
## Retrying transient errors

Wrap any `Model` in `RetryingModel` to retry transient provider failures
(`Unavailable`, `RateLimited`, `Transport`) with exponential backoff + jitter.
Retry is **opt-in** — configured by wrapping the model, not via `RunConfig`
(the runner can't reach the agent's model, and core can't sleep) — and is
disabled unless you wrap. It covers *connection establishment*: once a response
has started streaming, a mid-stream drop is surfaced rather than retried.

```rust
use std::time::Duration;
use paigasus_helikon_runtime_tokio::{RetryPolicy, RetryingModel};

// `model` is any `Model` (e.g. an OpenAI or Anthropic provider).
let policy = RetryPolicy::new()
    .max_attempts(4)
    .base_delay(Duration::from_millis(250));
let resilient = RetryingModel::new(model, policy);
// Build your agent with `resilient` as its model.
```

`RateLimited { retry_after_ms }` waits at least the provider's hint; backoff
sleeps abort promptly on cancellation.
````

- [ ] **Step 2: Add a retry subsection to the Model Providers mdBook page**

Append this section to the end of `docs/book/src/concepts/model-providers.md`:

````markdown
## Retrying transient errors

Provider calls can fail transiently — rate limits (`RateLimited`), `503`s
(`Unavailable`), or dropped connections (`Transport`). Per ADR-10 the agent loop
never auto-retries; retry is an opt-in composition-layer concern.

`paigasus-helikon-runtime-tokio` provides a `RetryingModel<M>` decorator: it
wraps any `Model` and retries those transient variants with exponential backoff
and jitter. It is configured by **wrapping the model** (not via `RunConfig`),
and is disabled unless you wrap.

```rust,ignore
use std::time::Duration;
use paigasus_helikon_runtime_tokio::{RetryPolicy, RetryingModel};

let policy = RetryPolicy::new()
    .max_attempts(4)
    .base_delay(Duration::from_millis(250));
let resilient = RetryingModel::new(model, policy);
```

Retry covers *connection establishment*: a retryable error that arrives before
any content has streamed is retried; once tokens or tool-call deltas have been
emitted, a later error is surfaced rather than retried (output can't be
un-emitted). `RateLimited { retry_after_ms }` waits at least the provider's
hint, and backoff sleeps abort promptly on run cancellation.
````

- [ ] **Step 3: Verify the book builds clean (if `mdbook` is installed)**

Run: `mdbook build docs/book`
Expected: builds with no linkcheck errors (`[output.linkcheck] warning-policy = "error"`). If `mdbook` is not installed locally, skip — the prose adds no new cross-page links, so linkcheck is unaffected.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-runtime-tokio/README.md docs/book/src/concepts/model-providers.md
git commit -m "docs(runtime-tokio): SMA-393 document RetryingModel in README and book"
```

---

## Task 6: Full CI-gate verification

**Files:** none (verification only).

- [ ] **Step 1: Run every CI gate locally (matches `.github/workflows/ci.yml`)**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: all four succeed with no warnings/failures.

- [ ] **Step 2: Confirm the public surface flows through the facade**

Run: `cargo doc -p paigasus-helikon --all-features --no-deps` then confirm `RetryPolicy`/`RetryingModel` appear under `paigasus_helikon::runtime_tokio` (the facade re-exports the crate, so no facade code change was needed).

- [ ] **Step 3: Confirm the commit history is clean and conventional**

Run: `git log --oneline main..HEAD`
Expected: the spec/plan `docs(...)` commits plus `build(runtime-tokio)`, `feat(runtime-tokio)` ×1, `docs:`, `docs(runtime-tokio)` — each a valid `type(scope)` from the `.versionrc` allowlist.

- [ ] **Step 4: (If not already done) commit the plan itself**

```bash
git add docs/superpowers/plans/2026-06-17-retry-policy.md
git commit -m "docs(plan): SMA-393 add retry-policy implementation plan"
```

---

## Acceptance-criteria traceability

| Ticket AC | Covered by |
|-----------|-----------|
| N−1 retryable failures then success completes | Task 3 `retries_until_success` |
| Non-retryable (`Refused`) is not retried | Task 3 `non_retryable_is_not_retried` + Task 2 `non_retryable_variants_never_retry` |
| `RateLimited { retry_after_ms }` waits ≥ hint | Task 2 `rate_limited_waits_at_least_the_hint` + Task 3 `rate_limit_hint_is_awaited_then_retries` |
| Backoff sleeps abort promptly on cancellation | Task 3 `cancellation_aborts_backoff_promptly` |
| Default = retry disabled (ADR-10 preserved) | Structural (no `RunConfig` field; opt-in wrap) + Task 2 `max_attempts_one_is_passthrough` |
| Keep tokio timers out of core | Decorator lives in `runtime-tokio`; core change is doc-only (Task 4) |
| Docs reconciled | Task 4 (core + both providers), Task 5 (README + book) |
