use std::sync::Arc;

use crate::governor::{
    adaptive_rate, DailyBudget, Decision, Dedup, DedupOutcome, FlightRecorder, RateLimiter,
};
use crate::{LogEntry, LogLevel, LogType};

/// Pre-send transforms applied to every entry before batching.
///
/// Honors the spec's optional features (spec §8): level filtering, tag and
/// regex-on-message drop filters, sampling, and enrichment of hostname/PID/
/// service/env vars. Metrics and trace spans bypass level filtering and
/// sampling; WARN and above are never sampled.
#[derive(Clone)]
pub struct EntryProcessor {
    pub min_level: Option<LogLevel>,
    /// Entries carrying any of these `tag = value` pairs are dropped.
    pub drop_tags: Vec<(String, serde_json::Value)>,
    /// Entries whose message matches are dropped.
    pub drop_regex: Option<regex::Regex>,
    /// Global sampling rate applied to entries with no per-level override.
    pub sample_rate: f64,
    /// Per-level sampling overrides (level → rate).
    pub sample_rate_by_level: Vec<(LogLevel, f64)>,
    pub enrichment: Option<EnrichmentContext>,
    /// PII/secret redaction, applied last so dropped entries cost nothing.
    pub redactor: crate::redact::Redactor,
    /// Fold bursts of identical entries (`dedup_window_ms`).
    pub dedup: Option<Arc<Dedup>>,
    /// Cap routine entries per second (`rate_limit_per_sec`).
    pub rate_limiter: Option<Arc<RateLimiter>>,
    /// Sample routine entries harder as the queue fills (floor = lowest rate).
    pub adaptive_floor: Option<f64>,
    /// Daily volume budget that tightens sampling automatically.
    pub budget: Option<Arc<DailyBudget>>,
    /// Error-triggered flight recorder for low-severity context.
    pub recorder: Option<Arc<FlightRecorder>>,
}

impl std::fmt::Debug for EntryProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntryProcessor")
            .field("min_level", &self.min_level)
            .field("drop_tags", &self.drop_tags)
            .field("drop_regex", &self.drop_regex.as_ref().map(|_| ".."))
            .field("sample_rate", &self.sample_rate)
            .field("sample_rate_by_level", &self.sample_rate_by_level)
            .field("enrichment", &self.enrichment)
            .field("dedup", &self.dedup.is_some())
            .field("rate_limiter", &self.rate_limiter.is_some())
            .field("adaptive_floor", &self.adaptive_floor)
            .field("budget", &self.budget.is_some())
            .field("recorder", &self.recorder.is_some())
            .finish()
    }
}

impl Default for EntryProcessor {
    fn default() -> Self {
        Self {
            min_level: None,
            drop_tags: Vec::new(),
            drop_regex: None,
            sample_rate: 1.0,
            sample_rate_by_level: Vec::new(),
            enrichment: None,
            redactor: crate::redact::Redactor::default(),
            dedup: None,
            rate_limiter: None,
            adaptive_floor: None,
            budget: None,
            recorder: None,
        }
    }
}

/// Static facts injected into every log entry.
#[derive(Clone, Debug)]
pub struct EnrichmentContext {
    pub host: String,
    pub pid: String,
    pub service: String,
    /// Captured environment variables (`name`, `value`).
    pub env_vars: Vec<(String, String)>,
}

impl EntryProcessor {
    /// Whether any transformation is active (callers can use this to skip the
    /// processor entirely when the configuration is a pass-through).
    pub fn is_enabled(&self) -> bool {
        self.min_level.is_some()
            || (self.sample_rate - 1.0).abs() > f64::EPSILON
            || !self.sample_rate_by_level.is_empty()
            || self.enrichment.is_some()
            || self.drop_regex.is_some()
            || !self.drop_tags.is_empty()
            || self.redactor.is_enabled()
            || self.dedup.is_some()
            || self.rate_limiter.is_some()
            || self.adaptive_floor.is_some()
            || self.budget.is_some()
            || self.recorder.is_some()
    }

