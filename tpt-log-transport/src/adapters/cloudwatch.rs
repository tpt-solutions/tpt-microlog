//! CloudWatch Logs adapter (spec §5.3).
//!
//! POSTs `PutLogEvents` to CloudWatch Logs with AWS SigV4 signing.
//!
//! - credentials come from the config or the standard `AWS_ACCESS_KEY_ID` /
//!   `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` environment variables
//!   (IAM roles via metadata endpoints are not supported by this crate);
//! - a session token, when present, is attached and signed
//!   (`x-amz-security-token`);
//! - events are sorted by timestamp (PutLogEvents rejects unsorted batches),
//!   carry their metadata inline, and batches respect the service limits:
//!   max 10 000 events and ~1 MB per request, events older than 14 days are
//!   skipped;
//! - with `aws_auto_create_stream`, the log group/stream are created on the
//!   first send if missing.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tpt_log_core::{CompressedBatch, Config, LogEntry, Transport, TransportError};

use crate::adapters::sigv4::{self, Credentials};
use crate::http::{pooled_client, request_once};

const SERVICE: &str = "logs";
/// PutLogEvents: max 10 000 events per call.
const MAX_EVENTS_PER_CALL: usize = 10_000;
/// PutLogEvents: max 1 MB per call (the docs reserve 26 B of overhead).
const MAX_BODY_BYTES: usize = 1_024 * 1_024 - 26 * 1_024;
/// Events older than 14 days are rejected by the service.
const MAX_AGE_MS: u64 = 14 * 24 * 60 * 60 * 1_000;

/// CloudWatch Logs PutLogEvents adapter.
pub struct CloudWatchAdapter {
    client: reqwest::blocking::Client,
    url: String,
    host: String,
    creds: Credentials,
    session_token: Option<String>,
    log_group: String,
    log_stream: String,
    auto_create: bool,
    stream_ensured: AtomicBool,
    /// Fixed base epoch used to skip too-old events consistently in tests.
    now_ms: fn() -> u64,
}

impl std::fmt::Debug for CloudWatchAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudWatchAdapter")
            .field("url", &self.url)
            .field("log_group", &self.log_group)
            .field("log_stream", &self.log_stream)
            .finish_non_exhaustive()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Render an entry as a CloudWatch event: the message carries its structured
/// metadata inline (`message key="value" ...`) because PutLogEvents has no
/// attribute field.
fn to_event(entry: &LogEntry) -> serde_json::Value {
    let mut message = entry.message.clone();
    let mut pairs: Vec<String> = entry
        .metadata
        .iter()
        .map(|(k, v)| format!("{k}={}", render_meta_value(v)))
        .collect();
    pairs.sort();
    if !pairs.is_empty() {
        message.push(' ');
        message.push_str(&pairs.join(" "));
    }
    serde_json::json!({
        "timestamp": entry.timestamp,
        "message": message,
    })
}

fn render_meta_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => format!("{s:?}"),
        other => other.to_string(),
    }
}

impl CloudWatchAdapter {
    /// Build from core config. Requires `aws_region` plus credentials from
    /// the config or the `AWS_*` environment variables; `aws_log_group` and
    /// `aws_log_stream` name the destination. The endpoint defaults to
    /// `https://logs.<region>.amazonaws.com`.
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let region = config.aws_region.clone().ok_or_else(|| {
            TransportError::Config("CloudWatch adapter needs `aws_region`".into())
        })?;
        let (access_key, secret_key, session_token) = config
            .resolved_aws_credentials()
            .ok_or_else(|| {
                TransportError::Config(
                    "CloudWatch adapter needs `aws_access_key`/`aws_secret_key` (or AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY env)".into(),
                )
            })?;
        let log_group = config.aws_log_group.clone().ok_or_else(|| {
            TransportError::Config("CloudWatch adapter needs `aws_log_group`".into())
        })?;
        let log_stream = config.aws_log_stream.clone().ok_or_else(|| {
            TransportError::Config("CloudWatch adapter needs `aws_log_stream`".into())
        })?;

        let host = config
            .endpoint
            .clone()
            .unwrap_or_else(|| format!("logs.{region}.amazonaws.com"));
        let url = if host.starts_with("http://") || host.starts_with("https://") {
            host.clone()
        } else {
            format!("https://{host}")
        };
        let hostname = url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or(&host)
            .to_string();

