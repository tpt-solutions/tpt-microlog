//! Config-to-transport factory (spec §2.7 auto-detection).
//!
//! Maps the `Config.backend` string to a concrete [`Transport`] implementation.
//! `none`/`""` produce a discarding transport; backends belonging to later
//! phases return a clean configuration error until they land.

use std::fmt::Write as _;

use tpt_log_core::{Config, NullTransport, Transport, TransportError};

use crate::adapters::cloudwatch::CloudWatchAdapter;
use crate::adapters::datadog::DatadogAdapter;
use crate::adapters::elasticsearch::ElasticsearchAdapter;
use crate::adapters::prometheus::PrometheusAdapter;
use crate::http::{BodyMode, HttpTransport};
use crate::net::file::FileTransport;
use crate::net::grpc::GrpcTransport;
use crate::net::tcp::TcpTransport;
use crate::net::udp::UdpTransport;

/// Build the transport selected by `config.backend`.
pub fn build_transport(config: &Config) -> Result<Box<dyn Transport>, TransportError> {
    match config.backend.as_str() {
        "" | "none" => Ok(Box::new(NullTransport) as Box<dyn Transport>),
        "http" | "custom" => Ok(Box::new(HttpTransport::from_config(config)?)),
        "datadog" => Ok(Box::new(DatadogAdapter::from_config(config)?)),
        "elasticsearch" | "es" => Ok(Box::new(ElasticsearchAdapter::from_config(config)?)),
        "cloudwatch" => Ok(Box::new(CloudWatchAdapter::from_config(config)?)),
        "prometheus" => Ok(Box::new(PrometheusAdapter::from_config(config)?)),
        "grpc" | "otlp" => Ok(Box::new(GrpcTransport::from_config(config)?)),
        "udp" => Ok(Box::new(UdpTransport::from_config(config)?)),
        "tcp" => Ok(Box::new(TcpTransport::from_config(config)?)),
        "file" => Ok(Box::new(FileTransport::from_config(config)?)),
        other => Err(TransportError::Config(format!(
            "backend '{other}' is not implemented yet (see env TPT_LOG_BACKEND)"
        ))),
    }
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

        let mode = if config.fast_mode {
            // Fast mode yields uncompressed payloads; send raw JSON so the
            // receiver can parse it directly.
            BodyMode::Json
        } else {
            // Default: stream the compressed bytes with `Content-Encoding: zstd`.
            BodyMode::Zstd
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
            },
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
        assert!(matches!(
            build_transport(&cfg),
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
