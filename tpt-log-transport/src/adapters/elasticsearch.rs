//! Elasticsearch adapter — Bulk API (spec §5.3).
//!
//! POSTs newline-delimited JSON to `<endpoint>/_bulk`. Each entry becomes a
//! `{"index":{"_index":...}}` action line followed by a JSON document line.
//!
//! - the index defaults to `tpt-microlog` (overridable via `es_index`) and is
//!   lowercased + sanitized, never derived from the machine hostname;
//! - bulk responses with `errors: true` are inspected per item: item-level
//!   429 surfaces as a transient error (the core retries), anything else as
//!   permanent;
//! - auth: `api_key` → `Authorization: ApiKey <key>` (base64-encoded key as
//!   issued by Elasticsearch), `es_basic_auth` (`"user:pass"`) → Basic.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;

use tpt_log_core::{CompressedBatch, Config, LogEntry, Transport, TransportError};

use crate::http::{client_from_config, request_once_full};

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

/// Elasticsearch index names must be lowercase and free of
/// `\ / * ? " < > | , #` and leading `-`/`_`.
fn sanitize_index(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else if matches!(
                c,
                '\\' | '/' | '*' | '?' | '"' | '<' | '>' | '|' | ',' | '#' | ' '
            ) {
                '_'
            } else {
                c
            }
        })
        .collect();
    while out.starts_with('-') || out.starts_with('_') {
        out.remove(0);
    }
    if out.is_empty() {
        DEFAULT_INDEX.to_string()
    } else {
        out
    }
}

/// Flatten one entry into a searchable document object.
fn to_doc(entry: &LogEntry) -> serde_json::Value {
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

/// Inspect a bulk response body. `Ok(None)` = all items indexed; `Some(err)`
/// = at least one item failed, classified by the item's HTTP-ish status.
fn bulk_item_error(body: &[u8]) -> Option<TransportError> {
    let parsed: serde_json::Value = serde_json::from_slice(body).ok()?;
    let errors = parsed.get("errors")?.as_bool()?;
    if !errors {
        return None;
    }
    let items = parsed.get("items")?.as_array()?;
    for item in items {
        // Shape: {"index": {"status": 429, "error": {...}}}
        let Some(index_result) = item.get("index") else {
            continue;
        };
        let status = index_result.get("status").and_then(|s| s.as_u64());
        let reason = index_result
            .pointer("/error/reason")
            .and_then(|r| r.as_str())
            .unwrap_or("unknown bulk item error");
        match status {
            Some(429) => {
                return Some(TransportError::RateLimited {
                    message: format!("elasticsearch bulk item: {reason}"),
                    retry_after_ms: None,
                });
            }
            Some(s) if (500..600).contains(&s) => {
                return Some(TransportError::Transient(format!(
                    "elasticsearch bulk item (HTTP {s}): {reason}"
                )));
            }
            Some(s) if (400..500).contains(&s) => {
                return Some(TransportError::Permanent(format!(
                    "elasticsearch bulk item (HTTP {s}): {reason}"
                )));
            }
            _ => {}
        }
    }
    Some(TransportError::Permanent(
        "elasticsearch bulk reported errors without item detail".into(),
    ))
}

impl ElasticsearchAdapter {
    /// Build from core config. `endpoint` defaults to localhost:9200.
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let base = config
            .endpoint
            .clone()
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
        let url = format!("{}/_bulk", base.trim_end_matches('/'));
        let index = sanitize_index(config.es_index.as_deref().unwrap_or(DEFAULT_INDEX));
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
        if let Some(key) = &config.api_key {
            headers.push((
                "Authorization".to_string(),
                format!("ApiKey {}", key.expose()),
            ));
        }
        if let Some(auth) = &config.es_basic_auth {
            headers.push((
                "Authorization".to_string(),
                format!("Basic {}", B64.encode(auth.expose().as_bytes())),
            ));
        }
        Ok(Self {
            client: client_from_config(config),
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
        // `request_once_full` only returns for 2xx; the body may still report
        // per-item bulk errors (HTTP 200 with `errors: true`). Item-level 429
        // is classified transient, so the core retries it.
        let response = request_once_full(
            &self.client,
            &self.url,
            &self.headers,
            "application/x-ndjson",
            lines.into_bytes(),
        )?;
        match bulk_item_error(&response.body) {
            None => Ok(()),
            Some(err) => Err(err),
        }
    }

    fn name(&self) -> &str {
        "elasticsearch"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_log_core::{LogLevel, LogType};

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

    #[test]
    fn index_defaults_to_constant_and_sanitizes() {
        assert_eq!(sanitize_index(DEFAULT_INDEX), "tpt-microlog");
        assert_eq!(sanitize_index("My-App.Logs"), "my-app.logs");
        assert_eq!(sanitize_index("weird/index name"), "weird_index_name");
        assert_eq!(sanitize_index("-_leading"), "leading");
        assert_eq!(sanitize_index(""), "tpt-microlog");
    }

    #[test]
    fn auth_headers_from_config() {
        let cfg = Config {
            backend: "elasticsearch".into(),
            api_key: Some(tpt_log_core::Secret::new("a2V5OnZhbHVl")),
            es_basic_auth: Some(tpt_log_core::Secret::new("user:pass")),
            ..Config::default()
        };
        let adapter = ElasticsearchAdapter::from_config(&cfg).unwrap();
        // api_key wins the Authorization header; basic auth is appended after
        // (last header with the same name is used by reqwest).
        let auths: Vec<&(String, String)> = adapter
            .headers
            .iter()
            .filter(|(k, _)| k == "Authorization")
            .collect();
        assert_eq!(auths.len(), 2, "{auths:?}");
        assert_eq!(auths[0].1, "ApiKey a2V5OnZhbHVl");
        assert_eq!(auths[1].1, format!("Basic {}", B64.encode(b"user:pass")));
    }

    #[test]
    fn bulk_error_response_is_classified() {
        let ok = br#"{"errors":false,"items":[]}"#;
        assert!(bulk_item_error(ok).is_none());

        let rate_limited = br#"{"errors":true,"items":[{"index":{"status":429,"error":{"type":"es_rejected_execution_exception","reason":"too many requests"}}}]}"#;
        assert!(matches!(
            bulk_item_error(rate_limited),
            Some(TransportError::RateLimited { .. })
        ));

        let permanent = br#"{"errors":true,"items":[{"index":{"status":400,"error":{"type":"mapper_parsing_exception","reason":"bad mapping"}}}]}"#;
        assert!(matches!(
            bulk_item_error(permanent),
            Some(TransportError::Permanent(_))
        ));

        let server = br#"{"errors":true,"items":[{"index":{"status":503,"error":{"type":"process_cluster_event_timeout_exception","reason":"timeout"}}}]}"#;
        assert!(matches!(
            bulk_item_error(server),
            Some(TransportError::Transient(_))
        ));
    }
}
