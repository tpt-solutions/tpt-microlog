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
    /// path.
    pub fn process(&self, entry: &mut LogEntry) -> bool {
        // Drop filters apply to everything, including metrics.
        if let Some(re) = &self.drop_regex {
            if re.is_match(&entry.message) {
                return false;
            }
        }
        if !self.drop_tags.is_empty() {
            let matches_tag = self
                .drop_tags
                .iter()
                .any(|(key, value)| entry.metadata.get(key).is_some_and(|v| v == value));
            if matches_tag {
                return false;
            }
        }
        // Level filtering and sampling never apply to metrics or spans.
        if entry.log_type == LogType::Log {
            if let Some(min) = self.min_level {
                if entry.level < min {
                    return false;
                }
            }
        }
        if let Some(rate) = self.sample_rate_for(entry) {
            if rate < 1.0 {
                let roll = {
                    use rand::Rng;
                    rand::thread_rng().gen::<f64>()
                };
                if roll > rate {
                    return false;
                }
            }
        }
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
        true
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
}
