//! OpenTelemetry Collector adapter over OTLP/HTTP with JSON encoding.
//!
//! POSTs OTLP `ExportLogsServiceRequest` JSON to `<endpoint>/v1/logs`
//! (endpoint defaults to `http://localhost:4318`). This complements the
//! gRPC transport (`backend = "grpc"`) for collectors that only expose the
//! HTTP receiver.

use tpt_log_core::{CompressedBatch, Config, LogEntry, LogLevel, Transport, TransportError};

use crate::http::{client_from_config, request_once};

/// OTLP/HTTP (JSON) adapter.
pub struct OtlpHttpAdapter {
    client: reqwest::blocking::Client,
    url: String,
    headers: Vec<(String, String)>,
}

impl std::fmt::Debug for OtlpHttpAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtlpHttpAdapter")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// Canonical OTLP severity numbers (trace 1, debug 5, info 9, warn 13,
/// error 17, fatal 21 — each band spans 4).
fn severity_number(level: LogLevel) -> u32 {
    match level {
        LogLevel::Trace => 1,
        LogLevel::Debug => 5,
        LogLevel::Info => 9,
        LogLevel::Warn => 13,
        LogLevel::Error => 17,
        LogLevel::Fatal => 21,
    }
}

fn attr(key: &str, value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value as V;
    let any_value = match value {
        V::String(s) => serde_json::json!({ "stringValue": s }),
        V::Bool(b) => serde_json::json!({ "boolValue": b }),
        V::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::json!({ "intValue": i.to_string() })
            } else {
                serde_json::json!({ "doubleValue": n.as_f64().unwrap_or(0.0) })
            }
        }
        other => serde_json::json!({ "stringValue": other.to_string() }),
    };
    serde_json::json!({ "key": key, "value": any_value })
}

/// Build the `ExportLogsServiceRequest` JSON body.
fn build_body(entries: &[LogEntry]) -> serde_json::Value {
    let records: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            let mut attributes: Vec<serde_json::Value> =
                e.metadata.iter().map(|(k, v)| attr(k, v)).collect();
            attributes.push(attr("log.type", &serde_json::json!(e.log_type.to_string())));
            serde_json::json!({
                "timeUnixNano": e.timestamp.to_string() + "000000",
                "severityNumber": severity_number(e.level),
                "severityText": e.level.to_string().to_ascii_uppercase(),
                "body": { "stringValue": e.message },
                "attributes": attributes,
            })
        })
        .collect();
    serde_json::json!({
        "resourceLogs": [{
            "resource": { "attributes": [
                attr("service.name", &serde_json::json!("tpt-microlog")),
            ]},
            "scopeLogs": [{ "logRecords": records }],
        }],
    })
}

impl OtlpHttpAdapter {
    /// Build from core config. `endpoint` defaults to
    /// `http://localhost:4318` (the collector's HTTP receiver port).
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let base = config
            .endpoint
            .clone()
            .unwrap_or_else(|| "http://localhost:4318".to_string());
        let url = format!("{}/v1/logs", base.trim_end_matches('/'));
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
            headers.push(("x-otlp-api-key".to_string(), key.expose().to_string()));
        }
        Ok(Self {
            client: client_from_config(config),
            url,
            headers,
        })
    }
}

impl Transport for OtlpHttpAdapter {
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
        "otlp-http"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_log_core::LogType;

    #[test]
    fn body_is_valid_otlp_json() {
        let mut e = LogEntry::new(LogLevel::Warn, "disk full").with("device", "/dev/sda1");
        e.timestamp = 1_700_000_000_123;
        e.log_type = LogType::Log;
        let body = build_body(&[e]);
        let record = &body["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0];
        assert_eq!(record["timeUnixNano"], "1700000000123000000");
        assert_eq!(record["severityNumber"], 13);
        assert_eq!(record["severityText"], "WARN");
        assert_eq!(record["body"]["stringValue"], "disk full");
        let attrs = record["attributes"].as_array().unwrap();
        assert!(attrs
            .iter()
            .any(|a| a["key"] == "device" && a["value"]["stringValue"] == "/dev/sda1"));
        assert!(attrs.iter().any(|a| a["key"] == "log.type"));
        assert_eq!(
            body["resourceLogs"][0]["resource"]["attributes"][0]["key"],
            "service.name"
        );
    }

    #[test]
    fn typed_attributes_map_to_otlp_any_value() {
        let v = attr("n", &serde_json::json!(42i64));
        assert_eq!(v["value"]["intValue"], "42");
        let v = attr("f", &serde_json::json!(1.5));
        assert_eq!(v["value"]["doubleValue"], 1.5);
        let v = attr("b", &serde_json::json!(true));
        assert_eq!(v["value"]["boolValue"], true);
    }
}
