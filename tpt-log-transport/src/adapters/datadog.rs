//! Datadog adapter — Logs API v2 (spec §5.3).
//!
//! POSTs an array of Datadog log events to the intake API with the
//! `DD-API-KEY` header. The endpoint defaults to
//! `https://http-intake.logs.datadoghq.com/api/v2/logs`, is derived from
//! `datadog_site` when set (e.g. `datadoghq.eu`), and can be overridden
//! explicitly (proxies and tests).
//!
//! Requests are split at Datadog's intake limits: max 1000 log events and
//! ~5 MB per request. With `datadog_metrics = true`, `metric()` entries are
//! additionally shipped through the Metrics API (`/api/v2/series`).

use tpt_log_core::{CompressedBatch, Config, LogEntry, LogLevel, LogType, Transport, TransportError};

use crate::http::{pooled_client, request_once, request_once_full};

const DEFAULT_ENDPOINT: &str = "https://http-intake.logs.datadoghq.com/api/v2/logs";
const DEFAULT_SITE: &str = "datadoghq.com";
/// Datadog intake caps a logs request at 1000 events.
const MAX_EVENTS_PER_REQUEST: usize = 1_000;
/// Conservative body ceiling (Datadog allows 5 MB raw).
const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;

/// Datadog Logs API v2 adapter.
pub struct DatadogAdapter {
    client: reqwest::blocking::Client,
    url: String,
    metrics_url: Option<String>,
    headers: Vec<(String, String)>,
}

impl std::fmt::Debug for DatadogAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatadogAdapter")
            .field("url", &self.url)
            .field("metrics_url", &self.metrics_url)
            .finish_non_exhaustive()
    }
}

fn status_for(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Trace | LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warning",
        LogLevel::Error => "error",
        LogLevel::Fatal => "critical",
    }
}

/// Metadata keys that map to dedicated Datadog fields (or are metric-only).
const RESERVED_KEYS: &[&str] = &["host", "service", "source", "pid", "value", "tags", "level"];

/// Map one entry to a Datadog v2 log event object. Metadata travels as
/// top-level attributes (the intake indexes arbitrary top-level keys), not as
/// the legacy `ddtags` string.
fn to_dd_event(entry: &LogEntry) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "message".into(),
        serde_json::Value::String(entry.message.clone()),
    );
    obj.insert(
        "status".into(),
        serde_json::Value::String(status_for(entry.level).into()),
    );
    obj.insert("timestamp".into(), serde_json::Value::from(entry.timestamp));

    let source = entry
        .metadata
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("tpt-microlog");
    obj.insert("ddsource".into(), serde_json::Value::String(source.into()));

    if let Some(service) = entry.metadata.get("service").and_then(|v| v.as_str()) {
        obj.insert("service".into(), serde_json::Value::String(service.into()));
    }
    if let Some(host) = entry.metadata.get("host").and_then(|v| v.as_str()) {
        obj.insert("hostname".into(), serde_json::Value::String(host.into()));
    }

    // Remaining metadata becomes top-level attributes; a nested `tags` object
    // is kept as an attribute object (queryable with @tags.key).
    for (k, v) in &entry.metadata {
        if RESERVED_KEYS.contains(&k.as_str()) && k != "tags" {
            continue;
        }
        obj.entry(k.clone())
            .or_insert_with(|| v.clone());
    }
    serde_json::Value::Object(obj)
}

/// Render one `metric()` entry for the Datadog Metrics API v2
/// (`{"series": [{metric, points: [{timestamp, value}], tags}]}`).
fn to_dd_series_point(entry: &LogEntry) -> Option<serde_json::Value> {
    if entry.log_type != LogType::Metric {
        return None;
    }
    let value = entry.metadata.get("value")?.as_f64()?;
    let metric: String = entry
        .message
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let tags: Vec<String> = entry
        .metadata
        .get("tags")
        .and_then(|t| t.as_object())
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| format!("{}:{}", sanitize_tag_key(k), render_tag_value(v)))
                .collect()
        })
        .unwrap_or_default();
    Some(serde_json::json!({
        "metric": metric,
        "type": 3, // gauge
        "points": [{ "timestamp": entry.timestamp, "value": value }],
        "tags": tags,
    }))
}