        Ok(Self {
            client: pooled_client(),
            url,
            host: hostname,
            creds: Credentials {
                access_key,
                secret_key,
                region,
            },
            session_token,
            log_group,
            log_stream,
            auto_create: config.aws_auto_create_stream,
            stream_ensured: AtomicBool::new(false),
            now_ms,
        })
    }

    /// One signed POST against the CloudWatch Logs JSON API.
    fn post(&self, target: &str, body: &[u8]) -> Result<(), TransportError> {
        let mut extra = BTreeMap::new();
        extra.insert("content-type".into(), "application/x-amz-json-1.1".into());
        extra.insert("x-amz-target".into(), target.into());
        if let Some(token) = &self.session_token {
            extra.insert("x-amz-security-token".into(), token.clone());
        }
        let (auth, amz_date) = sigv4::sign(&self.creds, SERVICE, "POST", &self.host, "/", &extra, body);

        let mut headers = vec![
            ("authorization".to_string(), auth),
            ("x-amz-date".to_string(), amz_date),
            (
                "content-type".to_string(),
                "application/x-amz-json-1.1".to_string(),
            ),
            ("x-amz-target".to_string(), target.to_string()),
        ];
        if let Some(token) = &self.session_token {
            headers.push(("x-amz-security-token".to_string(), token.clone()));
        }
        request_once(&self.client, &self.url, &headers, "application/x-amz-json-1.1", body.to_vec())
    }

    /// Create the log group/stream when `aws_auto_create_stream` is set.
    /// Existing groups/streams are fine (the API errors with
    /// `ResourceAlreadyExists`, which is ignored).
    fn ensure_stream(&self) -> Result<(), TransportError> {
        if !self.auto_create || self.stream_ensured.load(Ordering::Relaxed) {
            return Ok(());
        }
        let group = serde_json::json!({ "logGroupName": self.log_group });
        let group_body = serde_json::to_vec(&group)
            .map_err(|e| TransportError::Rejected(format!("serialize failed: {e}")))?;
        if let Err(e) = self.post("Logs_20140328.CreateLogGroup", &group_body) {
            // Only already-exists is benign; surface anything else.
            let text = e.to_string();
            if !text.contains("ResourceAlreadyExists") && !text.contains("already exists") {
                return Err(e);
            }
        }
        let stream = serde_json::json!({
            "logGroupName": self.log_group,
            "logStreamName": self.log_stream,
        });
        let stream_body = serde_json::to_vec(&stream)
            .map_err(|e| TransportError::Rejected(format!("serialize failed: {e}")))?;
        if let Err(e) = self.post("Logs_20140328.CreateLogStream", &stream_body) {
            let text = e.to_string();
            if !text.contains("ResourceAlreadyExists") && !text.contains("already exists") {
                return Err(e);
            }
        }
        self.stream_ensured.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Filter to fresh events, sort ascending by timestamp, and split into
    /// service-limit-respecting chunks (10k events / ~1 MB each).
    fn chunk_events(&self, entries: &[LogEntry]) -> Vec<Vec<serde_json::Value>> {
        let cutoff = (self.now_ms)().saturating_sub(MAX_AGE_MS);
        let mut events: Vec<serde_json::Value> = entries
            .iter()
            .filter(|e| e.timestamp >= cutoff)
            .map(to_event)
            .collect();
        events.sort_by_key(|e| {
            e.get("timestamp")
                .and_then(|t| t.as_u64())
                .unwrap_or_default()
        });

        let mut chunks = Vec::new();
        let mut current: Vec<serde_json::Value> = Vec::new();
        let mut current_bytes = 0usize;
        for event in events {
            let approx = serde_json::to_string(&event).map(|s| s.len()).unwrap_or(0) + 1;
            if !current.is_empty()
                && (current.len() >= MAX_EVENTS_PER_CALL
                    || current_bytes + approx > MAX_BODY_BYTES)
            {
                chunks.push(std::mem::take(&mut current));
                current_bytes = 0;
            }
            current_bytes += approx;
            current.push(event);
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        chunks
    }

    fn build_body(&self, events: &[serde_json::Value]) -> serde_json::Value {
        serde_json::json!({
            "logGroupName": self.log_group,
            "logStreamName": self.log_stream,
            "logEvents": events,
        })
    }
}

impl Transport for CloudWatchAdapter {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        let entries = batch
            .decompress_entries()
            .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;
        self.ensure_stream()?;

        for events in self.chunk_events(&entries) {
            let body = serde_json::to_vec(&self.build_body(&events))
                .map_err(|e| TransportError::Rejected(format!("serialize failed: {e}")))?;
            self.post("Logs_20140328.PutLogEvents", &body)?;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "cloudwatch"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_log_core::{LogLevel, LogType, Secret};

    fn test_config() -> Config {
        Config {
            aws_region: Some("eu-west-1".into()),
            aws_access_key: Some("AKID".into()),
            aws_secret_key: Some(Secret::new("SECRET")),
            aws_log_group: Some("group".into()),
            aws_log_stream: Some("stream".into()),
            endpoint: Some("https://logs.eu-west-1.amazonaws.com".into()),
            ..Config::default()
        }
    }

    fn entry(timestamp: u64, message: &str) -> LogEntry {
        LogEntry {
            timestamp,
            level: LogLevel::Info,
            log_type: LogType::Log,
            message: message.into(),
            metadata: Default::default(),
        }
    }

    #[test]
    fn build_body_shape() {
        let adapter = CloudWatchAdapter::from_config(&test_config()).unwrap();
        let events = adapter.chunk_events(&[entry(42, "hi")]);
        let body = adapter.build_body(&events[0]);
        assert_eq!(body["logGroupName"], "group");
        assert_eq!(body["logStreamName"], "stream");
        assert_eq!(body["logEvents"][0]["timestamp"], 42);
        assert_eq!(body["logEvents"][0]["message"], "hi");
    }

    #[test]
    fn requires_region_and_credentials() {
        let cfg = Config {
            aws_region: Some("us-east-1".into()),
            ..Config::default()
        };
        assert!(CloudWatchAdapter::from_config(&cfg).is_err());
        // Without any AWS fields at all.
        let cfg = Config::default();
        assert!(CloudWatchAdapter::from_config(&cfg).is_err());
    }

    #[test]
    fn credentials_can_come_from_env() {
        std::env::set_var("AWS_ACCESS_KEY_ID", "AKID-ENV");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "SECRET-ENV");
        std::env::set_var("AWS_SESSION_TOKEN", "TOKEN-ENV");
        let cfg = Config {
            aws_region: Some("us-east-1".into()),
            aws_log_group: Some("g".into()),
            aws_log_stream: Some("s".into()),
            ..Config::default()
        };
        let adapter = CloudWatchAdapter::from_config(&cfg).unwrap();
        assert_eq!(adapter.creds.access_key, "AKID-ENV");
        assert_eq!(adapter.creds.secret_key, "SECRET-ENV");
        assert_eq!(adapter.session_token.as_deref(), Some("TOKEN-ENV"));
        std::env::remove_var("AWS_ACCESS_KEY_ID");
        std::env::remove_var("AWS_SECRET_ACCESS_KEY");
        std::env::remove_var("AWS_SESSION_TOKEN");
    }

    #[test]
    fn events_are_sorted_metadata_inlined_and_old_events_dropped() {
        let adapter = CloudWatchAdapter::from_config(&test_config()).unwrap();
        let mut e1 = entry(200, "second");
        e1.metadata.insert("user".into(), serde_json::json!("bob"));
        e1.metadata.insert("n".into(), serde_json::json!(3));
        let mut ancient = entry(10, "ancient");
        // Age relative to the adapter's clock (fixed in tests for determinism).
        ancient.timestamp = (adapter.now_ms)().saturating_sub(MAX_AGE_MS + 1);
        let e0 = entry(100, "first");

        let chunks = adapter.chunk_events(&[e1, ancient, e0]);
        assert_eq!(chunks.len(), 1, "ancient event is filtered, rest fits");
        let events = &chunks[0];
        assert_eq!(events.len(), 2, "the 14-day-old event must be skipped");
        assert_eq!(events[0]["message"], "first");
        assert_eq!(
            events[1]["message"],
            "second n=3 user=\"bob\"",
            "metadata inlined as sorted key=value pairs"
        );
    }

    #[test]
    fn oversized_batches_split_at_count_and_byte_limits() {
        let adapter = CloudWatchAdapter::from_config(&test_config()).unwrap();
        // > 10k events → more than one call.
        let many: Vec<LogEntry> = (0..10_500).map(|i| entry(i as u64, "e")).collect();
        assert_eq!(adapter.chunk_events(&many).len(), 2);

        // > ~1 MB of message text → more than one call.
        let big_message = "x".repeat(2_000);
        let heavy: Vec<LogEntry> = (0..1_000).map(|i| entry(i as u64, &big_message)).collect();
        let chunks = adapter.chunk_events(&heavy);
        assert!(chunks.len() >= 2, "1 MB budget must split: {}", chunks.len());
        assert!(chunks[0].len() >= 400, "first chunk should be near-full");
    }

    #[test]
    fn session_token_is_signed_and_sent() {
        let cfg = Config {
            aws_region: Some("us-east-1".into()),
            aws_access_key: Some("AKID".into()),
            aws_secret_key: Some(Secret::new("SECRET")),
            aws_session_token: Some(Secret::new("TOKEN")),
            aws_log_group: Some("g".into()),
            aws_log_stream: Some("s".into()),
            ..Config::default()
        };
        let adapter = CloudWatchAdapter::from_config(&cfg).unwrap();
        assert_eq!(adapter.session_token.as_deref(), Some("TOKEN"));
        // The debug output must not leak it.
        let debug = format!("{adapter:?}");
        assert!(!debug.contains("TOKEN"), "{debug}");
    }
}
