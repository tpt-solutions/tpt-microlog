//! Volume governors: burst de-duplication, rate limiting, adaptive sampling,
//! the error-triggered flight recorder and the daily volume budget.
//!
//! All of them are driven by *entry timestamps* rather than a wall clock, so
//! they are deterministic and unit-testable. WARN and above are never sampled,
//! rate limited or budget-limited: governors only thin out routine traffic.
//! (De-duplication is the one exception, and it never loses information: the
//! first entry passes and a `repeat_count` records what was folded into it.)

use std::collections::{HashMap, VecDeque};

use parking_lot::Mutex;

use crate::{LogEntry, LogLevel};

/// What happened to an entry in [`crate::filter::EntryProcessor::process_with`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The entry continues into the pipeline.
    Keep,
    /// Removed by `min_level`, `filter_regex` or `filter_tags`.
    Filtered,
    /// Removed by sampling (static, adaptive or budget-driven).
    Sampled,
    /// Removed by `rate_limit_per_sec`.
    RateLimited,
    /// Folded into an earlier identical entry (`dedup_window_ms`).
    Deduplicated,
    /// Held in the flight recorder, shipped only if an error follows.
    Recorded,
}

// ---------------------------------------------------------------- dedup

struct Bucket {
    window_start: u64,
    last_seen: u64,
    suppressed: u64,
    template: LogEntry,
}

/// Collapses bursts of identical log entries (same level and message) inside a
/// window into the first entry plus a `repeat_count`.
pub struct Dedup {
    window_ms: u64,
    cap: usize,
    buckets: Mutex<HashMap<(LogLevel, String), Bucket>>,
}

/// Outcome of [`Dedup::check`].
#[derive(Debug, PartialEq, Eq)]
pub enum DedupOutcome {
    /// Deliver the entry; `repeat_count` is how many identical entries were
    /// folded away since the last delivered one.
    Pass { repeat_count: Option<u64> },
    /// Identical to a recent entry: drop it (it is counted).
    Suppress,
}