fn sanitize_tag_key(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '/' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn render_tag_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl DatadogAdapter {
    /// Build from core config. Requires `api_key`; the endpoint comes from
    /// `endpoint` if set, else from `datadog_site`, else the US default.
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let key = config
            .api_key
            .as_ref()
            .map(|s| s.expose().to_string())
            .ok_or_else(|| TransportError::Config("Datadog adapter needs an `api_key`".into()))?;

        let site = config.datadog_site.as_deref().unwrap_or(DEFAULT_SITE);
        let default_intake = format!("https://http-intake.logs.{site}/api/v2/logs");
        let url = config
            .endpoint
            .clone()
            .unwrap_or(default_intake);

        // Metrics API base: derive from an explicitly overridden endpoint by
        // replacing the path, else from the site (api.<site>/api/v2/series).
        let metrics_url = if config.datadog_metrics {
            Some(match &config.endpoint {
                Some(endpoint) => match endpoint.split_once("/api/") {
                    Some((base, _)) => format!("{base}/api/v2/series"),
                    None => format!("{}/api/v2/series", endpoint.trim_end_matches('/')),
                },
                None => format!("https://api.{site}/api/v2/series"),
            })
        } else {
            None
        };

        let mut headers = vec![("DD-API-KEY".to_string(), key)];
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
        Ok(Self {
            client: pooled_client(),
            url,
            metrics_url,
            headers,
        })
    }

    /// Split entries into request-sized chunks: at most
    /// [`MAX_EVENTS_PER_REQUEST`] events and an approximate body budget of
    /// [`MAX_BODY_BYTES`] per request.
    fn chunk_entries(entries: &[serde_json::Value]) -> Vec<&[serde_json::Value]> {
        let mut chunks = Vec::new();
        let mut start = 0usize;
        let mut bytes = 0usize;
        let mut last_flushed = 0usize;
        for (i, event) in entries.iter().enumerate() {
            let approx = serde_json::to_string(event).map(|s| s.len()).unwrap_or(0) + 1;
            let count = i - start + 1;
            if count > 1 && (count > MAX_EVENTS_PER_REQUEST || bytes + approx > MAX_BODY_BYTES) {
                chunks.push(&entries[last_flushed..i]);
                last_flushed = i;
                start = i;
                bytes = approx;
            } else {
                bytes += approx;
            }
        }
        if last_flushed < entries.len() {
            chunks.push(&entries[last_flushed..]);
        }
        chunks
    }
}

impl Transport for DatadogAdapter {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        let entries = batch
            .decompress_entries()
            .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;

        // With datadog_metrics enabled, metric entries also go to the
        // Metrics API; they still appear in the log stream.
        if let Some(metrics_url) = &self.metrics_url {
            let mut series = Vec::new();
            for entry in &entries {
                if let Some(point) = to_dd_series_point(entry) {
                    series.push(point);
                }
            }
            if !series.is_empty() {
                let body = serde_json::to_vec(&serde_json::json!({ "series": series }))
                    .map_err(|e| TransportError::Rejected(format!("serialize failed: {e}")))?;
                request_once(&self.client, metrics_url, &self.headers, "application/json", body)?;
            }
        }

