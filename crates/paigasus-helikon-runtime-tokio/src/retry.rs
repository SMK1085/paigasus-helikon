//! Opt-in retry for transient model errors: a pure-data [`RetryPolicy`] and a
//! [`RetryingModel`] decorator that retries `Model::invoke` with backoff.
//!
//! Retry is configured by wrapping a [`paigasus_helikon_core::Model`] (not via
//! `RunConfig`): the runner only holds `&dyn Agent` and can't reach the model,
//! and core can't sleep. Disabled unless you wrap. See ADR-10.

use std::sync::Arc;
use std::time::Duration;

use async_stream::stream;
use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream::StreamExt as _;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

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
        let ceiling = if grown.is_finite() {
            grown.min(cap)
        } else {
            cap
        };
        let secs = if self.jitter {
            ceiling * jitter_fraction
        } else {
            ceiling
        };
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
                // (1) Invoke the inner model. The invoke call itself is not raced
                // with cancellation: for lazy-stream providers (HTTP/SSE) the
                // future resolves immediately, so racing it would let a
                // pre-cancelled token skip the invoke entirely (counter-intuitive
                // and would break the attempt-count contract). Cancellation is
                // honoured at the two points where real latency occurs: the
                // first-item peek and the backoff sleep.
                let model_stream_result = inner.invoke(request.clone(), cancel.clone()).await;

                let mut model_stream = match model_stream_result {
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
        assert!(p
            .next_delay(0, &ModelError::Refused { reason: "x".into() }, 0.0)
            .is_none());
        assert!(p
            .next_delay(0, &ModelError::ContextLengthExceeded, 0.0)
            .is_none());
        assert!(p
            .next_delay(0, &ModelError::Other(anyhow::anyhow!("x")), 0.0)
            .is_none());
    }

    #[test]
    fn retryable_variants_retry() {
        let p = RetryPolicy::new();
        assert!(p.next_delay(0, &ModelError::Unavailable, 0.0).is_some());
        assert!(p
            .next_delay(0, &ModelError::Transport("reset".into()), 0.0)
            .is_some());
        assert!(p
            .next_delay(
                0,
                &ModelError::RateLimited {
                    retry_after_ms: None
                },
                0.0
            )
            .is_some());
    }

    #[test]
    fn full_jitter_scales_between_zero_and_ceiling() {
        let p = RetryPolicy::new()
            .base_delay(Duration::from_secs(1))
            .multiplier(2.0);
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
            .max_attempts(4)
            .jitter(false);
        // Without the cap this would be 10s * 10^2 = 1000s; cap pins it at 5s.
        assert_eq!(
            p.next_delay(2, &ModelError::Unavailable, 0.0).unwrap(),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn rate_limited_waits_at_least_the_hint() {
        let p = RetryPolicy::new()
            .base_delay(Duration::from_millis(1))
            .jitter(true);
        let err = ModelError::RateLimited {
            retry_after_ms: Some(5_000),
        };
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

#[cfg(test)]
mod decorator_tests {
    use super::*;
    use paigasus_helikon_core::FinishReason;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// What the i-th `invoke` of [`ScriptModel`] yields.
    enum Resp {
        /// The stream's first (and only) item is this error.
        ErrFirst(ModelError),
        /// `TokenDelta("ok")` then `Finish(Stop)`.
        Ok,
        /// `TokenDelta("partial")` then this error (mid-stream).
        OkThenErr(ModelError),
    }

    impl Clone for Resp {
        fn clone(&self) -> Self {
            match self {
                Resp::ErrFirst(e) => Resp::ErrFirst(clone_err(e)),
                Resp::Ok => Resp::Ok,
                Resp::OkThenErr(e) => Resp::OkThenErr(clone_err(e)),
            }
        }
    }

    /// Helper: manually clone a `ModelError` variant for test use.
    fn clone_err(e: &ModelError) -> ModelError {
        match e {
            ModelError::Unavailable => ModelError::Unavailable,
            ModelError::RateLimited { retry_after_ms } => ModelError::RateLimited {
                retry_after_ms: *retry_after_ms,
            },
            ModelError::ContextLengthExceeded => ModelError::ContextLengthExceeded,
            ModelError::Refused { reason } => ModelError::Refused {
                reason: reason.clone(),
            },
            ModelError::Transport(s) => ModelError::Transport(s.clone()),
            ModelError::Other(e) => ModelError::Other(anyhow::anyhow!("{e}")),
            _ => ModelError::Other(anyhow::anyhow!("unknown variant")),
        }
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
            Resp::ErrFirst(ModelError::Refused {
                reason: "policy".to_owned(),
            }),
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
        assert!(matches!(
            items.last(),
            Some(Err(ModelError::Refused { .. }))
        ));
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
        let model = ScriptModel::new(vec![Resp::ErrFirst(ModelError::Unavailable), Resp::Ok]);
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
            Resp::ErrFirst(ModelError::RateLimited {
                retry_after_ms: Some(10_000),
            }),
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
