//! Opt-in retry for transient model errors: a pure-data [`RetryPolicy`] and a
//! [`RetryingModel`] decorator that retries `Model::invoke` with backoff.
//!
//! Retry is configured by wrapping a [`paigasus_helikon_core::Model`] (not via
//! `RunConfig`): the runner only holds `&dyn Agent` and can't reach the model,
//! and core can't sleep. Disabled unless you wrap. See ADR-10.

use std::time::Duration;

use paigasus_helikon_core::ModelError;

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
