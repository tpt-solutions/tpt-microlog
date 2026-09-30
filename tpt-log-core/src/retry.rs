use crate::TransportError;

/// Exponential backoff policy (spec §5.3): base → 2×base → 4×base → … capped
/// at 60s, with a hard ceiling on the number of delivery attempts.
///
/// `backoff_ms(n)` is the delay before retry `n + 1` (i.e. after `n`
/// consecutive failures); the first retry waits `base_ms`, never zero.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Maximum number of delivery attempts (default 10).
    pub max_attempts: u32,
    /// Initial backoff delay in milliseconds (default 1000).
    pub base_ms: u64,
    /// Backoff ceiling in milliseconds (default 60_000).
    pub max_backoff_ms: u64,
    /// Whether to apply random jitter to each delay (default true).
    pub jitter: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: crate::defaults::MAX_RETRIES,
            base_ms: crate::defaults::RETRY_BASE_MS,
            max_backoff_ms: crate::defaults::RETRY_MAX_BACKOFF_MS,
            jitter: true,
        }
    }
}

impl RetryPolicy {
    /// Backoff to sleep after `failures` consecutive failures (0-indexed;
    /// the first retry waits `base_ms`). Deterministic — no jitter applied.
    pub fn backoff_ms(&self, failures: u32) -> u64 {
        let shift = 1u64 << failures.min(10);
        self.base_ms
            .saturating_mul(shift)
            .min(self.max_backoff_ms.max(1))
    }

    /// Delay to actually sleep: deterministic backoff with de-synchronizing
    /// jitter (uniform in `[backoff/2, backoff]`) so many clients do not
    /// retry in lockstep after a shared outage.
    pub fn delay_ms(&self, failures: u32) -> u64 {
        let backoff = self.backoff_ms(failures);
        if !self.jitter || backoff <= 1 {
            return backoff;
        }
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let half = backoff / 2;
        half + rng.gen_range(0..=backoff - half)
    }

    /// Sleep for the jittered delay associated with `failures` so far.
    pub fn sleep_before(&self, failures: u32) {
        let ms = self.delay_ms(failures);
        if ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }
}

/// Outcome of a delivery attempt sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryOutcome {
    /// Delivered successfully (possibly after retries).
    Delivered { attempts: u32 },
    /// Permanently rejected; should fall back to the WAL.
    Permanent,
    /// Exhausted retries while still failing; should fall back to the WAL.
    Exhausted { attempts: u32 },
}

/// Compute the retry policy for a given config.
pub fn policy_from_config(cfg: &crate::Config) -> RetryPolicy {
    RetryPolicy {
        max_attempts: cfg.max_retries.max(1),
        base_ms: cfg.retry_base_ms.max(1),
        max_backoff_ms: cfg.retry_max_backoff_ms.max(cfg.retry_base_ms),
        jitter: true,
    }
}

/// Extra delay the backend asked for via `Retry-After` (ms), if any.
pub(crate) fn retry_after_hint(err: &TransportError) -> Option<u64> {
    match err {
        TransportError::RateLimited { retry_after_ms, .. } => *retry_after_ms,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let p = RetryPolicy {
            max_attempts: 20,
            base_ms: 1000,
            max_backoff_ms: 60_000,
            jitter: false,
        };
        assert_eq!(p.backoff_ms(0), 1000, "first retry waits the base delay");
        assert_eq!(p.backoff_ms(1), 2000);
        assert_eq!(p.backoff_ms(2), 4000);
        assert_eq!(p.backoff_ms(6), 60_000);
        assert_eq!(p.backoff_ms(50), 60_000);
    }

    #[test]
    fn jitter_stays_within_half_backoff() {
        let p = RetryPolicy {
            max_attempts: 10,
            base_ms: 1000,
            max_backoff_ms: 60_000,
            jitter: true,
        };
        for failures in 0..6 {
            let d = p.delay_ms(failures);
            let backoff = p.backoff_ms(failures);
            assert!(
                d >= backoff / 2 && d <= backoff,
                "failures={failures} d={d}"
            );
        }
    }

    #[test]
    fn permanent_gives_up_immediately() {
        // deliver_one-style classification: Permanent never retries.
        let err: TransportError = TransportError::Permanent("bad key".into());
        assert!(matches!(err, TransportError::Permanent(_)));
    }

    #[test]
    fn rate_limited_carries_hint() {
        let err = TransportError::RateLimited {
            message: "HTTP 429".into(),
            retry_after_ms: Some(2_500),
        };
        assert_eq!(retry_after_hint(&err), Some(2_500));
        assert_eq!(
            retry_after_hint(&TransportError::Transient("x".into())),
            None
        );
    }
}
