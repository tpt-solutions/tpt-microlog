use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::defaults;
use crate::LogLevel;

/// Backend selection string constants.
pub mod backend {
    /// No-op transport (in-memory only).
    pub const NONE: &str = "none";
    /// Generic JSON POST to a configurable endpoint.
    pub const CUSTOM_HTTP: &str = "custom";
    /// Datadog Logs API v2.
    pub const DATADOG: &str = "datadog";
    /// Elasticsearch Bulk API.
    pub const ELASTICSEARCH: &str = "elasticsearch";
    /// AWS CloudWatch Logs PutLogEvents (SigV4).
    pub const CLOUDWATCH: &str = "cloudwatch";
    /// Embedded Prometheus `/metrics` pull endpoint.
    pub const PROMETHEUS: &str = "prometheus";
    /// gRPC (OpenTelemetry Collector compatible).
    pub const GRPC: &str = "grpc";
    /// Fire-and-forget UDP.
    pub const UDP: &str = "udp";
    /// Ordered TCP stream.
    pub const TCP: &str = "tcp";
    /// Local JSONL file.
    pub const FILE: &str = "file";
}

/// Parsed WAL directory root (from file/env overrides).
///
/// Uses the user data dir (Windows: `%APPDATA%`, Unix: `$XDG_DATA_HOME` or
/// `~/.local/share`) with a temp-dir fallback. Implemented inline so the core
/// stays dependency-light and license-clean.
fn default_wal_dir() -> PathBuf {
    if let Some(base) = std::env::var_os("APPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_DATA_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| {
            home_dir_var().map(|mut p| {
                p.push(".local");
                p.push("share");
                p
            })
        })
    {
        return base.join("tpt-microlog").join("wal");
    }
    std::env::temp_dir().join("tpt-microlog").join("wal")
}

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

/// `tpt-microlog` configuration (spec §5.1-5.4).
///
/// Loaded from `tpt-log.toml` (optional) then overridden by `TPT_LOG_*`
/// environment variables. All fields have sane defaults so the library is
/// zero-configuration out of the box.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Logical service name, attached to every entry when `enrichment` is on.
    pub service: Option<String>,
    /// Backend name — see [`backend`].
    pub backend: String,
    /// Base URL / address for the chosen backend.
    pub endpoint: Option<String>,
    /// Secret token (Datadog `DD-API-KEY`, etc.).
    pub api_key: Option<String>,
    /// Extra HTTP headers as `name=value` entries.
    pub extra_headers: Vec<String>,
    /// gRPC channel target in `host:port` form.
    pub grpc_endpoint: Option<String>,
    /// UDP/TCP target in `host:port` form.
    pub net_endpoint: Option<String>,
    /// Local JSONL file for the `file` backend.
    pub file_path: Option<String>,
    /// Rotate the file backend above this many bytes.
    pub file_rotation_bytes: Option<u64>,
    /// Rotate the file backend after this many seconds.
    pub file_rotation_seconds: Option<u64>,
    /// Number of rotating files to keep for the `file` backend.
    pub file_rotations: usize,
    /// Prometheus scrape bind address (`host:port`).
    pub prometheus_bind: String,
    /// CloudWatch Logs configuration.
    pub aws_region: Option<String>,
    pub aws_access_key: Option<String>,
    pub aws_secret_key: Option<String>,
    pub aws_log_group: Option<String>,
    pub aws_log_stream: Option<String>,

    /// Flush batch at this many entries.
    pub batch_size: usize,
    /// Flush interval in milliseconds.
    pub flush_interval_ms: u64,
    /// Flush once accumulated bytes exceed this.
    pub memory_limit_bytes: usize,
    /// Ingestion queue capacity.
    pub queue_capacity: usize,
    /// zstd compression level.
    pub compression_level: i32,
    /// Disable compression for extreme throughput ("fast mode", spec §9).
    pub fast_mode: bool,

    /// Maximum delivery attempts (exponential backoff).
    pub max_retries: u32,
    /// Initial backoff delay (ms).
    pub retry_base_ms: u64,
    /// Backoff ceiling (ms).
    pub retry_max_backoff_ms: u64,

    /// Enable write-ahead log persistence.
    pub wal_enabled: bool,
    /// WAL directory.
    pub wal_dir: String,
    /// WAL size cap in bytes (FIFO drop oldest).
    pub wal_max_size_bytes: u64,
    /// WAL retry poll interval (ms).
    pub wal_retry_interval_ms: u64,

    /// Attach hostname / PID / service to every entry (spec §8).
    pub enrichment: bool,
    /// Drop entries below this level.
    pub min_level: Option<String>,
    /// Percent of logs to deliver (0.0 - 1.0, spec §8).
    pub sample_rate: f64,
    /// Drop entries carrying any of these `tag=value` pairs (spec §8).
    pub filter_tags: Vec<String>,
    /// Drop entries whose message matches this regex (spec §8).
    pub filter_regex: Option<String>,

    /// Whether newline-delimited base64 framing is used for the WAL.
    pub wal_frame_newline: bool,
}

