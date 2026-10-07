use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::defaults;
use crate::LogLevel;

/// Backend selection string constants.
pub mod backend {
    /// No-op transport (in-memory only).
    pub const NONE: &str = "none";
    /// Human-readable stdout (pretty on a TTY, JSON lines when piped).
    pub const CONSOLE: &str = "console";
    /// Send to several backends at once (or fail over through them).
    pub const FANOUT: &str = "fanout";
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
    /// Grafana Loki HTTP push API.
    pub const LOKI: &str = "loki";
    /// OpenTelemetry Collector over OTLP/HTTP (JSON encoding).
    pub const OTLP_HTTP: &str = "otlp-http";
    /// Splunk HTTP Event Collector.
    pub const SPLUNK_HEC: &str = "splunk";
}

/// Wrapper for credential material that never leaks through `Debug` or
/// `Serialize` (both render `"[redacted]"`). Deserialize from plain text;
/// read back with [`Secret::expose`] only where the value is actually used.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(\"[redacted]\")")
    }
}

impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("[redacted]")
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Secret)
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Secret(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Secret(value.to_string())
    }
}

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// The underlying credential. Call sites must avoid logging it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// How the generic HTTP backend encodes request bodies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BodyMode {
    /// POST the reconstructed JSON batch: `{"entries":[...]}` (default).
    #[default]
    Json,
    /// POST the raw compressed bytes with `Content-Encoding: zstd`.
    Zstd,
}

impl std::fmt::Display for BodyMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            BodyMode::Json => "json",
            BodyMode::Zstd => "zstd",
        })
    }
}

impl std::str::FromStr for BodyMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "json" => Ok(BodyMode::Json),
            "zstd" => Ok(BodyMode::Zstd),
            other => Err(format!("unknown body_mode '{other}' (expected json, zstd)")),
        }
    }
}

impl Serialize for BodyMode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for BodyMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// What to do when the ingestion queue is full (spec §2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum QueueFullPolicy {
    /// Block the producer until space is available (never drops).
    #[default]
    Block,
    /// Drop the entry being pushed; the queue keeps its oldest entries.
    DropNewest,
    /// Evict the oldest buffered entry to make room for the new one.
    DropOldest,
}

impl std::fmt::Display for QueueFullPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            QueueFullPolicy::Block => "block",
            QueueFullPolicy::DropNewest => "drop_newest",
            QueueFullPolicy::DropOldest => "drop_oldest",
        })
    }
}

impl std::str::FromStr for QueueFullPolicy {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "block" => Ok(QueueFullPolicy::Block),
            "drop_newest" | "drop-newest" => Ok(QueueFullPolicy::DropNewest),
            "drop_oldest" | "drop-oldest" => Ok(QueueFullPolicy::DropOldest),
            other => Err(format!(
                "unknown queue_full_policy '{other}' (expected block, drop_newest, drop_oldest)"
            )),
        }
    }
}

impl Serialize for QueueFullPolicy {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for QueueFullPolicy {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// How eagerly batches are persisted before delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Durability {
    /// WAL is only written after delivery attempts are exhausted (default).
    #[default]
    BestEffort,
    /// Every batch is written to the WAL *before* the first delivery attempt;
    /// the WAL entry is removed on success. Survives an abrupt process kill
    /// at the cost of one disk write per batch.
    WalFirst,
}

impl std::fmt::Display for Durability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Durability::BestEffort => "best-effort",
            Durability::WalFirst => "wal-first",
        })
    }
}

impl std::str::FromStr for Durability {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "best-effort" | "best_effort" => Ok(Durability::BestEffort),
            "wal-first" | "wal_first" => Ok(Durability::WalFirst),
            other => Err(format!(
                "unknown durability '{other}' (expected best-effort, wal-first)"
            )),
        }
    }
}

impl Serialize for Durability {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Durability {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
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
    /// Secret token (Datadog `DD-API-KEY`, etc.). Redacted in Debug/serde.
    pub api_key: Option<Secret>,
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
    /// Redacted in Debug/serde.
    pub aws_secret_key: Option<Secret>,
    pub aws_log_group: Option<String>,
    pub aws_log_stream: Option<String>,
    /// STS session token for temporary credentials. Redacted in Debug/serde.
    pub aws_session_token: Option<Secret>,
    /// Create the log group/stream on first send when they do not exist.
    pub aws_auto_create_stream: bool,

    /// Datadog site (e.g. `datadoghq.eu`, `us5.datadoghq.com`); the intake
    /// endpoint becomes `https://http-intake.logs.<site>/api/v2/logs`.
    pub datadog_site: Option<String>,
    /// Ship `metric()` entries through the Datadog Metrics API as well.
    pub datadog_metrics: bool,

    /// Explicit Elasticsearch index name (default `tpt-microlog`).
    pub es_index: Option<String>,
    /// HTTP basic auth as `user:password` (Elasticsearch). Redacted.
    pub es_basic_auth: Option<Secret>,

