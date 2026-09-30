//! Prometheus adapter — pull model (spec §5.3).
//!
//! Embeds a tiny HTTP server on `prometheus_bind` and aggregates metric
//! entries (`LogType::Metric`) into Prometheus text format served at `/metrics`.
//!
//! Series are keyed by metric name + label set (same name with different
//! labels are distinct series), label keys are sanitized, label values are
//! escaped, and exactly one `# TYPE` line is emitted per metric name.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tpt_log_core::{CompressedBatch, Config, LogEntry, Transport, TransportError};

use crate::adapters::value_as_number;

/// A series key: sorted `(label key, label value)` pairs.
type LabelSet = Vec<(String, String)>;

/// Aggregates latest gauge values per (metric name, label set).
#[derive(Default)]
struct Registry {
    series: BTreeMap<String, BTreeMap<LabelSet, f64>>,
}

impl Registry {
    fn record(&mut self, entry: &LogEntry) {
        let Some(value) = entry.metadata.get("value").and_then(value_as_number) else {
            return;
        };
        let name = sanitize_name(&entry.message);
        let mut labels: LabelSet = entry
            .metadata
            .get("tags")
            .and_then(|t| t.as_object())
            .map(|obj| {
                obj.iter()
                    .map(|(k, v)| {
                        let value = match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        (sanitize_name(k), escape_label_value(&value))
                    })
                    .collect()
            })
            .unwrap_or_default();
        labels.sort();
        self.series
            .entry(name)
            .or_default()
            .insert(labels, value);
    }

    fn render_metrics(&self) -> Vec<u8> {
        let mut out = String::new();
        for (name, labelsets) in &self.series {
            // One TYPE header per metric name, regardless of label sets.
            out.push_str(&format!("# TYPE {name} gauge\n"));
            for (labels, value) in labelsets {
                if labels.is_empty() {
                    out.push_str(&format!("{name} {value}\n"));
                } else {
                    let rendered: Vec<String> = labels
                        .iter()
                        .map(|(k, v)| format!("{k}=\"{v}\""))
                        .collect();
                    out.push_str(&format!("{name}{{{}}} {value}\n", rendered.join(",")));
                }
            }
        }
        out.into_bytes()
    }
}

/// Replace every character outside `[a-zA-Z0-9_:]` with `_` and prefix a
/// leading digit so the result is a valid Prometheus identifier.
fn sanitize_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == ':' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// Escape a label value per the Prometheus text format
/// (backslash, double quote, and newline).
fn escape_label_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Prometheus pull-model adapter. Serves `/metrics` on a bound TCP listener
/// until [`PrometheusAdapter::stop`] is called (or the adapter is dropped).
pub struct PrometheusAdapter {
    listener: Arc<TcpListener>,
    metrics_url: String,
    registry: Arc<Mutex<Registry>>,
    stop: Arc<AtomicBool>,
    server: Mutex<Option<thread::JoinHandle<()>>>,
}

impl std::fmt::Debug for PrometheusAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrometheusAdapter")
            .field("metrics_url", &self.metrics_url)
            .finish_non_exhaustive()
    }
}

impl PrometheusAdapter {
    /// Bind a pull listener. `bind` is `host:port`; use port `0` to get an
    /// ephemeral port, then call [`Self::metrics_url`].
    pub fn bind(bind: &str) -> Result<Self, TransportError> {
        let listener = TcpListener::bind(bind)
            .map_err(|e| TransportError::Config(format!("cannot bind {bind}: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| TransportError::Config(format!("local_addr failed: {e}")))?;
        let metrics_url = format!("http://{addr}/metrics");
        listener
            .set_nonblocking(true)
            .map_err(|e| TransportError::Config(format!("nonblocking failed: {e}")))?;

        let listener = Arc::new(listener);
        let registry = Arc::new(Mutex::new(Registry::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let server = {
            let listener = listener.clone();
            let registry = registry.clone();
            let stop = stop.clone();
            thread::Builder::new()
                .name("tpt-microlog-prometheus".into())
                .spawn(move || serve(listener, registry, stop))
                .map_err(|e| TransportError::Config(format!("spawn prometheus thread: {e}")))?
        };

        Ok(Self {
            listener,
            metrics_url,
            registry,
            stop,
            server: Mutex::new(Some(server)),
        })
    }

    /// Build from core config (`prometheus_bind`).
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        Self::bind(&config.prometheus_bind)
    }

    /// URL to scrape, e.g. `http://127.0.0.1:9090/metrics`.
    pub fn metrics_url(&self) -> &str {
        &self.metrics_url
    }

    /// Stop the embedded HTTP server (idempotent; also runs on `Drop`).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.server.lock().unwrap_or_else(|e| e.into_inner()).take() {
            // Nudge the nonblocking accept loop awake; it exits within one
            // poll interval of the flag being set.
            let _ = handle.join();
        }
    }
}

impl Drop for PrometheusAdapter {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve(listener: Arc<TcpListener>, registry: Arc<Mutex<Registry>>, stop: Arc<AtomicBool>) {
    let poll = Duration::from_millis(50);
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                handle_connection(&mut stream, &registry);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(poll);
            }
            Err(_) => return,
        }
    }
}

