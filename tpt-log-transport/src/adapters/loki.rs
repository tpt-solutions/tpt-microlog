//! Grafana Loki adapter — HTTP push API.
//!
//! POSTs to `<endpoint>/loki/api/v1/push` (endpoint defaults to
//! `http://localhost:3100`). Entries are grouped into streams by
//! `(service, level)` label pairs; the message is the line and the entry
//! metadata rides along as Loki structured metadata. `api_key` is sent as a
//! Bearer token (Grafana Cloud users typically prefer `es_basic_auth`-style
//! basic auth via `extra_headers`).

use std::collections::BTreeMap;

use tpt_log_core::{CompressedBatch, Config, LogEntry, Transport, TransportError};

use crate::http::{client_from_config, request_once};

/// Loki push API adapter.
pub struct LokiAdapter {
    client: reqwest::blocking::Client,
    url: String,
    headers: Vec<(String, String)>,
}

impl std::fmt::Debug for LokiAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LokiAdapter")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

fn loki_level(entry: &LogEntry) -> String {
    entry.level.to_string().to_ascii_uppercase()
}

/// One log line: `(timestamp, line, structured metadata)`.
type StreamValue = (u64, String, serde_json::Value);

/// Build the push body: streams grouped by (service, level) labels, values
/// sorted by timestamp (Loki requires ordering within a stream).
fn build_body(entries: &[LogEntry]) -> serde_json::Value {
    let mut streams: BTreeMap<(String, String), Vec<StreamValue>> = BTreeMap::new();
    for e in entries {
        let service = e
            .metadata
            .get("service")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let key = (service, loki_level(e));
        let line = {
            let mut pairs: Vec<String> = e
                .metadata
                .iter()
                .filter(|(k, _)| {
                    let k = k.as_str();
                    k != "service" && k != "host" && k != "pid"
                })
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            pairs.sort();
            if pairs.is_empty() {
                e.message.clone()
            } else {
                format!("{} {}", e.message, pairs.join(" "))
            }
        };
        let metadata = serde_json::Value::Object(e.metadata.clone());
        streams
            .entry(key)
            .or_default()
            .push((e.timestamp, line, metadata));
    }

    let streams: Vec<serde_json::Value> = streams
        .into_iter()
        .map(|((service, level), mut values)| {
            values.sort_by_key(|(ts, _, _)| *ts);
            let values: Vec<serde_json::Value> = values
                .into_iter()
                .map(|(ts, line, meta)| serde_json::json!([ts.to_string(), line, meta]))
                .collect();
            serde_json::json!({
                "stream": { "service": service, "level": level },
                "values": values,
            })
        })
        .collect();
    serde_json::json!({ "streams": streams })
}

impl LokiAdapter {
    /// Build from core config. `endpoint` defaults to `http://localhost:3100`.
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let base = config
            .endpoint
            .clone()
            .unwrap_or_else(|| "http://localhost:3100".to_string());
        let url = format!("{}/loki/api/v1/push", base.trim_end_matches('/'));
        let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        for raw in &config.extra_headers {
            match raw.split_once('=') {
                Some((k, v)) => headers.push((k.trim().to_string(), v.trim().to_string())),
                None => {
                    return Err(TransportError::Config(format!(
                        "malformed extra_header '{raw}' (expected name=value)"
                    )));
                }
            }
        }
        if let Some(key) = &config.api_key {
            headers.push((
                "Authorization".to_string(),
                format!("Bearer {}", key.expose()),
            ));
        }
        Ok(Self {
            client: client_from_config(config),
            url,
            headers,
        })
    }
}

impl Transport for LokiAdapter {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        let entries = batch
            .decompress_entries()
            .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;
        let body = serde_json::to_vec(&build_body(&entries))
            .map_err(|e| TransportError::Rejected(format!("serialize failed: {e}")))?;
        request_once(
            &self.client,
            &self.url,
            &self.headers,
            "application/json",
            body,
        )
    }

    fn name(&self) -> &str {
        "loki"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_log_core::{LogLevel, LogType};

    #[test]
    fn streams_group_by_service_and_level_and_sort_values() {
        let mut a = LogEntry::new(LogLevel::Info, "first");
        a.timestamp = 2_000;
        a.metadata
            .insert("service".into(), serde_json::json!("svc"));
        let mut b = LogEntry::new(LogLevel::Error, "boom");
        b.timestamp = 1_000;
        b.metadata
            .insert("service".into(), serde_json::json!("svc"));
        let mut other = LogEntry::new(LogLevel::Info, "other stream");
        other.timestamp = 3_000;
        other
            .metadata
            .insert("service".into(), serde_json::json!("svc2"));

        let body = build_body(&[a, b, other]);
        let streams = body["streams"].as_array().unwrap();
        assert_eq!(streams.len(), 3, "{body}");
        // (svc, ERROR) < (svc, INFO) < (svc2, INFO) in BTreeMap order.
        assert_eq!(streams[0]["stream"]["level"], "ERROR");
        assert_eq!(streams[0]["values"][0][0], "1000");
        assert_eq!(streams[0]["values"][0][1], "boom");
        assert_eq!(streams[1]["values"][0][0], "2000");
        assert_eq!(streams[2]["stream"]["service"], "svc2");
    }

    #[test]
    fn entries_survive_the_shape() {
        let e = LogEntry {
            timestamp: 5,
            level: LogLevel::Info,
            log_type: LogType::Log,
            message: "x".into(),
            metadata: Default::default(),
        };
        let body = build_body(&[e]);
        assert_eq!(body["streams"][0]["stream"]["service"], "unknown");
    }
}
