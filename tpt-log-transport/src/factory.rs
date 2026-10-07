//! Config-to-transport factory (spec §2.7 auto-detection).
//!
//! Maps the `Config.backend` string to a concrete [`Transport`] implementation.
//! `none`/`""` produce a discarding transport, `console` prints to stdout, and
//! `fanout` routes to the nested `backends` list.

use std::fmt::Write as _;

use tpt_log_core::{BodyMode, Config, NullTransport, Transport, TransportError};

use crate::adapters::cloudwatch::CloudWatchAdapter;
use crate::adapters::datadog::DatadogAdapter;
use crate::adapters::elasticsearch::ElasticsearchAdapter;
use crate::adapters::loki::LokiAdapter;
use crate::adapters::otlp_http::OtlpHttpAdapter;
use crate::adapters::prometheus::PrometheusAdapter;
use crate::adapters::splunk::SplunkAdapter;
use crate::http::HttpTransport;
use crate::net::console::ConsoleTransport;
use crate::net::fanout::RoutingTransport;
use crate::net::file::FileTransport;
use crate::net::grpc::GrpcTransport;
use crate::net::tcp::TcpTransport;
use crate::net::udp::UdpTransport;

/// Backend names accepted in `Config.backend` (and in a `fanout` list).
pub const BACKENDS: &[&str] = &[
    "none",
    "console",
    "stdout",
    "fanout",
    "custom",
    "http",
    "datadog",
    "elasticsearch",
    "es",
    "cloudwatch",
    "prometheus",
    "grpc",
    "otlp",
    "udp",
    "tcp",
    "file",
    "loki",
    "otlp-http",
    "splunk",
];

/// Build the transport selected by `config.backend`.
pub fn build_transport(config: &Config) -> Result<Box<dyn Transport>, TransportError> {
    match config.backend.as_str() {
        "" | "none" => Ok(Box::new(NullTransport) as Box<dyn Transport>),
        "console" | "stdout" => Ok(Box::new(ConsoleTransport::from_config(config))),
        "fanout" => build_routing(config),
        "http" | "custom" => Ok(Box::new(HttpTransport::from_config(config)?)),
        "datadog" => Ok(Box::new(DatadogAdapter::from_config(config)?)),
        "elasticsearch" | "es" => Ok(Box::new(ElasticsearchAdapter::from_config(config)?)),
        "cloudwatch" => Ok(Box::new(CloudWatchAdapter::from_config(config)?)),
        "prometheus" => Ok(Box::new(PrometheusAdapter::from_config(config)?)),
        "loki" => Ok(Box::new(LokiAdapter::from_config(config)?)),
        "otlp-http" => Ok(Box::new(OtlpHttpAdapter::from_config(config)?)),
        "splunk" | "splunk-hec" => Ok(Box::new(SplunkAdapter::from_config(config)?)),
        "grpc" | "otlp" => Ok(Box::new(GrpcTransport::from_config(config)?)),
        "udp" => Ok(Box::new(UdpTransport::from_config(config)?)),
        "tcp" => Ok(Box::new(TcpTransport::from_config(config)?)),
        "file" => Ok(Box::new(FileTransport::from_config(config)?)),
        other => Err(TransportError::Config(format!(
            "unknown backend '{other}' (expected one of: {})",
            BACKENDS.join(", ")
        ))),
    }
}

/// Build the `fanout` routing transport from the nested `backends` list.
///
/// Each entry reuses the same config (endpoints, credentials, and headers are
/// shared), so a fan-out entry names a *backend*, not a destination.
fn build_routing(config: &Config) -> Result<Box<dyn Transport>, TransportError> {
    if config.backends.is_empty() {
        return Err(TransportError::Config(
            "fanout backend needs `backends = [\"datadog\", \"file\"]` — the list of \
             backend names to route to"
                .into(),
        ));
    }
    if config.backends.iter().any(|b| b == "fanout") {
        return Err(TransportError::Config(
            "fanout cannot nest another fanout backend".into(),
        ));
    }
    let mut transports: Vec<Box<dyn Transport>> = Vec::with_capacity(config.backends.len());
    for name in &config.backends {
        let mut nested = config.clone();
        nested.backend = name.clone();
        transports.push(build_transport(&nested)?);
    }
    Ok(Box::new(RoutingTransport::new(transports, config.failover)))
}

impl HttpTransport {
    /// Build from a core [`Config`], honoring `endpoint` and `extra_headers`
    /// (`name=value` pairs). Requires `backend = "http"`.
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let url = config
            .endpoint
            .clone()
            .ok_or_else(|| TransportError::Config("HTTP backend needs an `endpoint`".into()))?;

        let mut headers = Vec::new();
        for raw in &config.extra_headers {
            match raw.split_once('=') {
                Some((name, value)) => {
                    let (name, value) = (name.trim(), value.trim());
                    if name.is_empty() {
                        return Err(TransportError::Config(format!(
                            "malformed extra_header '{raw}' (empty name)"
                        )));
                    }
                    headers.push((name.to_string(), value.to_string()));
                }
                None => {
                    return Err(TransportError::Config(format!(
                        "malformed extra_header '{raw}' (expected name=value)"
                    )));
                }
            }
        }