fn handle_connection(stream: &mut std::net::TcpStream, registry: &Arc<Mutex<Registry>>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok();
    let Ok(raw) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(raw);
    let mut request_line = String::new();
    let mut content_length = 0usize;
    let _ = reader.read_line(&mut request_line);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    // Swallow any body bytes (POSTs) without waiting on keep-alive sockets.
    if content_length > 0 {
        let mut rest = vec![0u8; content_length.min(1 << 20)];
        let _ = reader.read_exact(&mut rest);
    }

    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let body = match registry.lock() {
        Ok(reg) if path.starts_with("/metrics") => reg.render_metrics(),
        Ok(_) => b"tpt-microlog prometheus exporter".to_vec(),
        Err(_) => return,
    };
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.write_all(&body);
}

impl Transport for PrometheusAdapter {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        let entries = batch
            .decompress_entries()
            .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;
        let mut reg = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        for e in &entries {
            reg.record(e);
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "prometheus"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_log_core::{LogLevel, LogType};

    #[test]
    fn name_sanitizes_chars() {
        assert_eq!(sanitize_name("cpu.usage"), "cpu_usage");
        assert_eq!(sanitize_name("42users"), "_42users");
        assert_eq!(sanitize_name("http_status"), "http_status");
        assert_eq!(sanitize_name("latency-ms"), "latency_ms");
    }

    #[test]
    fn label_values_are_escaped() {
        assert_eq!(escape_label_value("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(escape_label_value("back\\slash"), "back\\\\slash");
        assert_eq!(escape_label_value("line1\nline2"), "line1\\nline2");
    }

    fn metric_entry(name: &str, value: f64, tags: serde_json::Value) -> LogEntry {
        let mut metadata = serde_json::Map::new();
        metadata.insert("value".into(), serde_json::json!(value));
        if !tags.is_null() {
            metadata.insert("tags".into(), tags);
        }
        LogEntry {
            timestamp: 1,
            level: LogLevel::Info,
            log_type: LogType::Metric,
            message: name.into(),
            metadata,
        }
    }

    #[test]
    fn same_name_different_labels_are_distinct_series() {
        let mut reg = Registry::default();
        reg.record(&metric_entry("cpu", 1.0, serde_json::json!({"host": "a"})));
        reg.record(&metric_entry("cpu", 2.0, serde_json::json!({"host": "b"})));
        reg.record(&metric_entry("cpu", 9.0, serde_json::json!({"host": "a"}))); // overwrite
        let text = String::from_utf8(reg.render_metrics()).unwrap();
        assert_eq!(text.matches("# TYPE cpu gauge").count(), 1, "{text}");
        assert!(text.contains("cpu{host=\"a\"} 9"));
        assert!(text.contains("cpu{host=\"b\"} 2"));
        assert_eq!(text.lines().count(), 3, "{text}");
    }

    #[test]
    fn label_keys_sanitized_and_values_escaped_in_render() {
        let mut reg = Registry::default();
        reg.record(&metric_entry(
            "req",
            3.0,
            serde_json::json!({"path/odd": "a\"b", "ok": "x\ny"}),
        ));
        let text = String::from_utf8(reg.render_metrics()).unwrap();
        assert!(
            text.contains("req{ok=\"x\\ny\",path_odd=\"a\\\"b\"} 3"),
            "{text}"
        );
    }
}