impl Dedup {
    /// Window length in milliseconds; at most `cap` distinct messages are
    /// tracked at once (memory bound).
    pub fn new(window_ms: u64, cap: usize) -> Self {
        Self {
            window_ms: window_ms.max(1),
            cap: cap.max(1),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Decide whether `entry` is delivered or folded into an earlier one.
    pub fn check(&self, entry: &LogEntry) -> DedupOutcome {
        let now = entry.timestamp;
        let key = (entry.level, entry.message.clone());
        let mut buckets = self.buckets.lock();
        if let Some(bucket) = buckets.get_mut(&key) {
            if now.saturating_sub(bucket.window_start) < self.window_ms {
                bucket.suppressed += 1;
                bucket.last_seen = now;
                return DedupOutcome::Suppress;
            }
            let folded = bucket.suppressed;
            bucket.window_start = now;
            bucket.last_seen = now;
            bucket.suppressed = 0;
            bucket.template = entry.clone();
            return DedupOutcome::Pass {
                repeat_count: (folded > 0).then_some(folded),
            };
        }
        if buckets.len() >= self.cap {
            // Make room by forgetting settled buckets; buckets that still owe
            // a summary are kept so no count is lost.
            let window = self.window_ms;
            buckets.retain(|_, b| b.suppressed > 0 || now.saturating_sub(b.window_start) < window);
            if buckets.len() >= self.cap {
                return DedupOutcome::Pass { repeat_count: None };
            }
        }
        buckets.insert(
            key,
            Bucket {
                window_start: now,
                last_seen: now,
                suppressed: 0,
                template: entry.clone(),
            },
        );
        DedupOutcome::Pass { repeat_count: None }
    }

    /// Summary entries for windows that closed with suppressed repeats. With
    /// `force` (flush/shutdown) every pending count is emitted.
    pub fn drain_summaries(&self, now_ms: u64, force: bool) -> Vec<LogEntry> {
        let window = self.window_ms;
        let mut out = Vec::new();
        let mut buckets = self.buckets.lock();
        buckets.retain(|_, b| {
            let expired = now_ms.saturating_sub(b.window_start) >= window;
            if b.suppressed > 0 && (force || expired) {
                let mut summary = b.template.clone();
                summary.timestamp = b.last_seen;
                summary
                    .metadata
                    .insert("repeat_count".into(), b.suppressed.into());
                summary
                    .metadata
                    .insert("repeat_summary".into(), serde_json::Value::Bool(true));
                out.push(summary);
                b.suppressed = 0;
            }
            !expired || b.suppressed > 0
        });
        out
    }
}

// ----------------------------------------------------------- rate limiter

/// Token bucket (burst of one second's worth) over entry timestamps.
pub struct RateLimiter {
    per_sec: f64,
    state: Mutex<(f64, u64)>,
}

impl RateLimiter {
    /// Allow `per_sec` entries per second on average.
    pub fn new(per_sec: u32) -> Self {
        Self {
            per_sec: f64::from(per_sec.max(1)),
            state: Mutex::new((f64::from(per_sec.max(1)), 0)),
        }
    }

    /// `true` when an entry at `now_ms` may pass.
    pub fn allow(&self, now_ms: u64) -> bool {
        let mut state = self.state.lock();
        let (tokens, last) = &mut *state;
        if *last == 0 {
            *last = now_ms;
        }
        let elapsed = now_ms.saturating_sub(*last) as f64;
        *tokens = (*tokens + elapsed * self.per_sec / 1000.0).min(self.per_sec);
        *last = now_ms.max(*last);
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

// --------------------------------------------------------- adaptive rate

/// Queue fill ratio at which adaptive sampling starts to engage.
pub const ADAPTIVE_HIGH_WATER: f64 = 0.5;

/// Sampling multiplier for routine entries given queue `pressure` (0.0 empty
/// to 1.0 full): 1.0 up to the high-water mark, then a linear fall to `floor`.
pub fn adaptive_rate(pressure: f64, floor: f64) -> f64 {
    let floor = floor.clamp(0.0, 1.0);
    if pressure <= ADAPTIVE_HIGH_WATER {
        return 1.0;
    }
    let t = ((pressure - ADAPTIVE_HIGH_WATER) / (1.0 - ADAPTIVE_HIGH_WATER)).min(1.0);
    1.0 - t * (1.0 - floor)
}

// ---------------------------------------------------------- daily budget

const DAY_MS: u64 = 86_400_000;
/// Before this much of the day has elapsed the offered rate is too noisy to
/// project from, so sampling is left alone (unless the budget is spent).
const BUDGET_WARMUP_MS: u64 = 60_000;

struct BudgetState {
    day: u64,
    /// Routine entries that reached the budget stage (before its sampling).
    offered: u64,
    /// Entries delivered this day, WARN+ included.
    spent: u64,
}

/// A daily entry budget that tightens sampling automatically: the multiplier
/// is chosen so the projected end-of-day volume lands on the budget. Once the
/// budget is spent only WARN and above get through.
pub struct DailyBudget {
    budget: u64,
    state: Mutex<BudgetState>,
}

impl DailyBudget {
    /// `budget` entries per UTC day.
    pub fn new(budget: u64) -> Self {
        Self {
            budget: budget.max(1),
            state: Mutex::new(BudgetState {
                day: 0,
                offered: 0,
                spent: 0,
            }),
        }
    }

    fn roll(state: &mut BudgetState, now_ms: u64) {
        let day = now_ms / DAY_MS;
        if state.day != day {
            *state = BudgetState {
                day,
                offered: 0,
                spent: 0,
            };
        }
    }

    /// Sampling multiplier (0.0..=1.0) for a routine entry at `now_ms`, and
    /// record that it was offered.
    pub fn offer(&self, now_ms: u64) -> f64 {
        let mut s = self.state.lock();
        Self::roll(&mut s, now_ms);
        s.offered += 1;
        if s.spent >= self.budget {
            return 0.0;
        }
        let into_day = now_ms % DAY_MS;
        if into_day < BUDGET_WARMUP_MS {
            return 1.0;
        }
        let remaining_ms = (DAY_MS - into_day) as f64;
        let offered_per_ms = s.offered as f64 / into_day as f64;
        let projected_offers = offered_per_ms * remaining_ms;
        if projected_offers <= 0.0 {
            return 1.0;
        }
        let headroom = (self.budget - s.spent) as f64;
        (headroom / projected_offers).clamp(0.0, 1.0)
    }

    /// Record a delivered entry (any level).
    pub fn spend(&self, now_ms: u64) {
        let mut s = self.state.lock();
        Self::roll(&mut s, now_ms);
        s.spent += 1;
    }
}

// -------------------------------------------------------- flight recorder

/// Ring buffer of recent low-severity entries, shipped only when an error
/// happens, so DEBUG detail costs nothing until it explains a failure.
pub struct FlightRecorder {
    cap: usize,
    max_level: LogLevel,
    ring: Mutex<VecDeque<LogEntry>>,
}

impl FlightRecorder {
    /// Hold up to `cap` entries at or below `max_level`.
    pub fn new(cap: usize, max_level: LogLevel) -> Self {
        Self {
            cap: cap.max(1),
            max_level,
            ring: Mutex::new(VecDeque::new()),
        }
    }

    /// Whether `entry` belongs in the recorder rather than the pipeline.
    pub fn captures(&self, entry: &LogEntry) -> bool {
        entry.log_type == crate::LogType::Log && entry.level <= self.max_level
    }

    /// Remember `entry`, evicting the oldest when full.
    pub fn record(&self, entry: LogEntry) {
        let mut ring = self.ring.lock();
        if ring.len() >= self.cap {
            ring.pop_front();
        }
        ring.push_back(entry);
    }

    /// Take everything recorded so far (oldest first), marked as context.
    pub fn take(&self) -> Vec<LogEntry> {
        self.ring
            .lock()
            .drain(..)
            .map(|mut e| {
                e.metadata
                    .insert("flight_recorder".into(), serde_json::Value::Bool(true));
                e
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(level: LogLevel, msg: &str, ts: u64) -> LogEntry {
        let mut e = LogEntry::new(level, msg);
        e.timestamp = ts;
        e
    }

    #[test]
    fn dedup_folds_bursts_and_reports_the_count() {
        let d = Dedup::new(1000, 100);
        assert_eq!(
            d.check(&at(LogLevel::Info, "boom", 10_000)),
            DedupOutcome::Pass { repeat_count: None }
        );
        for i in 1..=4 {
            assert_eq!(
                d.check(&at(LogLevel::Info, "boom", 10_000 + i * 100)),
                DedupOutcome::Suppress
            );
        }
        // A different level or message is independent.
        assert!(matches!(
            d.check(&at(LogLevel::Warn, "boom", 10_500)),
            DedupOutcome::Pass { .. }
        ));
        // After the window, the next occurrence carries the folded count.
        assert_eq!(
            d.check(&at(LogLevel::Info, "boom", 11_500)),
            DedupOutcome::Pass {
                repeat_count: Some(4)
            }
        );
    }

    #[test]
    fn dedup_summaries_cover_bursts_that_just_stop() {
        let d = Dedup::new(1000, 100);
        d.check(&at(LogLevel::Error, "db down", 5_000));
        d.check(&at(LogLevel::Error, "db down", 5_100));
        d.check(&at(LogLevel::Error, "db down", 5_200));
        assert!(
            d.drain_summaries(5_500, false).is_empty(),
            "window still open"
        );
        let out = d.drain_summaries(6_100, false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].metadata["repeat_count"], 2);
        assert_eq!(out[0].metadata["repeat_summary"], true);
        assert!(
            d.drain_summaries(9_000, true).is_empty(),
            "count emitted once"
        );
    }

    #[test]
    fn dedup_force_drains_open_windows_on_shutdown() {
        let d = Dedup::new(60_000, 100);
        d.check(&at(LogLevel::Info, "x", 1_000));
        d.check(&at(LogLevel::Info, "x", 1_001));
        let out = d.drain_summaries(1_002, true);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].metadata["repeat_count"], 1);
    }

    #[test]
    fn dedup_memory_is_bounded() {
        let d = Dedup::new(60_000, 3);
        for i in 0..10 {
            d.check(&at(LogLevel::Info, &format!("m{i}"), 1_000));
        }
        assert!(d.buckets.lock().len() <= 3);
        // Untracked messages simply pass instead of failing.
        assert!(matches!(
            d.check(&at(LogLevel::Info, "m9", 1_001)),
            DedupOutcome::Pass { .. }
        ));
    }

    #[test]
    fn rate_limiter_allows_a_burst_then_refills() {
        let r = RateLimiter::new(10);
        let t0 = 1_000_000;
        let allowed = (0..50).filter(|_| r.allow(t0)).count();
        assert_eq!(allowed, 10, "one second's worth of burst");
        assert!(!r.allow(t0 + 50));
        assert!(r.allow(t0 + 150), "0.15s refills 1.5 tokens");
        // A long idle period refills to the cap, not beyond it.
        let after_idle = (0..50).filter(|_| r.allow(t0 + 60_000)).count();
        assert_eq!(after_idle, 10);
    }

    #[test]
    fn adaptive_rate_curve() {
        assert_eq!(adaptive_rate(0.0, 0.1), 1.0);
        assert_eq!(adaptive_rate(0.5, 0.1), 1.0);
        assert!((adaptive_rate(0.75, 0.1) - 0.55).abs() < 1e-9);
        assert!((adaptive_rate(1.0, 0.1) - 0.1).abs() < 1e-9);
        assert!((adaptive_rate(2.0, 0.1) - 0.1).abs() < 1e-9, "clamped");
    }

    #[test]
    fn budget_leaves_traffic_alone_when_on_track() {
        let b = DailyBudget::new(1_000_000);
        let noon = 100 * DAY_MS + DAY_MS / 2;
        for i in 0..1000 {
            assert_eq!(b.offer(noon + i), 1.0);
            b.spend(noon + i);
        }
    }

    #[test]
    fn budget_tightens_to_hit_the_target() {
        let b = DailyBudget::new(1_000);
        let day = 200 * DAY_MS;
        // 6 hours in, 600 entries already offered at full rate: the offered
        // rate projects to 2400/day against a budget of 1000.
        let t = day + DAY_MS / 4;
        for i in 0..599 {
            b.offer(t - 1_000_000 + i);
            b.spend(t - 1_000_000 + i);
        }
        let m = b.offer(t);
        assert!(
            m < 0.5 && m > 0.1,
            "multiplier {m} should be roughly 400/1800"
        );
    }

    #[test]
    fn budget_exhaustion_and_next_day_reset() {
        let b = DailyBudget::new(3);
        let day = 300 * DAY_MS;
        let t = day + 10_000;
        for _ in 0..3 {
            b.offer(t);
            b.spend(t);
        }
        assert_eq!(b.offer(t), 0.0, "spent budget stops routine traffic");
        assert_eq!(b.offer(day + DAY_MS + 5), 1.0, "a new UTC day starts fresh");
    }

    #[test]
    fn flight_recorder_ring_and_context_marking() {
        let r = FlightRecorder::new(3, LogLevel::Debug);
        assert!(r.captures(&at(LogLevel::Debug, "d", 1)));
        assert!(r.captures(&at(LogLevel::Trace, "t", 1)));
        assert!(!r.captures(&at(LogLevel::Info, "i", 1)));
        assert!(!r.captures(&LogEntry::metric("m", 1.0, None)));
        for i in 0..5 {
            r.record(at(LogLevel::Debug, &format!("d{i}"), i));
        }
        let ctx = r.take();
        let msgs: Vec<&str> = ctx.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(msgs, ["d2", "d3", "d4"], "oldest evicted, order kept");
        assert!(ctx.iter().all(|e| e.metadata["flight_recorder"] == true));
        assert!(r.take().is_empty());
    }
}