        // Plain JSON is the default so any receiver can consume it; zstd
        // bodies are opt-in (`body_mode = "zstd"`). `fast_mode` implies JSON
        // (its payloads are already uncompressed).
        let mode = match (config.body_mode, config.fast_mode) {
            (_, true) | (BodyMode::Json, false) => BodyMode::Json,
            (BodyMode::Zstd, false) => BodyMode::Zstd,
        };
        Ok(Self::with_mode(url, headers, mode))
    }
}

/// Render an endpoint URL as a friendly string (used by tests/diagnostics).
pub fn describe(config: &Config) -> String {
    let mut s = String::new();
    let _ = write!(s, "backend={}", config.backend);
    if let Some(url) = &config.endpoint {
        let _ = write!(s, ", endpoint={url}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_log_core::batch::CompressedBatchPayload;
    use tpt_log_core::CompressedBatch;

    fn batch() -> CompressedBatch {
        CompressedBatch {
            payload: CompressedBatchPayload {
                bytes: b"{}".to_vec(),
                entry_count: 0,
                compressed: false,
                raw_json: None,
            },
        }
    }

    /// `unwrap_err` needs `Debug` on the success type; transports are not.
    fn expect_config_error(result: Result<Box<dyn Transport>, TransportError>) -> String {
        match result {
            Ok(_) => panic!("expected a configuration error"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn none_backend_discards() {
        let cfg = Config {
            backend: "none".into(),
            ..Config::default()
        };
        let t = build_transport(&cfg).unwrap();
        assert_eq!(t.name(), "none");
        t.send(&batch()).unwrap();
    }

    #[test]
    fn http_backend_needs_endpoint() {
        let cfg = Config {
            backend: "http".into(),
            ..Config::default()
        };
        assert!(matches!(
            build_transport(&cfg),
            Err(TransportError::Config(_))
        ));
    }

    #[test]
    fn http_backend_builds_pooled_client() {
        let cfg = Config {
            backend: "http".into(),
            endpoint: Some("https://example.invalid/v1/logs".into()),
            fast_mode: true,
            extra_headers: vec!["Authorization=Bearer t".into(), "X-Id=7".into()],
            ..Config::default()
        };
        let t = build_transport(&cfg).unwrap();
        assert_eq!(t.name(), "http");
    }

    #[test]
    fn unknown_backend_is_config_error() {
        let cfg = Config {
            backend: "carrier-pigeon".into(),
            ..Config::default()
        };
        let msg = expect_config_error(build_transport(&cfg));
        assert!(msg.contains("carrier-pigeon"), "{msg}");
        assert!(
            msg.contains("datadog"),
            "error should list valid backends: {msg}"
        );
    }

    #[test]
    fn console_is_the_zero_config_default() {
        let cfg = Config::default();
        assert_eq!(cfg.backend, "console");
        let t = build_transport(&cfg).unwrap();
        assert_eq!(t.name(), "console");
        assert!(BACKENDS.contains(&"console"));
    }

    #[test]
    fn loki_otlp_and_splunk_are_reachable() {
        for (backend, expected) in [
            ("loki", "loki"),
            ("otlp-http", "otlp-http"),
            ("splunk", "splunk"),
        ] {
            let cfg = Config {
                backend: backend.into(),
                api_key: Some(tpt_log_core::Secret::new("tok")),
                ..Config::default()
            };
            let t = build_transport(&cfg).unwrap();
            assert_eq!(t.name(), expected, "backend {backend}");
        }
    }

    #[test]
    fn fanout_routes_to_every_listed_backend() {
        let cfg = Config {
            backend: "fanout".into(),
            backends: vec!["none".into(), "file".into()],
            file_path: Some(
                std::env::temp_dir()
                    .join("fanout-test.jsonl")
                    .display()
                    .to_string(),
            ),
            ..Config::default()
        };
        let t = build_transport(&cfg).unwrap();
        assert_eq!(t.name(), "fanout");
    }

    #[test]
    fn fanout_failover_is_named() {
        let cfg = Config {
            backend: "fanout".into(),
            backends: vec!["none".into(), "none".into()],
            failover: true,
            ..Config::default()
        };
        assert_eq!(build_transport(&cfg).unwrap().name(), "fanout-failover");
    }

    #[test]
    fn fanout_needs_a_backend_list() {
        let cfg = Config {
            backend: "fanout".into(),
            ..Config::default()
        };
        let msg = expect_config_error(build_transport(&cfg));
        assert!(msg.contains("backends"), "{msg}");
    }

    #[test]
    fn fanout_rejects_nesting_and_unknown_members() {
        let nested = Config {
            backend: "fanout".into(),
            backends: vec!["fanout".into()],
            ..Config::default()
        };
        assert!(matches!(
            build_transport(&nested),
            Err(TransportError::Config(_))
        ));
        let bogus = Config {
            backend: "fanout".into(),
            backends: vec!["carrier-pigeon".into()],
            ..Config::default()
        };
        assert!(matches!(
            build_transport(&bogus),
            Err(TransportError::Config(_))
        ));
    }

    #[test]
    fn malformed_header_is_config_error() {
        let cfg = Config {
            backend: "http".into(),
            endpoint: Some("https://example.invalid".into()),
            extra_headers: vec!["no-equals-sign".into()],
            ..Config::default()
        };
        assert!(matches!(
            build_transport(&cfg),
            Err(TransportError::Config(_))
        ));
    }
}