        let events: Vec<serde_json::Value> = entries.iter().map(to_dd_event).collect();
        for chunk in Self::chunk_entries(&events) {
            let body = serde_json::to_vec(chunk)
                .map_err(|e| TransportError::Rejected(format!("serialize failed: {e}")))?;
            request_once(
                &self.client,
                &self.url,
                &self.headers,
                "application/json",
                body,
            )?;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "datadog"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_log_core::Secret;

    fn event(message: &str, pad: usize) -> serde_json::Value {
        serde_json::json!({ "message": format!("{message}{}", "x".repeat(pad)) })
    }

    #[test]
    fn chunks_respect_event_count_and_body_budget() {
        let small: Vec<serde_json::Value> = (0..2_500).map(|i| event("m", 0 + (i % 10))).collect();
        let chunks = DatadogAdapter::chunk_entries(&small);
        assert_eq!(chunks.len(), 3, "2500 events must split into 3 requests");
        assert!(chunks.iter().all(|c| c.len() <= MAX_EVENTS_PER_REQUEST));

        // ~1000 events of 6 KB each → ~6 MB total → at least 2 chunks.
        let big: Vec<serde_json::Value> = (0..1_000).map(|_| event("pad", 6_000)).collect();
        let chunks = DatadogAdapter::chunk_entries(&big);
        assert!(chunks.len() >= 2, "5 MB budget must split large bodies");
    }

    #[test]
    fn site_derives_endpoints() {
        let cfg = Config {
            backend: "datadog".into(),
            api_key: Some(Secret::new("k")),
            datadog_site: Some("datadoghq.eu".into()),
            datadog_metrics: true,
            ..Config::default()
        };
        let adapter = DatadogAdapter::from_config(&cfg).unwrap();
        assert_eq!(
            adapter.url,
            "https://http-intake.logs.datadoghq.eu/api/v2/logs"
        );
        assert_eq!(adapter.metrics_url.as_deref(), Some("https://api.datadoghq.eu/api/v2/series"));
    }

    #[test]
    fn custom_endpoint_overrides_site_and_derives_metrics_path() {
        let cfg = Config {
            backend: "datadog".into(),
            api_key: Some(Secret::new("k")),
            endpoint: Some("http://127.0.0.1:8126/api/v2/logs".into()),
            datadog_metrics: true,
            ..Config::default()
        };
        let adapter = DatadogAdapter::from_config(&cfg).unwrap();
        assert_eq!(adapter.url, "http://127.0.0.1:8126/api/v2/logs");
        assert_eq!(
            adapter.metrics_url.as_deref(),
            Some("http://127.0.0.1:8126/api/v2/series")
        );
    }

    #[test]
    fn metadata_becomes_top_level_attributes_not_ddtags() {
        let mut metadata = serde_json::Map::new();
        metadata.insert("user_id".into(), serde_json::json!(42));
        metadata.insert("region".into(), serde_json::json!("eu-1"));
        let e = LogEntry {
            timestamp: 1_700_000_000_000,
            level: LogLevel::Warn,
            log_type: LogType::Log,
            message: "hello".into(),
            metadata,
        };
        let event = to_dd_event(&e);
        assert!(event.get("ddtags").is_none(), "{event}");
        assert_eq!(event["user_id"], 42);
        assert_eq!(event["region"], "eu-1");
        assert_eq!(event["status"], "warning");
        assert_eq!(event["ddsource"], "tpt-microlog");
    }

    #[test]
    fn metric_entries_render_series_points() {
        let mut metadata = serde_json::Map::new();
        metadata.insert("value".into(), serde_json::json!(42.5));
        metadata.insert("tags".into(), serde_json::json!({"host": "server-01"}));
        let e = LogEntry {
            timestamp: 1_700_000_000_000,
            level: LogLevel::Info,
            log_type: LogType::Metric,
            message: "cpu.usage#1".into(),
            metadata,
        };
        let point = to_dd_series_point(&e).unwrap();
        assert_eq!(point["metric"], "cpu.usage_1");
        assert_eq!(point["points"][0]["value"], 42.5);
        assert_eq!(point["tags"][0], "host:server-01");
        // Non-metric entries are skipped.
        let mut log_entry = e.clone();
        log_entry.log_type = LogType::Log;
        assert!(to_dd_series_point(&log_entry).is_none());
    }
}