    /// Effective sampling rate for `entry`: WARN and above are never sampled,
    /// per-level overrides win over the global rate, metrics/traces are
    /// exempt.
    fn sample_rate_for(&self, entry: &LogEntry) -> Option<f64> {
        if entry.log_type != LogType::Log {
            return None; // metrics and trace spans always go through
        }
        if entry.level >= LogLevel::Warn {
            return None; // never sample WARN / ERROR / FATAL
        }
        if let Some((_level, rate)) = self
            .sample_rate_by_level
            .iter()
            .find(|(level, _)| *level == entry.level)
        {
            return Some(*rate);
        }
        Some(self.sample_rate)
    }

    /// Filter/enrich `entry` in place. Returns `false` when the entry should
    /// be dropped. In-place mutation avoids a copy of every entry on the hot
    /// path. See [`EntryProcessor::process_with`] for the reason it was dropped.
    pub fn process(&self, entry: &mut LogEntry) -> bool {
        self.process_with(entry, 0.0) == Decision::Keep
    }

    /// Like [`process`](Self::process), but aware of queue `pressure`
    /// (0.0 empty .. 1.0 full) for adaptive sampling, and reporting *why* an
    /// entry was dropped.
    ///
    /// Order: drop filters, flight recorder, `min_level`, sampling (static x
    /// adaptive x daily budget), rate limit, de-duplication, then enrichment
    /// and redaction. WARN and above (and metrics/spans) skip sampling and
    /// rate limiting.
    pub fn process_with(&self, entry: &mut LogEntry, pressure: f64) -> Decision {
        // Drop filters apply to everything, including metrics.
        if let Some(re) = &self.drop_regex {
            if re.is_match(&entry.message) {
                return Decision::Filtered;
            }
        }
        if !self.drop_tags.is_empty() {
            let matches_tag = self
                .drop_tags
                .iter()
                .any(|(key, value)| entry.metadata.get(key).is_some_and(|v| v == value));
            if matches_tag {
                return Decision::Filtered;
            }
        }
        // Low-severity entries wait in the flight recorder for an error. They
        // are redacted before being stored, never shipped on their own.
        if let Some(recorder) = &self.recorder {
            if recorder.captures(entry) {
                let mut held = entry.clone();
                self.finalize(&mut held);
                recorder.record(held);
                return Decision::Recorded;
            }
        }
        // Level filtering and sampling never apply to metrics or spans.
        if entry.log_type == LogType::Log {
            if let Some(min) = self.min_level {
                if entry.level < min {
                    return Decision::Filtered;
                }
            }
        }
        let routine = entry.log_type == LogType::Log && entry.level < LogLevel::Warn;
        let mut rate = self.sample_rate_for(entry).unwrap_or(1.0);
        if routine {
            if let Some(floor) = self.adaptive_floor {
                rate *= adaptive_rate(pressure, floor);
            }
            if let Some(budget) = &self.budget {
                rate *= budget.offer(entry.timestamp);
            }
        }
        if rate < 1.0 {
            let roll = {
                use rand::Rng;
                rand::thread_rng().gen::<f64>()
            };
            if roll > rate {
                return Decision::Sampled;
            }
        }
        if routine {
            if let Some(limiter) = &self.rate_limiter {
                if !limiter.allow(entry.timestamp) {
                    return Decision::RateLimited;
                }
            }
        }
        if entry.log_type == LogType::Log {
            if let Some(dedup) = &self.dedup {
                match dedup.check(entry) {
                    DedupOutcome::Suppress => return Decision::Deduplicated,
                    DedupOutcome::Pass {
                        repeat_count: Some(n),
                    } => {
                        entry.metadata.insert("repeat_count".into(), n.into());
                    }
                    DedupOutcome::Pass { repeat_count: None } => {}
                }
            }
        }
        if let Some(budget) = &self.budget {
            budget.spend(entry.timestamp);
        }
        self.finalize(entry);
        Decision::Keep
    }

