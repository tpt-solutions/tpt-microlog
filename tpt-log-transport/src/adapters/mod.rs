//! Backend-specific HTTP adapters (spec §5.3).
//!
//! Each adapter formats a decoded batch into the backend's wire protocol and
//! posts it through a pooled blocking client. Classification (transient vs
//! permanent) is centralized in [`crate::http::request_once`]; retry and WAL
//! fallback are owned by the core delivery engine.

pub mod cloudwatch;
pub mod datadog;
pub mod elasticsearch;
pub mod loki;
pub mod otlp_http;
pub mod prometheus;
pub mod sigv4;
pub mod splunk;

/// Shared formatting helper: build the `{"...": value}` object for a metric
/// entry's numeric `metadata["value"]`.
pub(crate) fn value_as_number(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}
