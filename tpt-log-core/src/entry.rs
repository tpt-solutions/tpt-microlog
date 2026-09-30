use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{LogLevel, LogType};

/// A single telemetry entry (spec §5.1).
///
/// ```text
/// LogEntry { timestamp, level, type, message, metadata: HashMap<String, Value> }
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    /// Unix epoch time in milliseconds.
    pub timestamp: u64,
    /// Severity level.
    pub level: LogLevel,
    /// Classification (Log / Metric / Trace).
    #[serde(rename = "type")]
    pub log_type: LogType,
    /// Message text, or the metric/span name for `Metric`/`Trace`.
    pub message: String,
    /// Arbitrary structured key-value metadata.
    pub metadata: Map<String, Value>,
}

impl LogEntry {
    /// Build a standard `Log` entry.
    pub fn new(level: LogLevel, message: impl Into<String>) -> Self {
        let now = now_millis();
        Self {
            timestamp: now,
            level,
            log_type: LogType::Log,
            message: message.into(),
            metadata: Map::new(),
        }
    }

    /// Build a `Metric` entry with an optional tag set.
    pub fn metric(name: impl Into<String>, value: f64, tags: Option<&Map<String, Value>>) -> Self {
        let mut metadata = Map::new();
        metadata.insert("value".to_string(), Value::from(value));
        if let Some(tags) = tags {
            metadata.insert("tags".to_string(), Value::Object(tags.clone()));
        }
        Self {
            timestamp: now_millis(),
            level: LogLevel::Info,
            log_type: LogType::Metric,
            message: name.into(),
            metadata,
        }
    }

    /// Add a single metadata key/value pair.
    pub fn with<T: Into<Value>>(mut self, key: impl Into<String>, value: T) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Extend metadata from an iterator of `(key, value)` pairs.
    pub fn with_many<K>(mut self, pairs: impl IntoIterator<Item = (K, Value)>) -> Self
    where
        K: Into<String>,
    {
        for (k, v) in pairs {
            self.metadata.insert(k.into(), v);
        }
        self
    }

    /// Approximate in-memory size useful for memory-based batching.
    pub fn approx_size(&self) -> usize {
        let base = self.message.len() + 32;
        base + self.metadata.values().map(json_size).sum::<usize>()
    }
}

/// Current time in milliseconds since the Unix epoch.
pub fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as u64,
        Err(_) => 0,
    }
}

/// Rough JSON-encoded size of a value (bytes, keys, and per-node overhead).
fn json_size(value: &Value) -> usize {
    use serde_json::Value as V;
    const NODE_OVERHEAD: usize = 24;
    match value {
        V::Null | V::Bool(_) | V::Number(_) => NODE_OVERHEAD,
        V::String(s) => s.len() + NODE_OVERHEAD,
        V::Array(items) => NODE_OVERHEAD + items.iter().map(json_size).sum::<usize>(),
        V::Object(map) => {
            NODE_OVERHEAD
                + map
                    .iter()
                    .map(|(k, v)| k.len() + json_size(v))
                    .sum::<usize>()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_log_entry() {
        let e = LogEntry::new(LogLevel::Warn, "disk nearly full").with("device", "/dev/sda1");
        assert_eq!(e.level, LogLevel::Warn);
        assert_eq!(e.log_type, LogType::Log);
        assert_eq!(e.metadata["device"], Value::from("/dev/sda1"));
        assert!(e.timestamp > 1_000_000_000_000);
    }

    #[test]
    fn builds_metric_entry() {
        let mut tags = Map::new();
        tags.insert("host".to_string(), Value::from("server-01"));
        let m = LogEntry::metric("cpu_usage", 42.5, Some(&tags));
        assert_eq!(m.log_type, LogType::Metric);
        assert_eq!(m.message, "cpu_usage");
        assert_eq!(m.metadata["value"], Value::from(42.5));
        assert_eq!(m.metadata["tags"]["host"], Value::from("server-01"));
    }

    #[test]
    fn roundtrips_json() {
        let e = LogEntry::new(LogLevel::Info, "hello").with("n", 123i64);
        let s = serde_json::to_string(&e).unwrap();
        let back: LogEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(back.message, "hello");
        assert_eq!(back.metadata["n"], Value::from(123i64));
        assert_eq!(back.timestamp, e.timestamp);
    }
}
