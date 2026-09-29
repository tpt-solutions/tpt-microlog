//! Elasticsearch adapter — Bulk API (spec §5.3).
//!
//! POSTs newline-delimited JSON to `<endpoint>/_bulk`. Each entry becomes a
//! `{"index":{"_index":...}}` action line followed by a JSON document line.

use tpt_log_core::CompressedBatch;
use tpt_log_core::{Config, Transport, TransportError};

use crate::http::{pooled_client, request_once};

const DEFAULT_ENDPOINT: &str = "http://localhost:9200";
const DEFAULT_INDEX: &str = "tpt-microlog";

/// Elasticsearch Bulk API adapter.
pub struct ElasticsearchAdapter {
    client: reqwest::blocking::Client,
    url: String,
    headers: Vec<(String, String)>,
    index: String,
}

impl std::fmt::Debug for ElasticsearchAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElasticsearchAdapter")
            .field("url", &self.url)
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

/// Flatten one entry into a searchable document object.
fn to_doc(entry: &tpt_log_core::LogEntry) -> serde_json::Value {
    let mut doc = serde_json::Map::new();
    doc.insert(
        "@timestamp".into(),
        serde_json::Value::from(entry.timestamp),
    );
    doc.insert(
        "level".into(),
        serde_json::Value::String(entry.level.to_string().to_ascii_lowercase()),
    );
    doc.insert(
        "type".into(),
        serde_json::Value::String(entry.log_type.to_string()),
    );
    doc.insert(
        "message".into(),
        serde_json::Value::String(entry.message.clone()),
    );
    for (k, v) in &entry.metadata {
        if !doc.contains_key(k) {
            doc.insert(k.clone(), v.clone());
        }
    }
    serde_json::Value::Object(doc)
}

impl ElasticsearchAdapter {
    /// Build from core config. `endpoint` defaults to localhost:9200.
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let base = config
            .endpoint
            .clone()
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
        let url = format!("{}/_bulk", base.trim_end_matches('/'));
        let index = config
            .service
            .clone()
            .unwrap_or_else(|| DEFAULT_INDEX.to_string());
        let mut headers = Vec::new();
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
            index,
        })
    }
}

impl Transport for ElasticsearchAdapter {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        use std::fmt::Write as _;
        let entries = batch
            .decompress_entries()
            .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;

        let mut lines = String::with_capacity(entries.len() * 64);
        for entry in &entries {
            let action = serde_json::json!({
                "index": { "_index": self.index }
            });
            let _ = writeln!(
                lines,
                "{}",
                serde_json::to_string(&action).unwrap_or_default()
            );
            let doc = to_doc(entry);
            let _ = writeln!(lines, "{}", serde_json::to_string(&doc).unwrap_or_default());
        }
        request_once(
            &self.client,
            &self.url,
            &self.headers,
            "application/x-ndjson",
            lines.into_bytes(),
        )
    }

    fn name(&self) -> &str {
        "elasticsearch"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_log_core::{LogEntry, LogLevel, LogType};

    #[test]
    fn doc_includes_timestamp_level_message_and_metadata() {
        let mut metadata = serde_json::Map::new();
        metadata.insert("user".into(), serde_json::Value::String("x".into()));
        let e = LogEntry {
            timestamp: 5,
            level: LogLevel::Warn,
            log_type: LogType::Log,
            message: "disk full".into(),
            metadata,
        };
        let doc = to_doc(&e);
        assert_eq!(doc["@timestamp"], 5);
        assert_eq!(doc["level"], "warn");
        assert_eq!(doc["message"], "disk full");
        assert_eq!(doc["user"], "x");
    }
}