    /// How the generic HTTP backend encodes the body: `json` (default) or
    /// `zstd` (opt-in compressed bytes with `Content-Encoding: zstd`).
    pub body_mode: BodyMode,

    /// Ask the TCP receiver for a 1-byte ack after each frame.
    pub tcp_ack: bool,

    /// Optional PEM bundle used to verify server TLS certificates.
    pub ca_cert: Option<String>,
    /// Optional HTTP(S) proxy URL for all network backends.
    pub proxy: Option<String>,
    /// Backends to route to when `backend = "fanout"`.
    pub backends: Vec<String>,
    /// With `backend = "fanout"`: deliver through the backends in order and
    /// stop at the first success instead of sending to all of them.
    pub failover: bool,
    /// Named configuration preset applied on load (see [`Config::apply_preset`]).
    pub preset: Option<String>,

    /// Flush batch at this many entries.
    pub batch_size: usize,
    /// Flush interval in milliseconds.
    pub flush_interval_ms: u64,
    /// Flush once accumulated bytes exceed this.
    pub memory_limit_bytes: usize,
    /// Ingestion queue capacity.
    pub queue_capacity: usize,
    /// Behavior when the ingestion queue is full.
    pub queue_full_policy: QueueFullPolicy,
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
    /// `Retry-After` responses are honored up to this cap (ms).
    pub retry_after_max_ms: u64,

    /// Enable write-ahead log persistence.
    pub wal_enabled: bool,
    /// WAL directory.
    pub wal_dir: String,
    /// WAL size cap in bytes (FIFO drop oldest).
    pub wal_max_size_bytes: u64,
    /// WAL retry poll interval (ms).
    pub wal_retry_interval_ms: u64,
    /// Move a WAL file to `wal/dead/` after this many failed replay passes.
    pub wal_dead_letter_replays: u32,
    /// Write every batch to the WAL before the first delivery attempt.
    pub durability: Durability,

    /// Hard budget for `shutdown()`; remaining batches spill to the WAL when
    /// it expires so the host app is never hung by the logger.
    pub shutdown_timeout_ms: u64,

    /// Attach hostname / PID / service / env vars to every entry (spec §8).
    pub enrichment: bool,
    /// Environment variable names captured into enriched entries.
    pub enrich_env_vars: Vec<String>,
    /// Drop entries below this level.
    pub min_level: Option<String>,
    /// Percent of logs to deliver (0.0 - 1.0, spec §8).
    pub sample_rate: f64,
    /// Per-level sampling overrides (level name → rate), e.g.
    /// `[sample_rate_by_level]\nDEBUG = 0.1`. WARN and above are never sampled.
    pub sample_rate_by_level: BTreeMap<String, f64>,
    /// Drop entries carrying any of these `tag=value` pairs (spec §8).
    pub filter_tags: Vec<String>,
    /// Drop entries whose message matches this regex (spec §8).
    pub filter_regex: Option<String>,
    /// Metadata keys (case-insensitive) whose values are always masked.
    pub redact_keys: Vec<String>,
    /// Regexes; matches in the message and string metadata are masked.
    pub redact_patterns: Vec<String>,
    /// Enable built-in detectors (email, card, SSN, JWT, bearer, AWS key).
    pub redact_builtin: bool,
    /// Fold identical entries (same level + message) seen within this many
    /// milliseconds into the first, adding `repeat_count`. 0 = off.
    pub dedup_window_ms: u64,
    /// Routine (below WARN) entries allowed per second. 0 = unlimited.
    pub rate_limit_per_sec: u32,
    /// Sample routine entries harder as the ingestion queue fills.
    pub adaptive_sampling: bool,
    /// Lowest sampling rate adaptive sampling falls to (0.0 - 1.0).
    pub adaptive_sampling_floor: f64,
    /// Keep this many recent low-severity entries and ship them only when an
    /// ERROR/FATAL follows. 0 = off.
    pub flight_recorder_size: usize,
    /// Entries at or below this level go to the flight recorder.
    pub flight_recorder_max_level: String,
    /// Entries per UTC day; sampling tightens automatically to stay within
    /// it. WARN and above are never limited. 0 = no budget.
    pub daily_budget_entries: u64,
    /// `host:port` for a plain-HTTP health endpoint (`/healthz`, `/stats`).
    pub health_bind: Option<String>,
    /// Emit the logger's own health as metrics every N ms. 0 = off.
    pub self_metrics_interval_ms: u64,

