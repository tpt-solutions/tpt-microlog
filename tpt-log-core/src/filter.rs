use crate::{LogEntry, LogLevel};

/// Pre-send transforms applied to every entry before batching.
///
/// Honors the spec's optional features (spec §8): level filtering, tag and
/// regex-on-message drop filters, sampling, and enrichment of hostname/PID/
/// service.
#[derive(Clone)]
pub struct EntryProcessor {
    pub min_level: Option<LogLevel>,
    /// Entries carrying any of these `tag = value` pairs are dropped.
    pub drop_tags: Vec<(String, serde_json::Value)>,
    /// Entries whose message matches are dropped.
    pub drop_regex: Option<regex::Regex>,
    pub sample_rate: f64,
    pub enrichment: Option<EnrichmentContext>,
}

impl std::fmt::Debug for EntryProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntryProcessor")
            .field("min_level", &self.min_level)
            .field("drop_tags", &self.drop_tags)
            .field("drop_regex", &self.drop_regex.as_ref().map(|_| ".."))
            .field("sample_rate", &self.sample_rate)
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
}

impl EntryProcessor {
    pub fn is_enabled(&self) -> bool {
        self.min_level.is_some()
            || (self.sample_rate - 1.0).abs() > f64::EPSILON
            || self.enrichment.is_some()
    }

    /// Returns `None` if the entry should be dropped, otherwise a
    /// (possibly enriched) copy. `rng` is optional sampling randomness; pass
    /// `None` to default to a process-local fast RNG.
    pub fn process(&self, entry: &LogEntry) -> Option<LogEntry> {
        if let Some(min) = self.min_level {
            if entry.level < min {
                return None;
            }
        }
        if let Some(re) = &self.drop_regex {
            if re.is_match(&entry.message) {
                return None;
            }
        }
        if !self.drop_tags.is_empty() {
            let matches_tag = self
                .drop_tags
                .iter()
                .any(|(key, value)| entry.metadata.get(key).is_some_and(|v| v == value));
            if matches_tag {
                return None;
            }
        }
        if self.sample_rate < 1.0 {
            let roll = {
                use rand::Rng;
                let mut rng = rand::thread_rng();
                rng.gen::<f64>()
            };
            if roll > self.sample_rate {
                return None;
            }
        }
        let mut out = entry.clone();
        if let Some(enc) = &self.enrichment {
            out.metadata
                .entry("host".to_string())
                .or_insert(serde_json::Value::String(enc.host.clone()));
            out.metadata
                .entry("pid".to_string())
                .or_insert(serde_json::Value::String(enc.pid.clone()));
            out.metadata
                .entry("service".to_string())
                .or_insert(serde_json::Value::String(enc.service.clone()));
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LogType;

    #[test]
    fn drops_below_min_level() {
        let p = EntryProcessor {
            min_level: Some(LogLevel::Warn),
            ..Default::default()
        };
        let info = LogEntry::new(LogLevel::Info, "x");
        let warn = LogEntry::new(LogLevel::Warn, "y");
        assert!(p.process(&info).is_none());
        assert!(p.process(&warn).is_some());
    }

    #[test]
    fn enriches_entries() {
        let p = EntryProcessor {
            enrichment: Some(EnrichmentContext {
                host: "box-1".into(),
                pid: "42".into(),
                service: "svc".into(),
            }),
            ..Default::default()
        };
        let mut e = LogEntry::new(LogLevel::Info, "m");
        e.log_type = LogType::Log;
        let out = p.process(&e).unwrap();
        assert_eq!(
            out.metadata["host"],
            serde_json::Value::String("box-1".into())
        );
        assert_eq!(out.metadata["pid"], serde_json::Value::String("42".into()));
        assert_eq!(
            out.metadata["service"],
            serde_json::Value::String("svc".into())
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
            if p.process(&LogEntry::new(LogLevel::Info, "x")).is_some() {
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
        assert!(p
            .process(&LogEntry::new(LogLevel::Info, "healthcheck ok"))
            .is_none());
        assert!(p
            .process(&LogEntry::new(LogLevel::Info, "health-check ok"))
            .is_none());
        assert!(p
            .process(&LogEntry::new(LogLevel::Info, "real event"))
            .is_some());
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
        assert!(p.process(&noisy).is_none());
        assert!(p.process(&tagged).is_some());
        assert!(p
            .process(&LogEntry::new(LogLevel::Info, "no tags"))
            .is_some());
    }
}
