//! Fluent construction of a ready-to-use [`MicroLog`].
//!
//! ```no_run
//! use tpt_log_transport::Builder;
//!
//! let log = Builder::new()
//!     .backend("datadog")
//!     .api_key("dd-key")
//!     .service("checkout")
//!     .redact_keys(["password", "token"])
//!     .build()
//!     .unwrap();
//! log.info("up");
//! ```
//!
//! [`Builder::new`] starts from [`Config::load`] (config file + `TPT_LOG_*`
//! environment), so anything not set explicitly keeps its configured value.
//! Use [`Builder::from_config`] to start from a fixed config instead.

use std::sync::Arc;
use std::time::Duration;

use tpt_log_core::{Config, LogLevel, MicroLog, Secret, Transport, TransportError};

use crate::build_transport;

/// Fluent builder for [`MicroLog`].
pub struct Builder {
    config: Config,
    transport: Option<Arc<dyn Transport>>,
    preset_error: Option<String>,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    /// Start from the loaded configuration (file + environment overrides).
    pub fn new() -> Self {
        Self::from_config(Config::load())
    }

    /// Start from an explicit configuration.
    pub fn from_config(config: Config) -> Self {
        Self {
            config,
            transport: None,
            preset_error: None,
        }
    }

    /// Apply a named preset (`dev`, `prod`, `grafana-loki`, ...). An unknown
    /// name surfaces as an error from [`build`](Self::build).
    pub fn preset(mut self, name: &str) -> Self {
        if let Err(e) = self.config.apply_preset(name) {
            self.preset_error = Some(e);
        }
        self
    }

    /// Delivery backend (see [`crate::BACKENDS`]).
    pub fn backend(mut self, backend: impl Into<String>) -> Self {
        self.config.backend = backend.into();
        self
    }

    /// Logical service name.
    pub fn service(mut self, service: impl Into<String>) -> Self {
        self.config.service = Some(service.into());
        self
    }

    /// Base URL / address for the backend.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.config.endpoint = Some(endpoint.into());
        self
    }

    /// Backend credential (never printed by `Debug`).
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.config.api_key = Some(Secret::new(key));
        self
    }

    /// Drop entries below `level`.
    pub fn min_level(mut self, level: LogLevel) -> Self {
        self.config.min_level = Some(level.to_string());
        self
    }

    /// Entries per batch.
    pub fn batch_size(mut self, n: usize) -> Self {
        self.config.batch_size = n.max(1);
        self
    }

    /// Flush at least this often.
    pub fn flush_interval(mut self, interval: Duration) -> Self {
        self.config.flush_interval_ms = (interval.as_millis() as u64).max(1);
        self
    }

    /// Enable or disable the on-disk write-ahead log.
    pub fn wal(mut self, enabled: bool) -> Self {
        self.config.wal_enabled = enabled;
        self
    }

    /// Mask these metadata keys (case-insensitive) before anything is stored
    /// or sent.
    pub fn redact_keys<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.config.redact_keys = keys.into_iter().map(Into::into).collect();
        self
    }

    /// Enable the built-in PII detectors (email, card, SSN, JWT, ...).
    pub fn redact_builtin(mut self, enabled: bool) -> Self {
        self.config.redact_builtin = enabled;
        self
    }

    /// Escape hatch: edit any other [`Config`] field.
    pub fn configure(mut self, f: impl FnOnce(&mut Config)) -> Self {
        f(&mut self.config);
        self
    }

    /// Deliver through a custom transport instead of the configured backend.
    pub fn transport(mut self, transport: Arc<dyn Transport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// The configuration as it will be used.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Build the logger.
    pub fn build(self) -> Result<MicroLog, TransportError> {
        if let Some(e) = self.preset_error {
            return Err(TransportError::Config(e));
        }
        let transport = match self.transport {
            Some(t) => t,
            None => Arc::from(build_transport(&self.config)?),
        };
        Ok(MicroLog::new(self.config, transport))
    }

    /// Build the logger behind an `Arc`, the form the `tracing` layer, `log`
    /// backend and panic hook take.
    pub fn build_arc(self) -> Result<Arc<MicroLog>, TransportError> {
        self.build().map(Arc::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet() -> Builder {
        Builder::from_config(Config::default())
            .wal(false)
            .batch_size(1)
            .flush_interval(Duration::from_millis(20))
    }

    #[test]
    fn builds_and_delivers_through_the_file_backend() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.jsonl");
        let log = quiet()
            .backend("file")
            .configure(|c| c.file_path = Some(path.to_string_lossy().into_owned()))
            .service("svc")
            .redact_keys(["token"])
            .build()
            .unwrap();
        tpt_log_core::mlog!(log, Info, "hello", token = "s3cret", n = 1);
        assert!(log.flush());
        log.shutdown();

        let line = std::fs::read_to_string(&path).unwrap();
        let entry: serde_json::Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
        assert_eq!(entry["message"], "hello");
        assert_eq!(entry["metadata"]["token"], "[REDACTED]");
        assert_eq!(entry["metadata"]["n"], 1);
    }

    #[test]
    fn custom_transport_and_arc() {
        let spy = Arc::new(tpt_log_core::transport::SpyTransport::default());
        struct Fwd(Arc<tpt_log_core::transport::SpyTransport>);
        impl Transport for Fwd {
            fn send(&self, b: &tpt_log_core::CompressedBatch) -> Result<(), TransportError> {
                self.0.send(b)
            }
            fn name(&self) -> &str {
                "fwd"
            }
        }
        let log = quiet()
            .transport(Arc::new(Fwd(spy.clone())))
            .build_arc()
            .unwrap();
        log.warn("via custom transport");
        assert!(log.flush());
        assert!(spy
            .entries()
            .iter()
            .any(|e| e.message == "via custom transport"));
    }

    #[test]
    fn unknown_preset_and_backend_are_errors() {
        assert!(matches!(
            quiet().preset("nope").build(),
            Err(TransportError::Config(m)) if m.contains("available")
        ));
        assert!(quiet().backend("carrier-pigeon").build().is_err());
    }
}