    /// Whether newline-delimited base64 framing is used for the WAL.
    pub wal_frame_newline: bool,
}

impl Default for Config {
    fn default() -> Self {
        let hostname = hostname::get().ok().and_then(|h| h.into_string().ok());
        Self {
            service: hostname,
            backend: String::from(backend::CONSOLE),
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
            aws_session_token: None,
            aws_auto_create_stream: false,
            datadog_site: None,
            datadog_metrics: false,
            es_index: None,
            es_basic_auth: None,
            body_mode: BodyMode::Json,
            tcp_ack: false,
            ca_cert: None,
            proxy: None,
            backends: Vec::new(),
            failover: false,
            preset: None,
            batch_size: defaults::BATCH_SIZE,
            flush_interval_ms: defaults::FLUSH_INTERVAL_MS,
            memory_limit_bytes: defaults::MEMORY_LIMIT_BYTES,
            queue_capacity: defaults::QUEUE_CAPACITY,
            queue_full_policy: QueueFullPolicy::Block,
            compression_level: defaults::COMPRESSION_LEVEL,
            fast_mode: false,
            max_retries: defaults::MAX_RETRIES,
            retry_base_ms: defaults::RETRY_BASE_MS,
            retry_max_backoff_ms: defaults::RETRY_MAX_BACKOFF_MS,
            retry_after_max_ms: defaults::RETRY_AFTER_MAX_MS,
            wal_enabled: true,
            wal_dir: default_wal_dir().to_string_lossy().to_string(),
            wal_max_size_bytes: defaults::WAL_MAX_SIZE_BYTES,
            wal_retry_interval_ms: defaults::WAL_RETRY_INTERVAL_MS,
            wal_dead_letter_replays: defaults::WAL_DEAD_LETTER_REPLAYS,
            durability: Durability::BestEffort,
            shutdown_timeout_ms: defaults::SHUTDOWN_TIMEOUT_MS,
            enrichment: false,
            enrich_env_vars: Vec::new(),
            min_level: None,
            sample_rate: 1.0,
            sample_rate_by_level: BTreeMap::new(),
            filter_tags: Vec::new(),
            filter_regex: None,
            redact_keys: Vec::new(),
            redact_patterns: Vec::new(),
            redact_builtin: false,
            dedup_window_ms: 0,
            rate_limit_per_sec: 0,
            adaptive_sampling: false,
            adaptive_sampling_floor: 0.1,
            flight_recorder_size: 0,
            flight_recorder_max_level: "DEBUG".to_string(),
            daily_budget_entries: 0,
            health_bind: None,
            self_metrics_interval_ms: 0,
            wal_frame_newline: true,
        }
    }
}

/// Configuration file names/locations searched when `TPT_LOG_CONFIG` is unset.
const CONFIG_FILE_NAME: &str = "tpt-log.toml";

/// Candidate config paths in increasing precedence: current directory, user
/// config dir, then system config dir.
fn config_search_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from(CONFIG_FILE_NAME)];
    if let Some(home) = home_dir_var() {
        let mut user = home;
        if cfg!(windows) {
            user.push("tpt-microlog");
        } else {
            user.push(".config");
            user.push("tpt-microlog");
        }
        paths.push(user.join(CONFIG_FILE_NAME));
    }
    paths.push(PathBuf::from("/etc/tpt-microlog").join(CONFIG_FILE_NAME));
    paths
}

/// Top-level field names accepted in `tpt-log.toml`; anything else triggers a
/// warning so typos never silently disable a feature.
const KNOWN_CONFIG_KEYS: &[&str] = &[
    "service",
    "backend",
    "endpoint",
    "api_key",
    "extra_headers",
    "grpc_endpoint",
    "net_endpoint",
    "file_path",
    "file_rotation_bytes",
    "file_rotation_seconds",
    "file_rotations",
    "prometheus_bind",
    "aws_region",
    "aws_access_key",
    "aws_secret_key",
    "aws_log_group",
    "aws_log_stream",
    "aws_session_token",
    "aws_auto_create_stream",
    "datadog_site",
    "datadog_metrics",
    "es_index",
    "es_basic_auth",
    "body_mode",
    "tcp_ack",
    "ca_cert",
    "proxy",
    "backends",
    "failover",
    "preset",
    "batch_size",
    "flush_interval_ms",
    "memory_limit_bytes",
    "queue_capacity",
    "queue_full_policy",
    "compression_level",
    "fast_mode",
    "max_retries",
    "retry_base_ms",
    "retry_max_backoff_ms",
    "retry_after_max_ms",
    "wal_enabled",
    "wal_dir",
    "wal_max_size_bytes",
    "wal_retry_interval_ms",
    "wal_dead_letter_replays",
    "durability",
    "shutdown_timeout_ms",
    "enrichment",
    "enrich_env_vars",
    "min_level",
    "sample_rate",
    "sample_rate_by_level",
    "filter_tags",
    "filter_regex",
    "redact_keys",
    "redact_patterns",
    "redact_builtin",
    "dedup_window_ms",
    "rate_limit_per_sec",
    "adaptive_sampling",
    "adaptive_sampling_floor",
    "flight_recorder_size",
    "flight_recorder_max_level",
    "daily_budget_entries",
    "health_bind",
    "self_metrics_interval_ms",
    "wal_frame_newline",
];