    /// Enrichment and redaction: the last step for every delivered entry.
    pub fn finalize(&self, entry: &mut LogEntry) {
        if let Some(enc) = &self.enrichment {
            entry
                .metadata
                .entry("host".to_string())
                .or_insert(serde_json::Value::String(enc.host.clone()));
            entry
                .metadata
                .entry("pid".to_string())
                .or_insert(serde_json::Value::String(enc.pid.clone()));
            entry
                .metadata
                .entry("service".to_string())
                .or_insert(serde_json::Value::String(enc.service.clone()));
            for (name, value) in &enc.env_vars {
                let key = format!("env_{}", name.to_ascii_lowercase());
                entry
                    .metadata
                    .entry(key)
                    .or_insert(serde_json::Value::String(value.clone()));
            }
        }
        self.redactor.apply(entry);
    }

    /// Context to ship ahead of `entry` when it is an ERROR/FATAL: everything
    /// the flight recorder holds (oldest first). Empty otherwise.
    pub fn take_flight_context(&self, entry: &LogEntry) -> Vec<LogEntry> {
        match &self.recorder {
            Some(recorder) if entry.log_type == LogType::Log && entry.level >= LogLevel::Error => {
                recorder.take()
            }
            _ => Vec::new(),
        }
    }

    /// `repeat_count` summaries for de-duplication windows that closed (or,
    /// with `force`, every open one: flush and shutdown).
    pub fn drain_summaries(&self, now_ms: u64, force: bool) -> Vec<LogEntry> {
        let Some(dedup) = &self.dedup else {
            return Vec::new();
        };
        let mut out = dedup.drain_summaries(now_ms, force);
        for entry in &mut out {
            self.finalize(entry);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LogType;

    fn processor() -> EntryProcessor {
        EntryProcessor {
            enrichment: Some(EnrichmentContext {
                host: "box-1".into(),
                pid: "42".into(),
                service: "svc".into(),
                env_vars: vec![("DEPLOY_ENV".into(), "prod".into())],
            }),
            ..Default::default()
        }
    }

    #[test]
    fn drops_below_min_level() {
        let p = EntryProcessor {
            min_level: Some(LogLevel::Warn),
            ..Default::default()
        };
        let mut info = LogEntry::new(LogLevel::Info, "x");
        let mut warn = LogEntry::new(LogLevel::Warn, "y");
        assert!(!p.process(&mut info));
        assert!(p.process(&mut warn));
    }

    #[test]
    fn metrics_bypass_min_level_and_sampling() {
        let p = EntryProcessor {
            min_level: Some(LogLevel::Fatal),
            sample_rate: 0.0, // would drop everything if applied
            ..Default::default()
        };
        let mut metric = LogEntry::metric("cpu", 0.5, None);
        assert!(
            p.process(&mut metric),
            "metrics survive min_level + sampling"
        );
        let mut span = LogEntry::new(LogLevel::Debug, "span");
        span.log_type = LogType::Trace;
        assert!(p.process(&mut span), "spans survive min_level + sampling");
        let mut log = LogEntry::new(LogLevel::Debug, "log");
        assert!(!p.process(&mut log));
    }

    #[test]
    fn warn_and_above_never_sampled() {
        let p = EntryProcessor {
            sample_rate: 0.0,
            ..Default::default()
        };
        for level in [LogLevel::Warn, LogLevel::Error, LogLevel::Fatal] {
            let mut e = LogEntry::new(level, "important");
            assert!(p.process(&mut e), "{level:?} must never be sampled");
        }
    }

    #[test]
    fn per_level_sampling_map() {
        let p = EntryProcessor {
            sample_rate: 1.0,
            sample_rate_by_level: vec![(LogLevel::Debug, 0.0)],
            ..Default::default()
        };
        let mut debug = LogEntry::new(LogLevel::Debug, "chatty");
        assert!(!p.process(&mut debug));
        let mut info = LogEntry::new(LogLevel::Info, "normal");
        assert!(p.process(&mut info));
    }

    #[test]
    fn is_enabled_accounts_for_filters() {
        let p = EntryProcessor {
            drop_regex: Some(regex::Regex::new("noise").unwrap()),
            ..Default::default()
        };
        assert!(p.is_enabled());
        let p2 = EntryProcessor {
            drop_tags: vec![("k".into(), serde_json::json!(true))],
            ..Default::default()
        };
        assert!(p2.is_enabled());
        let p3 = EntryProcessor::default();
        assert!(!p3.is_enabled());
    }

    #[test]
    fn enriches_entries_in_place() {
        let p = processor();
        let mut e = LogEntry::new(LogLevel::Info, "m");
        e.log_type = LogType::Log;
        assert!(p.process(&mut e));
        assert_eq!(
            e.metadata["host"],
            serde_json::Value::String("box-1".into())
        );
        assert_eq!(e.metadata["pid"], serde_json::Value::String("42".into()));
        assert_eq!(
            e.metadata["service"],
            serde_json::Value::String("svc".into())
        );
        assert_eq!(
            e.metadata["env_deploy_env"],
            serde_json::Value::String("prod".into())
        );
    }

    #[test]
    fn sampling_keeps_some() {
        let p = EntryProcessor {
            sample_rate: 0.5,
            ..Default::default()
        };
        let mut kept = 0;
        for _ in 0..500 {
            let mut e = LogEntry::new(LogLevel::Info, "x");
            if p.process(&mut e) {
                kept += 1;
            }
        }
        // Sanity window: far from 0 and far from 500.
        assert!(kept > 100 && kept < 450, "kept={kept}");
    }

    #[test]
    fn drops_matching_regex() {
        let p = EntryProcessor {
            drop_regex: Some(regex::Regex::new("health.?check").unwrap()),
            ..Default::default()
        };
        let mut a = LogEntry::new(LogLevel::Info, "healthcheck ok");
        let mut b = LogEntry::new(LogLevel::Info, "health-check ok");
        let mut c = LogEntry::new(LogLevel::Info, "real event");
        assert!(!p.process(&mut a));
        assert!(!p.process(&mut b));
        assert!(p.process(&mut c));
    }

    #[test]
    fn drops_matching_tags() {
        let p = EntryProcessor {
            drop_tags: vec![("noise".to_string(), serde_json::Value::Bool(true))],
            ..Default::default()
        };
        let mut noisy = LogEntry::new(LogLevel::Info, "tick");
        noisy
            .metadata
            .insert("noise".into(), serde_json::json!(true));
        let mut tagged = LogEntry::new(LogLevel::Info, "tick");
        tagged
            .metadata
            .insert("noise".into(), serde_json::json!(false));
        let mut clean = LogEntry::new(LogLevel::Info, "no tags");
        assert!(!p.process(&mut noisy));
        assert!(p.process(&mut tagged));
        assert!(p.process(&mut clean));
    }

    #[test]
    fn adaptive_sampling_only_engages_under_pressure_and_spares_warnings() {
        let p = EntryProcessor {
            adaptive_floor: Some(0.0),
            ..Default::default()
        };
        let keep = |level, pressure| {
            (0..200)
                .filter(|_| {
                    let mut e = LogEntry::new(level, "x");
                    p.process_with(&mut e, pressure) == Decision::Keep
                })
                .count()
        };
        assert_eq!(
            keep(LogLevel::Info, 0.2),
            200,
            "idle queue: nothing sampled"
        );
        assert_eq!(
            keep(LogLevel::Info, 1.0),
            0,
            "full queue with floor 0: routine dropped"
        );
        assert_eq!(keep(LogLevel::Error, 1.0), 200, "errors always pass");
        let half = keep(LogLevel::Debug, 0.75);
        assert!(
            (60..=140).contains(&half),
            "~50% at 75% pressure, got {half}"
        );
        let mut dropped = LogEntry::new(LogLevel::Info, "x");
        assert_eq!(p.process_with(&mut dropped, 1.0), Decision::Sampled);
    }
}
