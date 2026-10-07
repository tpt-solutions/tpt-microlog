//! Logger health: a point-in-time snapshot for dashboards, `/healthz`
//! probes and the self-telemetry metrics.

use serde_json::{json, Value};

/// Coarse state of the logger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthStatus {
    /// Delivering normally.
    Ok,
    /// Working, but behind: batches waiting in the WAL, a nearly full queue,
    /// or the last delivery attempt failed.
    Degraded,
    /// Failing right now with undelivered data piling up.
    Down,
    /// `shutdown()` has completed.
    Stopped,
}

impl HealthStatus {
    /// Lower-case name used in JSON and metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            HealthStatus::Ok => "ok",
            HealthStatus::Degraded => "degraded",
            HealthStatus::Down => "down",
            HealthStatus::Stopped => "stopped",
        }
    }

    /// HTTP status for `/healthz`: only `Down` and `Stopped` are unhealthy;
    /// a degraded logger still serves traffic and is catching up.
    pub fn http_status(self) -> u16 {
        match self {
            HealthStatus::Ok | HealthStatus::Degraded => 200,
            HealthStatus::Down | HealthStatus::Stopped => 503,
        }
    }
}

/// Snapshot returned by [`crate::MicroLog::health`].
#[derive(Clone, Debug)]
pub struct Health {
    pub status: HealthStatus,
    /// Entries waiting in the ingestion queue.
    pub queue_depth: usize,
    pub queue_capacity: usize,
    /// Batches waiting for the delivery worker.
    pub inflight_batches: usize,
    /// Undelivered batches persisted in the WAL, and their size.
    pub wal_files: usize,
    pub wal_bytes: u64,
    pub delivered_entries: u64,
    pub dropped_entries: u64,
    pub delivery_failures: u64,
    /// Milliseconds since the last successful / failed delivery attempt.
    pub last_success_ms_ago: Option<u64>,
    pub last_failure_ms_ago: Option<u64>,
    /// Latency of the last successful send and the running mean (ms).
    pub last_delivery_latency_ms: u64,
    pub avg_delivery_latency_ms: f64,
}

impl Health {
    /// JSON body served by `/healthz` and `/stats`.
    pub fn to_json(&self) -> Value {
        json!({
            "status": self.status.as_str(),
            "queue_depth": self.queue_depth,
            "queue_capacity": self.queue_capacity,
            "inflight_batches": self.inflight_batches,
            "wal_files": self.wal_files,
            "wal_bytes": self.wal_bytes,
            "delivered_entries": self.delivered_entries,
            "dropped_entries": self.dropped_entries,
            "delivery_failures": self.delivery_failures,
            "last_success_ms_ago": self.last_success_ms_ago,
            "last_failure_ms_ago": self.last_failure_ms_ago,
            "last_delivery_latency_ms": self.last_delivery_latency_ms,
            "avg_delivery_latency_ms": (self.avg_delivery_latency_ms * 100.0).round() / 100.0,
        })
    }

    /// The numbers worth graphing, as `(metric name, value)` pairs.
    pub fn metrics(&self) -> Vec<(&'static str, f64)> {
        vec![
            ("microlog.queue_depth", self.queue_depth as f64),
            ("microlog.inflight_batches", self.inflight_batches as f64),
            ("microlog.wal_bytes", self.wal_bytes as f64),
            ("microlog.wal_files", self.wal_files as f64),
            ("microlog.delivered_entries", self.delivered_entries as f64),
            ("microlog.dropped_entries", self.dropped_entries as f64),
            ("microlog.delivery_failures", self.delivery_failures as f64),
            ("microlog.delivery_latency_ms", self.avg_delivery_latency_ms),
        ]
    }
}

/// Classify the logger from raw counters (kept separate for unit tests).
#[allow(clippy::too_many_arguments)]
pub fn classify(
    stopped: bool,
    queue_depth: usize,
    queue_capacity: usize,
    wal_files: usize,
    last_success_ms: u64,
    last_failure_ms: u64,
    now_ms: u64,
) -> HealthStatus {
    const DOWN_WINDOW_MS: u64 = 5 * 60_000;
    if stopped {
        return HealthStatus::Stopped;
    }
    let failing_now = last_failure_ms > last_success_ms;
    let backlog = wal_files > 0 || queue_depth > 0;
    if failing_now && backlog && now_ms.saturating_sub(last_failure_ms) < DOWN_WINDOW_MS {
        return HealthStatus::Down;
    }
    let nearly_full = queue_depth as f64 > 0.8 * queue_capacity.max(1) as f64;
    if failing_now || wal_files > 0 || nearly_full {
        return HealthStatus::Degraded;
    }
    HealthStatus::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        let now = 10_000_000;
        assert_eq!(classify(false, 0, 100, 0, 0, 0, now), HealthStatus::Ok);
        assert_eq!(
            classify(false, 0, 100, 0, now - 10, now - 9_000, now),
            HealthStatus::Ok,
            "an old failure followed by success is fine"
        );
        assert_eq!(
            classify(false, 0, 100, 2, now - 10, 0, now),
            HealthStatus::Degraded,
            "WAL backlog"
        );
        assert_eq!(
            classify(false, 90, 100, 0, now - 10, 0, now),
            HealthStatus::Degraded,
            "queue nearly full"
        );
        assert_eq!(
            classify(false, 0, 100, 0, now - 5_000, now - 100, now),
            HealthStatus::Degraded,
            "last attempt failed but nothing is stuck"
        );
        assert_eq!(
            classify(false, 0, 100, 3, now - 5_000, now - 100, now),
            HealthStatus::Down
        );
        assert_eq!(
            classify(false, 0, 100, 3, 0, now - 9 * 60_000, now),
            HealthStatus::Degraded,
            "a stale failure no longer counts as down"
        );
        assert_eq!(classify(true, 0, 100, 0, 0, 0, now), HealthStatus::Stopped);
        assert_eq!(HealthStatus::Down.http_status(), 503);
        assert_eq!(HealthStatus::Degraded.http_status(), 200);
    }
}