fn warn_unknown_keys(raw: &str) {
    let Ok(value) = toml::from_str::<toml::Value>(raw) else {
        return; // parse error surfaces through the real deserializer
    };
    let Some(table) = value.as_table() else {
        return;
    };
    for key in table.keys() {
        if !KNOWN_CONFIG_KEYS.contains(&key.as_str()) {
            let suggestion = KNOWN_CONFIG_KEYS
                .iter()
                .filter(|c| levenshtein_close(c, key))
                .min_by_key(|c| edit_distance(c, key))
                .map(|c| format!(" (did you mean '{c}'?)"))
                .unwrap_or_default();
            crate::diag::report(
                "config",
                &format!("ignoring unknown config key '{key}'{suggestion}"),
            );
        }
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn levenshtein_close(candidate: &str, key: &str) -> bool {
    edit_distance(candidate, key) * 2 < candidate.len().max(key.len())
}

impl Config {
    /// Load configuration following this order:
    ///
    /// 1. `$TPT_LOG_CONFIG` (explicit file path), else
    ///    `./tpt-log.toml`, then `~/.config/tpt-microlog/tpt-log.toml`
    ///    (`%APPDATA%\tpt-microlog` on Windows), then
    ///    `/etc/tpt-microlog/tpt-log.toml`;
    /// 2. `TPT_LOG_*` environment variable overrides.
    ///
    /// Parse failures and unknown keys warn on stderr and fall back to
    /// defaults rather than being swallowed silently.
    pub fn load() -> Self {
        let explicit = env_opt("TPT_LOG_CONFIG");
        let path = explicit
            .clone()
            .map(PathBuf::from)
            .or_else(|| config_search_paths().into_iter().find(|p| p.is_file()));
        let mut cfg = match &path {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(raw) => match Self::from_toml_str(&raw) {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        crate::diag::report(
                            "config",
                            &format!("ignoring invalid config file {}: {e}", path.display()),
                        );
                        Self::default()
                    }
                },
                Err(e) => {
                    crate::diag::report(
                        "config",
                        &format!("cannot read config file {}: {e}", path.display()),
                    );
                    Self::default()
                }
            },
            None => Self::default(),
        };
        cfg.apply_env();
        if let Some(preset) = cfg.preset.clone() {
            // A preset set via TPT_LOG_PRESET applies after env overrides so
            // it can flip backends on top of a config file.
            let _ = cfg.apply_preset(&preset);
        }
        cfg.normalize();
        cfg
    }

    /// Parse from a raw TOML string. Unknown keys warn on stderr; invalid
    /// values are returned as an error. A `preset = "..."` key applies the
    /// named preset before normalization.
    pub fn from_toml_str(raw: &str) -> Result<Self, toml::de::Error> {
        warn_unknown_keys(raw);
        let mut cfg = toml::from_str::<Config>(raw)?;
        if let Some(preset) = cfg.preset.clone() {
            if let Err(e) = cfg.apply_preset(&preset) {
                crate::diag::report("config", &e);
            }
        }
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
            self.api_key = Some(Secret::new(v));
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
        if let Some(v) = env("TPT_LOG_QUEUE_CAPACITY").and_then(|s| s.parse().ok()) {
            self.queue_capacity = v;
        }
        if let Some(v) = env("TPT_LOG_QUEUE_FULL_POLICY").and_then(|s| s.parse().ok()) {
            self.queue_full_policy = v;
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
        if let Some(v) = env("TPT_LOG_WAL_RETRY_INTERVAL_MS").and_then(|s| s.parse().ok()) {
            self.wal_retry_interval_ms = v;
        }
        if let Some(v) = env("TPT_LOG_DURABILITY").and_then(|s| s.parse().ok()) {
            self.durability = v;
        }
        if let Some(v) = env("TPT_LOG_SHUTDOWN_TIMEOUT_MS").and_then(|s| s.parse().ok()) {
            self.shutdown_timeout_ms = v;
        }
        if let Some(v) = env("TPT_LOG_ENRICHMENT") {
            self.enrichment = matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = env("TPT_LOG_ENRICH_ENV_VARS") {
            self.enrich_env_vars = split_comma_list(&v);
        }
        if let Some(v) = env("TPT_LOG_MIN_LEVEL") {
            self.min_level = Some(v);
        }
        if let Some(v) = env("TPT_LOG_SAMPLE_RATE").and_then(|s| s.parse().ok()) {
            self.sample_rate = v;
        }
        if let Some(v) = env("TPT_LOG_FILTER_TAGS") {
            self.filter_tags = split_comma_list(&v);
        }
        if let Some(v) = env("TPT_LOG_FILTER_REGEX") {
            self.filter_regex = Some(v);
        }
        if let Some(v) = env("TPT_LOG_REDACT_KEYS") {
            self.redact_keys = split_comma_list(&v);
        }
        if let Some(v) = env("TPT_LOG_DEDUP_WINDOW_MS").and_then(|s| s.parse().ok()) {
            self.dedup_window_ms = v;
        }
        if let Some(v) = env("TPT_LOG_RATE_LIMIT_PER_SEC").and_then(|s| s.parse().ok()) {
            self.rate_limit_per_sec = v;
        }
        if let Some(v) = env("TPT_LOG_ADAPTIVE_SAMPLING") {
            self.adaptive_sampling =
                matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = env("TPT_LOG_FLIGHT_RECORDER_SIZE").and_then(|s| s.parse().ok()) {
            self.flight_recorder_size = v;
        }
        if let Some(v) = env("TPT_LOG_DAILY_BUDGET_ENTRIES").and_then(|s| s.parse().ok()) {
            self.daily_budget_entries = v;
        }
        if let Some(v) = env("TPT_LOG_HEALTH_BIND") {
            self.health_bind = Some(v);
        }
        if let Some(v) = env("TPT_LOG_REDACT_BUILTIN") {
            self.redact_builtin = matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = env("TPT_LOG_FAST_MODE") {
            self.fast_mode = matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = env("TPT_LOG_FILE_PATH") {
            self.file_path = Some(v);
        }
        if let Some(v) = env("TPT_LOG_NET_ENDPOINT") {
            self.net_endpoint = Some(v);
        }
        if let Some(v) = env("TPT_LOG_GRPC_ENDPOINT") {
            self.grpc_endpoint = Some(v);
        }
        if let Some(v) = env("TPT_LOG_EXTRA_HEADERS") {
            self.extra_headers = split_comma_list(&v);
        }
        if let Some(v) = env("TPT_LOG_PROMETHEUS_BIND") {
            self.prometheus_bind = v;
        }
        if let Some(v) = env("TPT_LOG_AWS_REGION") {
            self.aws_region = Some(v);
        }
        if let Some(v) = env("TPT_LOG_AWS_ACCESS_KEY") {
            self.aws_access_key = Some(v);
        }
        if let Some(v) = env("TPT_LOG_AWS_SECRET_KEY") {
            self.aws_secret_key = Some(Secret::new(v));
        }
        if let Some(v) = env("TPT_LOG_AWS_LOG_GROUP") {
            self.aws_log_group = Some(v);
        }
        if let Some(v) = env("TPT_LOG_AWS_LOG_STREAM") {
            self.aws_log_stream = Some(v);
        }
        if let Some(v) = env("TPT_LOG_AWS_SESSION_TOKEN") {
            self.aws_session_token = Some(Secret::new(v));
        }
        if let Some(v) = env("TPT_LOG_AWS_AUTO_CREATE_STREAM") {
            self.aws_auto_create_stream =
                matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = env("TPT_LOG_DATADOG_SITE") {
            self.datadog_site = Some(v);
        }
        if let Some(v) = env("TPT_LOG_DATADOG_METRICS") {
            self.datadog_metrics = matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = env("TPT_LOG_ES_INDEX") {
            self.es_index = Some(v);
        }
        if let Some(v) = env("TPT_LOG_ES_BASIC_AUTH") {
            self.es_basic_auth = Some(Secret::new(v));
        }
        if let Some(v) = env("TPT_LOG_BODY_MODE").and_then(|s| s.parse().ok()) {
            self.body_mode = v;
        }
        if let Some(v) = env("TPT_LOG_TCP_ACK") {
            self.tcp_ack = matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = env("TPT_LOG_CA_CERT") {
            self.ca_cert = Some(v);
        }
        if let Some(v) = env("TPT_LOG_PROXY") {
            self.proxy = Some(v);
        }
        if let Some(v) = env("TPT_LOG_BACKENDS") {
            self.backends = split_comma_list(&v);
        }
        if let Some(v) = env("TPT_LOG_FAILOVER") {
            self.failover = matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = env("TPT_LOG_PRESET") {
            self.preset = Some(v);
        }
    }

    /// AWS credential chain: explicit config first, then the standard
    /// `AWS_*` environment variables (IAM roles via metadata endpoints are
    /// not supported by this crate).
    pub fn resolved_aws_credentials(&self) -> Option<(String, String, Option<String>)> {
        let access = self
            .aws_access_key
            .clone()
            .or_else(|| env_opt("AWS_ACCESS_KEY_ID"))?;
        let secret = self
            .aws_secret_key
            .as_ref()
            .map(|s| s.expose().to_string())
            .or_else(|| env_opt("AWS_SECRET_ACCESS_KEY"))?;
        let token = self
            .aws_session_token
            .as_ref()
            .map(|s| s.expose().to_string())
            .or_else(|| env_opt("AWS_SESSION_TOKEN"));
        Some((access, secret, token))
    }

    /// A `(hostname, pid)` pair used by the enrichment layer. The host is the
    /// real machine hostname (or `TPT_LOG_HOST`); it is *not* the service name.
    pub fn host_and_process(&self) -> (String, String) {
        let host = env_opt("TPT_LOG_HOST")
            .or_else(|| hostname::get().ok().and_then(|h| h.into_string().ok()))
            .unwrap_or_else(|| "unknown".to_string());
        let pid = std::process::id().to_string();
        (host, pid)
    }

    /// Values of the configured [`Config::enrich_env_vars`], present ones only.
    pub fn enrichment_env_vars(&self) -> Vec<(String, String)> {
        self.enrich_env_vars
            .iter()
            .filter_map(|name| std::env::var(name).ok().map(|v| (name.clone(), v)))
            .collect()
    }

    /// Apply a named preset over the current config (fields the preset sets
    /// are overwritten; everything else is kept). Unknown names return an
    /// actionable error listing the available presets.
    pub fn apply_preset(&mut self, name: &str) -> Result<(), String> {
        match name.trim().to_ascii_lowercase().as_str() {
            "dev" => {
                self.backend = backend::CONSOLE.into();
                self.min_level = Some("DEBUG".into());
                self.wal_enabled = false;
                self.flush_interval_ms = 250;
                self.batch_size = 10;
            }
            "prod" => {
                self.batch_size = 500;
                self.flush_interval_ms = 1_000;
                self.min_level = Some("INFO".into());
                self.wal_enabled = true;
                self.enrichment = true;
                self.max_retries = 10;
            }
            "datadog-eu" => {
                self.backend = backend::DATADOG.into();
                self.datadog_site = Some("datadoghq.eu".into());
            }
            "elastic-cloud" => {
                self.backend = backend::ELASTICSEARCH.into();
                self.es_index.get_or_insert_with(|| "tpt-microlog".into());
            }
            "grafana-loki" => {
                self.backend = backend::LOKI.into();
                self.endpoint
                    .get_or_insert_with(|| "http://localhost:3100".into());
            }
            "otel-collector" => {
                self.backend = backend::GRPC.into();
                self.grpc_endpoint
                    .get_or_insert_with(|| "http://localhost:4317".into());
            }
            "cloudwatch-lambda" => {
                self.backend = backend::CLOUDWATCH.into();
                if self.aws_region.is_none() {
                    self.aws_region = std::env::var("AWS_REGION")
                        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
                        .ok();
                }
                self.aws_auto_create_stream = true;
            }
            other => {
                return Err(format!(
                    "unknown preset '{other}' (available: dev, prod, datadog-eu, elastic-cloud, \
                     grafana-loki, otel-collector, cloudwatch-lambda)"
                ));
            }
        }
        Ok(())
    }

    /// Backend-agnostic sanity checks with actionable messages. Returns every
    /// problem found; an empty vector means the config is ready to use.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        match self.backend.as_str() {
            backend::DATADOG => {
                if self.api_key.is_none() {
                    problems.push(
                        "datadog backend needs `api_key` — set it in tpt-log.toml, the \
                         MicroLog(api_key=...) argument, or TPT_LOG_API_KEY"
                            .into(),
                    );
                }
            }
            backend::ELASTICSEARCH => {
                if self.api_key.is_none() && self.es_basic_auth.is_none() {
                    problems.push(
                        "elasticsearch backend usually needs `api_key` or `es_basic_auth` \
                         (anonymous access only works on unsecured clusters)"
                            .into(),
                    );
                }
            }
            backend::CLOUDWATCH => {
                if self.aws_region.is_none() {
                    problems.push(
                        "cloudwatch backend needs `aws_region` (or the AWS_REGION env var)".into(),
                    );
                }
                if self.resolved_aws_credentials().is_none() {
                    problems.push(
                        "cloudwatch backend needs credentials: set `aws_access_key` + \
                         `aws_secret_key`, or the AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY \
                         env vars"
                            .into(),
                    );
                }
                if self.aws_log_group.is_none() || self.aws_log_stream.is_none() {
                    problems.push(
                        "cloudwatch backend needs `aws_log_group` and `aws_log_stream`".into(),
                    );
                }
            }
            backend::CUSTOM_HTTP => {
                if self.endpoint.is_none() {
                    problems.push(
                        "http backend needs `endpoint` (e.g. endpoint = \"https://logs.example.com/v1\")"
                            .into(),
                    );
                }
            }
            backend::GRPC => {
                if self.grpc_endpoint.is_none() {
                    problems.push(
                        "grpc backend needs `grpc_endpoint` (host:port of an OpenTelemetry \
                         Collector — the default receiver port is 4317)"
                            .into(),
                    );
                }
            }
            backend::UDP | backend::TCP => {
                if self.net_endpoint.is_none() {
                    problems.push(format!(
                        "{} backend needs `net_endpoint` (host:port)",
                        self.backend
                    ));
                }
            }
            backend::FILE => {
                if self.file_path.is_none() {
                    problems.push("file backend needs `file_path`".into());
                }
            }
            backend::FANOUT if self.backends.is_empty() => {
                problems.push(
                    "fanout backend needs `backends = [\"datadog\", \"file\"]` — the \
                     list of backend names to route to"
                        .into(),
                );
            }
            _ => {}
        }
        problems
    }

    fn normalize(&mut self) {
        if let Some(l) = &self.min_level {
            if l.parse::<LogLevel>().is_err() {
                self.min_level = None;
            }
        }
        self.wal_dir = expand_tilde(&self.wal_dir);
        self.sample_rate = self.sample_rate.clamp(0.0, 1.0);
        self.adaptive_sampling_floor = self.adaptive_sampling_floor.clamp(0.0, 1.0);
        if self.flight_recorder_max_level.parse::<LogLevel>().is_err() {
            self.flight_recorder_max_level = "DEBUG".to_string();
        }
        self.sample_rate_by_level
            .retain(|k, _| k.parse::<LogLevel>().is_ok());
    }

    /// Resolved WAL directory with a leading `~` expanded.
    pub fn resolved_wal_dir(&self) -> PathBuf {
        PathBuf::from(expand_tilde(&self.wal_dir))
    }

    /// Map a `backend` string to an HTTP base URL when not explicitly supplied.
    pub fn inferred_endpoint(&self) -> Option<String> {
        match self.backend.as_str() {
            crate::config::backend::DATADOG => {
                Some("https://http-intake.logs.datadoghq.com/api/v2/logs".to_string())
            }
            crate::config::backend::ELASTICSEARCH => Some("http://localhost:9200".to_string()),
            crate::config::backend::LOKI => Some("http://localhost:3100".to_string()),
            crate::config::backend::OTLP_HTTP => Some("http://localhost:4318".to_string()),
            crate::config::backend::CLOUDWATCH => None,
            crate::config::backend::SPLUNK_HEC => None,
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

fn split_comma_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Expand only a *leading* `~` to the home directory. Interior `~` characters
/// are left alone so Windows 8.3 short paths (e.g. `PHILL~1`) survive.
fn expand_tilde(path: &str) -> String {
    let home = || {
        home_dir_var()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().to_string())
    };
    if path == "~" {
        return home();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return format!("{}/{}", home(), rest);
    }
    if let Some(rest) = path.strip_prefix("~\\") {
        return format!("{}\\{}", home(), rest);
    }
    path.to_string()
}

/// Best-effort home directory lookup (no external dependency).
fn home_dir_var() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped JSON Schema must describe exactly the keys the loader
    /// accepts and every field `Config` serializes, so it cannot drift.
    #[test]
    fn json_schema_matches_config_keys() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../tpt-log.schema.json")).unwrap();
        let props: std::collections::BTreeSet<String> = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let known: std::collections::BTreeSet<String> =
            KNOWN_CONFIG_KEYS.iter().map(|k| k.to_string()).collect();
        assert_eq!(props, known, "schema vs KNOWN_CONFIG_KEYS");

        let serialized = serde_json::to_value(Config::default()).unwrap();
        for key in serialized.as_object().unwrap().keys() {
            assert!(
                props.contains(key),
                "Config field `{key}` missing from schema"
            );
        }
        let presets = schema["properties"]["preset"]["enum"].as_array().unwrap();
        for name in presets {
            Config::default()
                .apply_preset(name.as_str().unwrap())
                .unwrap_or_else(|e| panic!("schema lists unusable preset: {e}"));
        }
    }

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.batch_size, 100);
        assert_eq!(c.flush_interval_ms, 1_000);
        assert_eq!(c.compression_level, 3);
        assert_eq!(c.max_retries, 10);
        assert!(c.wal_dir.contains("tpt-microlog"));
        assert_eq!(c.queue_full_policy, QueueFullPolicy::Block);
        assert_eq!(c.durability, Durability::BestEffort);
        assert!(c.shutdown_timeout_ms > 0);
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
queue_full_policy = "drop_newest"
durability = "wal-first"
"#;
        let c = Config::from_toml_str(toml).unwrap();
        assert_eq!(c.backend, "custom");
        assert_eq!(c.endpoint.as_deref(), Some("https://example.com/logs"));
        assert_eq!(c.api_key.as_ref().map(|s| s.expose()), Some("secret"));
        assert_eq!(c.batch_size, 500);
        assert_eq!(c.flush_interval_ms, 250);
        assert_eq!(c.compression_level, 5);
        assert_eq!(c.max_retries, 3);
        assert!(!c.wal_enabled);
        assert_eq!(c.queue_full_policy, QueueFullPolicy::DropNewest);
        assert_eq!(c.durability, Durability::WalFirst);
        // defaults preserved for other fields
        assert_eq!(c.wal_retry_interval_ms, 5_000);
    }

    #[test]
    fn rejects_invalid_enum_values() {
        let toml = r#"queue_full_policy = "explode""#;
        assert!(Config::from_toml_str(toml).is_err());
        let toml = r#"durability = "maybe""#;
        assert!(Config::from_toml_str(toml).is_err());
    }

    #[test]
    fn secrets_redacted_in_debug_and_serde() {
        let toml = r#"api_key = "super-secret""#;
        let c = Config::from_toml_str(toml).unwrap();
        let debug = format!("{c:?}");
        assert!(!debug.contains("super-secret"), "leaked via Debug: {debug}");
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("super-secret"), "leaked via serde: {json}");
        assert!(c.api_key.as_ref().unwrap().expose() == "super-secret");
    }

    #[test]
    fn min_level_invalid_ignored() {
        let toml = r#"min_level = "garbage""#;
        let c = Config::from_toml_str(toml).unwrap();
        assert!(c.min_level.is_none());
    }

    #[test]
    fn sample_rate_by_level_normalizes() {
        let toml = r#"
[sample_rate_by_level]
DEBUG = 0.1
garbage = 0.5
"#;
        let c = Config::from_toml_str(toml).unwrap();
        assert!(c.sample_rate_by_level.contains_key("DEBUG"));
        assert!(!c.sample_rate_by_level.contains_key("garbage"));
    }

    #[test]
    fn tilde_only_expanded_at_start() {
        let mut c = Config {
            wal_dir: "~/logs/wal".to_string(),
            ..Config::default()
        };
        c.normalize();
        assert!(!c.wal_dir.starts_with('~'));
        assert!(c.wal_dir.contains("/logs/wal") || c.wal_dir.contains("\\logs\\wal"));

        // Windows 8.3 short names must not be mangled.
        let mut c = Config {
            wal_dir: r"C:\Users\PHILL~1\AppData\wal".to_string(),
            ..Config::default()
        };
        c.normalize();
        assert_eq!(c.wal_dir, r"C:\Users\PHILL~1\AppData\wal");
    }

    #[test]
    fn host_and_process_is_real_hostname() {
        let c = Config {
            service: Some("my-service".into()),
            ..Config::default()
        };
        let (host, pid) = c.host_and_process();
        assert_ne!(host, "my-service", "host must not be the service name");
        assert_eq!(pid, std::process::id().to_string());
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

    #[test]
    fn env_expanded_coverage() {
        std::env::set_var("TPT_LOG_FILE_PATH", "/tmp/x.jsonl");
        std::env::set_var("TPT_LOG_NET_ENDPOINT", "127.0.0.1:9999");
        std::env::set_var("TPT_LOG_GRPC_ENDPOINT", "localhost:4317");
        std::env::set_var("TPT_LOG_EXTRA_HEADERS", "a=1,b=2");
        std::env::set_var("TPT_LOG_PROMETHEUS_BIND", "0.0.0.0:9100");
        std::env::set_var("TPT_LOG_QUEUE_CAPACITY", "42");
        std::env::set_var("TPT_LOG_WAL_RETRY_INTERVAL_MS", "250");
        std::env::set_var("TPT_LOG_AWS_REGION", "us-east-1");
        std::env::set_var("TPT_LOG_AWS_SECRET_KEY", "wJalrXUtnFEMI");
        std::env::set_var("TPT_LOG_QUEUE_FULL_POLICY", "drop_oldest");
        std::env::set_var("TPT_LOG_DURABILITY", "wal-first");
        let mut c = Config::default();
        c.apply_env();
        assert_eq!(c.file_path.as_deref(), Some("/tmp/x.jsonl"));
        assert_eq!(c.net_endpoint.as_deref(), Some("127.0.0.1:9999"));
        assert_eq!(c.grpc_endpoint.as_deref(), Some("localhost:4317"));
        assert_eq!(c.extra_headers, vec!["a=1".to_string(), "b=2".to_string()]);
        assert_eq!(c.prometheus_bind, "0.0.0.0:9100");
        assert_eq!(c.queue_capacity, 42);
        assert_eq!(c.wal_retry_interval_ms, 250);
        assert_eq!(c.aws_region.as_deref(), Some("us-east-1"));
        assert_eq!(
            c.aws_secret_key.as_ref().map(|s| s.expose()),
            Some("wJalrXUtnFEMI")
        );
        assert_eq!(c.queue_full_policy, QueueFullPolicy::DropOldest);
        assert_eq!(c.durability, Durability::WalFirst);
        for key in [
            "TPT_LOG_FILE_PATH",
            "TPT_LOG_NET_ENDPOINT",
            "TPT_LOG_GRPC_ENDPOINT",
            "TPT_LOG_EXTRA_HEADERS",
            "TPT_LOG_PROMETHEUS_BIND",
            "TPT_LOG_QUEUE_CAPACITY",
            "TPT_LOG_WAL_RETRY_INTERVAL_MS",
            "TPT_LOG_AWS_REGION",
            "TPT_LOG_AWS_SECRET_KEY",
            "TPT_LOG_QUEUE_FULL_POLICY",
            "TPT_LOG_DURABILITY",
        ] {
            std::env::remove_var(key);
        }
    }
}