impl Default for Config {
    fn default() -> Self {
        let hostname = hostname::get().ok().and_then(|h| h.into_string().ok());
        Self {
            service: hostname,
            backend: String::from(backend::NONE),
            endpoint: None,
            api_key: None,
            extra_headers: Vec::new(),
            grpc_endpoint: None,
            net_endpoint: None,
            file_path: None,
            file_rotation_bytes: None,
            file_rotation_seconds: None,
            file_rotations: 5,
            prometheus_bind: "127.0.0.1:9090".to_string(),
            aws_region: None,
            aws_access_key: None,
            aws_secret_key: None,
            aws_log_group: None,
            aws_log_stream: None,
            batch_size: defaults::BATCH_SIZE,
            flush_interval_ms: defaults::FLUSH_INTERVAL_MS,
            memory_limit_bytes: defaults::MEMORY_LIMIT_BYTES,
            queue_capacity: defaults::QUEUE_CAPACITY,
            compression_level: defaults::COMPRESSION_LEVEL,
            fast_mode: false,
            max_retries: defaults::MAX_RETRIES,
            retry_base_ms: defaults::RETRY_BASE_MS,
            retry_max_backoff_ms: defaults::RETRY_MAX_BACKOFF_MS,
            wal_enabled: true,
            wal_dir: default_wal_dir().to_string_lossy().to_string(),
            wal_max_size_bytes: defaults::WAL_MAX_SIZE_BYTES,
            wal_retry_interval_ms: defaults::WAL_RETRY_INTERVAL_MS,
            enrichment: false,
            min_level: None,
            sample_rate: 1.0,
            filter_tags: Vec::new(),
            filter_regex: None,
            wal_frame_newline: true,
        }
    }
}

