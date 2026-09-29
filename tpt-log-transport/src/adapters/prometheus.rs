//! Prometheus adapter — pull model (spec §5.3).
//!
//! Embeds a tiny HTTP server on `prometheus_bind` and aggregates metric
//! entries (`LogType::Metric`) into Prometheus text format served at `/metrics`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use tpt_log_core::CompressedBatch;
use tpt_log_core::{Config, LogEntry, Transport, TransportError};

use crate::adapters::value_as_number;

/// One recorded gauge sample.
#[derive(Clone, Debug)]
struct Sample {
    name: String,
    value: f64,
    labels: Vec<(String, String)>,
}

impl Sample {
    /// Prometheus text line: `name{labels} value`.
    fn render(&self) -> String {
        let name = sanitize_name(&self.name);
        if self.labels.is_empty() {
            format!("{name} {}", self.value)
        } else {
            let labels: Vec<String> = self
                .labels
                .iter()
                .map(|(k, v)| format!("{k}=\"{v}\""))
                .collect();
            format!("{name}{{{}}} {}", labels.join(","), self.value)
        }
    }
}

fn sanitize_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 5);
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

/// Aggregates latest gauge values per metric name (last-write-wins).
#[derive(Default)]
struct Registry {
    samples: Vec<Sample>,
}

impl Registry {
    fn record(&mut self, entry: &LogEntry) {
        let Some(value) = entry.metadata.get("value").and_then(value_as_number) else {
            return;
        };
        let labels: Vec<(String, String)> = entry
            .metadata
            .get("tags")
            .and_then(|t| t.as_object())
            .map(|obj| {
                obj.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().map(String::from).unwrap_or_default()))
                    .collect()
            })
            .unwrap_or_default();
        self.samples.retain(|s| s.name != entry.message);
        self.samples.push(Sample {
            name: entry.message.clone(),
            value,
            labels,
        });
    }

    fn render_metrics(&self) -> Vec<u8> {
        let mut out = String::new();
        for s in &self.samples {
            out.push_str(&format!(
                "# TYPE {} gauge\n{}\n",
                sanitize_name(&s.name),
                s.render()
            ));
        }
        out.into_bytes()
    }
}

/// Prometheus pull-model adapter. Serves `/metrics` on a bound TCP listener.
pub struct PrometheusAdapter {
    _listener: Arc<Mutex<TcpListener>>,
    metrics_url: String,
    registry: Arc<Mutex<Registry>>,
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

        let listener = Arc::new(Mutex::new(listener));
        let registry = Arc::new(Mutex::new(Registry::default()));
        let listener_thread = listener.clone();
        let registry_thread = registry.clone();
        thread::Builder::new()
            .name("tpt-microlog-prometheus".into())
            .spawn(move || serve(listener_thread, registry_thread))
            .expect("spawn prometheus listener");

        Ok(Self {
            _listener: listener,
            metrics_url,
            registry,
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
}

fn serve(listener: Arc<Mutex<TcpListener>>, registry: Arc<Mutex<Registry>>) {
    loop {
        let (mut stream, _) = match listener.lock().unwrap().accept() {
            Ok(pair) => pair,
            Err(_) => return,
        };
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .ok();
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
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
            let mut rest = vec![0u8; content_length];
            let _ = reader.read_exact(&mut rest);
        }

        let path = request_line.split_whitespace().nth(1).unwrap_or("/");
        let body = if path.starts_with("/metrics") {
            registry.lock().unwrap().render_metrics()
        } else {
            b"tpt-microlog prometheus exporter".to_vec()
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.write_all(&body);
    }
}

impl Transport for PrometheusAdapter {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        let entries = batch
            .decompress_entries()
            .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;
        let mut reg = self.registry.lock().unwrap();
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

    #[test]
    fn name_sanitizes_chars() {
        assert_eq!(sanitize_name("cpu.usage"), "cpu_usage");
        assert_eq!(sanitize_name("42users"), "_42users");
        assert_eq!(sanitize_name("http_status"), "http_status");
    }
}
