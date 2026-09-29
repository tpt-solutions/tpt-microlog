//! Datadog adapter — Logs API v2 (spec §5.3).
//!
//! POSTs an array of Datadog log events to the intake API with the
//! `DD-API-KEY` header. Endpoint defaults to `https://http-intake.logs.datadoghq.com/api/v2/logs`
//! and can be overridden (useful for proxies and tests).

use tpt_log_core::CompressedBatch;
use tpt_log_core::{Config, LogEntry, LogLevel, Transport, TransportError};

use crate::http::{pooled_client, request_once};

const DEFAULT_ENDPOINT: &str = "https://http-intake.logs.datadoghq.com/api/v2/logs";

/// Datadog Logs API v2 adapter.
pub struct DatadogAdapter {
    client: reqwest::blocking::Client,
    url: String,
    headers: Vec<(String, String)>,
}

impl std::fmt::Debug for DatadogAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatadogAdapter")
            .field("url", &self.url)
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

/// Map one entry to a Datadog v2 log event object.
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

    // Render the leading metadata pairs as Datadog tags.
    let tag_pairs: Vec<String> = entry
        .metadata
        .iter()
        .filter(|(k, _)| {
            !matches!(
                k.as_str(),
                "host" | "service" | "source" | "pid" | "value" | "tags" | "level"
            )
        })
        .map(|(k, v)| format!("{k}:{v}"))
        .collect();
    if !tag_pairs.is_empty() {
        obj.insert(
            "ddtags".into(),
            serde_json::Value::String(tag_pairs.join(",")),
        );
    }

    // Fold any nested `tags` object in as well.
    if let Some(tags) = entry.metadata.get("tags").and_then(|v| v.as_object()) {
        for (k, v) in tags {
            obj.insert(k.clone(), v.clone());
        }
    }

    obj.insert(
        "type".into(),
        serde_json::Value::String(entry.log_type.to_string()),
    );
    serde_json::Value::Object(obj)
}

impl DatadogAdapter {
    /// Build from core config. Requires `api_key`; endpoint overridable.
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let key = config
            .api_key
            .clone()
            .ok_or_else(|| TransportError::Config("Datadog adapter needs an `api_key`".into()))?;
        let url = config
            .endpoint
            .clone()
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
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
            headers,
        })
    }
}

impl Transport for DatadogAdapter {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        let entries = batch
            .decompress_entries()
            .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;
        let payload: Vec<serde_json::Value> = entries.iter().map(to_dd_event).collect();
        let body = serde_json::to_vec(&payload)
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
        "datadog"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_log_core::{LogEntry, LogType};

    fn entry(message: &str) -> LogEntry {
        LogEntry {
            timestamp: 1_700,
            level: LogLevel::Error,
            log_type: LogType::Log,
            message: message.into(),
            metadata: {
                let mut m = serde_json::Map::new();
                m.insert("service".into(), serde_json::Value::String("api".into()));
                m.insert("host".into(), serde_json::Value::String("box-1".into()));
                m
            },
        }
    }

    #[test]
    fn format_event_shape() {
        let e = to_dd_event(&entry("boom"));
        assert_eq!(e["status"], "error");
        assert_eq!(e["ddsource"], "tpt-microlog");
        assert_eq!(e["service"], "api");
        assert_eq!(e["hostname"], "box-1");
        assert_eq!(e["message"], "boom");
    }
}