impl Config {
    /// Load configuration from `tpt-log.toml` in the current directory if
    /// present, then overlay `TPT_LOG_*` environment variables.
    pub fn load() -> Self {
        let mut cfg = match std::fs::read_to_string("tpt-log.toml") {
            Ok(raw) => Self::from_toml_str(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        };
        cfg.apply_env();
        cfg.normalize();
        cfg
    }

    /// Parse from a raw TOML string.
    pub fn from_toml_str(raw: &str) -> Result<Self, toml::de::Error> {
        let mut cfg = toml::from_str::<Config>(raw)?;
        cfg.normalize();
        Ok(cfg)
    }

    /// Load from an explicit file path.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, std::io::Error> {
        let raw = std::fs::read_to_string(path)?;
        let cfg = Self::from_toml_str(&raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(cfg)
    }

    /// Overlay `TPT_LOG_*` environment variables over the current config.
    pub fn apply_env(&mut self) {
        let env = |key: &str| env_opt(key);
        if let Some(v) = env("TPT_LOG_BACKEND") {
            self.backend = v;
        }
        if let Some(v) = env("TPT_LOG_ENDPOINT") {
            self.endpoint = Some(v);
        }
        if let Some(v) = env("TPT_LOG_API_KEY") {
            self.api_key = Some(v);
        }
        if let Some(v) = env("TPT_LOG_SERVICE") {
            self.service = Some(v);
        }
        if let Some(v) = env("TPT_LOG_BATCH_SIZE").and_then(|s| s.parse().ok()) {
            self.batch_size = v;
        }
        if let Some(v) = env("TPT_LOG_FLUSH_INTERVAL_MS").and_then(|s| s.parse().ok()) {
            self.flush_interval_ms = v;
        }
        if let Some(v) = env("TPT_LOG_MEMORY_LIMIT_BYTES").and_then(|s| s.parse().ok()) {
            self.memory_limit_bytes = v;
        }
        if let Some(v) = env("TPT_LOG_COMPRESSION_LEVEL").and_then(|s| s.parse().ok()) {
            self.compression_level = v;
        }
        if let Some(v) = env("TPT_LOG_MAX_RETRIES").and_then(|s| s.parse().ok()) {
            self.max_retries = v;
        }
        if let Some(v) = env("TPT_LOG_RETRY_BASE_MS").and_then(|s| s.parse().ok()) {
            self.retry_base_ms = v;
        }
        if let Some(v) = env("TPT_LOG_RETRY_MAX_BACKOFF_MS").and_then(|s| s.parse().ok()) {
            self.retry_max_backoff_ms = v;
        }
        if let Some(v) = env("TPT_LOG_WAL_ENABLED") {
            self.wal_enabled = matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = env("TPT_LOG_WAL_DIR") {
            self.wal_dir = v;
        }
        if let Some(v) = env("TPT_LOG_WAL_MAX_SIZE_BYTES").and_then(|s| s.parse().ok()) {
            self.wal_max_size_bytes = v;
        }
        if let Some(v) = env("TPT_LOG_ENRICHMENT") {
            self.enrichment = matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = env("TPT_LOG_MIN_LEVEL") {
            self.min_level = Some(v);
        }
        if let Some(v) = env("TPT_LOG_SAMPLE_RATE").and_then(|s| s.parse().ok()) {
            self.sample_rate = v;
        }
        if let Some(v) = env("TPT_LOG_FILTER_TAGS") {
            self.filter_tags = v
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
        if let Some(v) = env("TPT_LOG_FILTER_REGEX") {
            self.filter_regex = Some(v);
        }
        if let Some(v) = env("TPT_LOG_FAST_MODE") {
            self.fast_mode = matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
        }
    }

    /// A `(hostname, pid)` pair used by the enrichment layer.
    pub fn host_and_process(&self) -> (String, String) {
        let host = self
            .service
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let pid = std::process::id().to_string();
        (host, pid)
    }

    fn normalize(&mut self) {
        if let Some(l) = &self.min_level {
            if l.parse::<LogLevel>().is_err() {
                self.min_level = None;
            }
        }
        if self.wal_dir.contains('~') {
            self.wal_dir = self.wal_dir.replace('~', &home_dir());
        }
        self.sample_rate = self.sample_rate.clamp(0.0, 1.0);
    }

    /// Resolved WAL directory with any `~` expanded.
    pub fn resolved_wal_dir(&self) -> PathBuf {
        PathBuf::from(self.wal_dir.replace('~', &home_dir()))
    }

    /// Map a `backend` string to an HTTP base URL when not explicitly supplied.
    pub fn inferred_endpoint(&self) -> Option<String> {
        match self.backend.as_str() {
            crate::config::backend::DATADOG => {
                Some("https://http-intake.logs.datadoghq.com/api/v2/logs".to_string())
            }
            crate::config::backend::ELASTICSEARCH => Some("http://localhost:9200".to_string()),
            crate::config::backend::CLOUDWATCH => None,
            other => self.endpoint.clone().or_else(|| {
                if other == backend::CUSTOM_HTTP {
                    self.endpoint.clone()
                } else {
                    None
                }
            }),
        }
    }

    /// Convenience builder for tests and wrappers.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Best-effort home directory lookup (no external dependency).
fn home_dir_var() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> String {
    home_dir_var()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.batch_size, 100);
        assert_eq!(c.flush_interval_ms, 1_000);
        assert_eq!(c.compression_level, 3);
        assert_eq!(c.max_retries, 10);
        assert!(c.wal_dir.contains("tpt-microlog"));
    }

    #[test]
    fn parses_toml() {
        let toml = r#"
backend = "custom"
endpoint = "https://example.com/logs"
api_key = "secret"
batch_size = 500
flush_interval_ms = 250
compression_level = 5
max_retries = 3
wal_enabled = false
"#;
        let c = Config::from_toml_str(toml).unwrap();
        assert_eq!(c.backend, "custom");
        assert_eq!(c.endpoint.as_deref(), Some("https://example.com/logs"));
        assert_eq!(c.api_key.as_deref(), Some("secret"));
        assert_eq!(c.batch_size, 500);
        assert_eq!(c.flush_interval_ms, 250);
        assert_eq!(c.compression_level, 5);
        assert_eq!(c.max_retries, 3);
        assert!(!c.wal_enabled);
        // defaults preserved for other fields
        assert_eq!(c.wal_retry_interval_ms, 5_000);
    }

    #[test]
    fn min_level_invalid_ignored() {
        let toml = r#"min_level = "garbage""#;
        let c = Config::from_toml_str(toml).unwrap();
        assert!(c.min_level.is_none());
    }

    #[test]
    fn env_overrides() {
        std::env::set_var("TPT_LOG_BACKEND", "udp");
        std::env::set_var("TPT_LOG_BATCH_SIZE", "7");
        let mut c = Config::default();
        c.apply_env();
        assert_eq!(c.backend, "udp");
        assert_eq!(c.batch_size, 7);
        std::env::remove_var("TPT_LOG_BACKEND");
        std::env::remove_var("TPT_LOG_BATCH_SIZE");
    }
}
