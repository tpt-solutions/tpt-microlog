//! Splunk HTTP Event Collector (HEC) adapter.
//!
//! POSTs newline-delimited HEC events to
//! `<endpoint>/services/collector/event`; `api_key` is the HEC token, sent as
//! `Authorization: Splunk <token>`.

use tpt_log_core::{CompressedBatch, Config, LogEntry, Transport, TransportError};

use crate::http::{client_from_config, request_once};

/// Splunk HEC adapter.
pub struct SplunkAdapter {
    client: reqwest::blocking::Client,
    url: String,
    headers: Vec<(String, String)>,
}

impl std::fmt::Debug for SplunkAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SplunkAdapter")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// One HEC event: `{"time", "event", "sourcetype", "source", "host", "fields"}`.
fn to_hec_event(entry: &LogEntry) -> String {
    let time = format!("{:.3}", entry.timestamp as f64 / 1_000.0);
    let mut event = serde_json::Map::new();
    event.insert(
        "message".into(),
        serde_json::Value::String(entry.message.clone()),
    );
    event.insert(
        "severity".into(),
        serde_json::Value::String(entry.level.to_string().to_ascii_uppercase()),
    );
    for (k, v) in &entry.metadata {
        event.entry(k.clone()).or_insert_with(|| v.clone());
    }
    let mut out = serde_json::Map::new();
    out.insert("time".into(), serde_json::Value::from(time));
    out.insert("event".into(), serde_json::Value::Object(event));
    out.insert("sourcetype".into(), serde_json::Value::from("_json"));
    if let Some(service) = entry.metadata.get("service") {
        out.insert("source".into(), service.clone());
    }
    if let Some(host) = entry.metadata.get("host") {
        out.insert("host".into(), host.clone());
    }
    let fields = serde_json::Value::Object(entry.metadata.clone());
    out.insert("fields".into(), fields);
    serde_json::to_string(&serde_json::Value::Object(out)).unwrap_or_default()
}

impl SplunkAdapter {
    /// Build from core config. Requires `api_key` (the HEC token); `endpoint`
    /// defaults to `https://localhost:8088`.
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let token = config
            .api_key
            .as_ref()
            .map(|s| s.expose().to_string())
            .ok_or_else(|| {
                TransportError::Config("splunk backend needs `api_key` (the HEC token)".into())
            })?;
        let base = config
            .endpoint
            .clone()
            .unwrap_or_else(|| "https://localhost:8088".to_string());
        let url = format!("{}/services/collector/event", base.trim_end_matches('/'));
        let mut headers = vec![
            ("Authorization".to_string(), format!("Splunk {token}")),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
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
            client: client_from_config(config),
            url,
            headers,
        })
    }
}

impl Transport for SplunkAdapter {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        let entries = batch
            .decompress_entries()
            .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;
        // HEC accepts newline-delimited events in a single request.
        let mut body = String::with_capacity(entries.len() * 96);
        for entry in &entries {
            body.push_str(&to_hec_event(entry));
            body.push('\n');
        }
        request_once(
            &self.client,
            &self.url,
            &self.headers,
            "application/json",
            body.into_bytes(),
        )
    }

    fn name(&self) -> &str {
        "splunk"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_log_core::{LogLevel, Secret};

    #[test]
    fn hec_event_shape() {
        let mut e = LogEntry::new(LogLevel::Warn, "disk full");
        e.timestamp = 1_700_000_012_345;
        e.metadata.insert("user".into(), serde_json::json!("bob"));
        e.metadata
            .insert("service".into(), serde_json::json!("api"));
        e.metadata.insert("host".into(), serde_json::json!("box-1"));
        let line = to_hec_event(&e);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["time"], "1700000012.345");
        assert_eq!(v["event"]["message"], "disk full");
        assert_eq!(v["event"]["severity"], "WARN");
        assert_eq!(v["event"]["user"], "bob");
        assert_eq!(v["sourcetype"], "_json");
        assert_eq!(v["source"], "api");
        assert_eq!(v["host"], "box-1");
        assert_eq!(v["fields"]["user"], "bob");
    }

    #[test]
    fn requires_token() {
        let cfg = Config::default();
        assert!(SplunkAdapter::from_config(&cfg).is_err());
        let cfg = Config {
            api_key: Some(Secret::new("hec-token")),
            ..Config::default()
        };
        let adapter = SplunkAdapter::from_config(&cfg).unwrap();
        assert!(adapter.headers[0].1 == "Splunk hec-token");
    }
}
