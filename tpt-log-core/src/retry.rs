use crate::TransportError;

/// Exponential backoff policy (spec §5.3): 1s → 2s → 4s → … capped at 60s,
/// with a hard ceiling on the number of delivery attempts.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Maximum number of delivery attempts (default 10).
    pub max_attempts: u32,
    /// Initial backoff delay in milliseconds (default 1000).
    pub base_ms: u64,
    /// Backoff ceiling in milliseconds (default 60_000).
    pub max_backoff_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: crate::defaults::MAX_RETRIES,
            base_ms: crate::defaults::RETRY_BASE_MS,
            max_backoff_ms: crate::defaults::RETRY_MAX_BACKOFF_MS,
        }
    }
}

impl RetryPolicy {
    /// Backoff to sleep before attempt `attempt_no` (0-indexed; 0 → none).
    pub fn backoff_ms(&self, attempt_no: u32) -> u64 {
        if attempt_no == 0 {
            return 0;
        }
        let shift = 1u64 << attempt_no.saturating_sub(1).min(10);
        self.base_ms
            .saturating_mul(shift)
            .min(self.max_backoff_ms.max(1))
    }

    /// Sleep for the backoff associated with the given attempt.
    pub fn sleep_before(&self, attempt_no: u32) {
        let ms = self.backoff_ms(attempt_no);
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

/// Run the retry loop, returning an overall outcome.
pub fn deliver_with_retry<F>(policy: &RetryPolicy, mut send: F) -> RetryOutcome
where
    F: FnMut() -> Result<(), TransportError>,
{
    let mut attempt: u32 = 0;
    loop {
        match send() {
            Ok(()) => {
                return RetryOutcome::Delivered {
                    attempts: attempt + 1,
                }
            }
            Err(TransportError::Permanent(_)) | Err(TransportError::Config(_)) => {
                return RetryOutcome::Permanent;
            }
            Err(TransportError::Transient(_))
            | Err(TransportError::Io(_))
            | Err(TransportError::Rejected(_)) => {
                if attempt + 1 >= policy.max_attempts.max(1) {
                    return RetryOutcome::Exhausted {
                        attempts: attempt + 1,
                    };
                }
                policy.sleep_before(attempt);
                attempt += 1;
            }
        }
    }
}

/// Compute the retry policy for a given config.
pub fn policy_from_config(cfg: &crate::Config) -> RetryPolicy {
    RetryPolicy {
        max_attempts: cfg.max_retries.max(1),
        base_ms: cfg.retry_base_ms.max(1),
        max_backoff_ms: cfg.retry_max_backoff_ms.max(cfg.retry_base_ms),
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
        };
        assert_eq!(p.backoff_ms(0), 0);
        assert_eq!(p.backoff_ms(1), 1000);
        assert_eq!(p.backoff_ms(2), 2000);
        assert_eq!(p.backoff_ms(3), 4000);
        assert_eq!(p.backoff_ms(7), 60_000);
        assert_eq!(p.backoff_ms(50), 60_000);
    }

    #[test]
    fn retry_honors_max_attempts() {
        let p = RetryPolicy {
            max_attempts: 3,
            base_ms: 1,
            max_backoff_ms: 2,
        };
        let mut calls = 0;
        let out = deliver_with_retry(&p, || {
            calls += 1;
            Err(TransportError::Transient("down".into()))
        });
        assert_eq!(out, RetryOutcome::Exhausted { attempts: 3 });
        assert_eq!(calls, 3);
    }

    #[test]
    fn permanent_gives_up_immediately() {
        let p = RetryPolicy {
            max_attempts: 10,
            base_ms: 1,
            max_backoff_ms: 2,
        };
        let mut calls = 0;
        let out = deliver_with_retry(&p, || {
            calls += 1;
            Err(TransportError::Permanent("bad key".into()))
        });
        assert_eq!(out, RetryOutcome::Permanent);
        assert_eq!(calls, 1);
    }

    #[test]
    fn succeeds_after_retries() {
        let p = RetryPolicy {
            max_attempts: 5,
            base_ms: 1,
            max_backoff_ms: 2,
        };
        let mut calls = 0;
        let out = deliver_with_retry(&p, || {
            calls += 1;
            if calls < 3 {
                Err(TransportError::Transient("not yet".into()))
            } else {
                Ok(())
            }
        });
        assert_eq!(out, RetryOutcome::Delivered { attempts: 3 });
        assert_eq!(calls, 3);
    }
}
