//! Health snapshot, the `/healthz` HTTP endpoint and self-telemetry metrics.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tpt_log_core::batch::CompressedBatch;
use tpt_log_core::{Config, HealthStatus, LogType, MicroLog, Transport, TransportError};

fn config() -> Config {
    Config {
        backend: "none".into(),
        wal_enabled: false,
        batch_size: 1,
        flush_interval_ms: 20,
        shutdown_timeout_ms: 2_000,
        ..Config::default()
    }
}

fn http_get(addr: std::net::SocketAddr, path: &str) -> (u16, serde_json::Value) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let code: u16 = raw.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("{}");
    (code, serde_json::from_str(body).unwrap())
}

#[test]
fn healthy_logger_reports_ok_with_latency_and_counts() {
    let (log, _spy) = MicroLog::with_spy(config());
    log.info("hello");
    assert!(log.flush());
    let h = log.health();
    assert_eq!(h.status, HealthStatus::Ok);
    assert_eq!(h.delivered_entries, 1);
    assert_eq!(h.wal_files, 0);
    assert!(h.last_success_ms_ago.is_some());
    assert!(h.last_failure_ms_ago.is_none());
    assert_eq!(h.queue_capacity, config().queue_capacity);
    log.shutdown();
    assert_eq!(log.health().status, HealthStatus::Stopped);
}

struct AlwaysFails;
impl Transport for AlwaysFails {
    fn send(&self, _: &CompressedBatch) -> Result<(), TransportError> {
        Err(TransportError::Transient("collector down".into()))
    }
    fn name(&self) -> &str {
        "always-fails"
    }
}

#[test]
fn failing_backend_with_a_wal_backlog_is_down() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = config();
    cfg.wal_enabled = true;
    cfg.wal_dir = tmp.path().to_string_lossy().into_owned();
    cfg.max_retries = 1;
    cfg.retry_base_ms = 1;
    cfg.wal_retry_interval_ms = 60_000;
    let log = MicroLog::new(cfg, Arc::new(AlwaysFails));
    log.error("cannot deliver");
    assert!(log.flush());
    let h = log.health();
    assert_eq!(h.status, HealthStatus::Down, "{h:?}");
    assert!(h.wal_files >= 1 && h.wal_bytes > 0);
    assert!(h.delivery_failures >= 1);
    assert!(h.last_failure_ms_ago.is_some());
    assert_eq!(h.to_json()["status"], "down");
}

#[test]
fn healthz_and_stats_endpoints() {
    let mut cfg = config();
    cfg.health_bind = Some("127.0.0.1:0".into());
    let (log, _spy) = MicroLog::with_spy(cfg);
    log.info("x");
    assert!(log.flush());
    let addr = log.health_addr().expect("endpoint bound");

    let (code, body) = http_get(addr, "/healthz");
    assert_eq!(code, 200);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["delivered_entries"], 1);

    let (code, body) = http_get(addr, "/stats");
    assert_eq!(code, 200);
    assert_eq!(body["counters"]["delivered_entries"], 1);
    assert!(body["counters"].get("sampled_entries").is_some());

    assert_eq!(http_get(addr, "/nope").0, 404);

    log.shutdown();
    let (code, body) = http_get(addr, "/healthz");
    assert_eq!(code, 503, "a stopped logger is not healthy");
    assert_eq!(body["status"], "stopped");
}

#[test]
fn self_metrics_are_emitted_periodically_and_bypass_filters() {
    let mut cfg = config();
    cfg.self_metrics_interval_ms = 50;
    cfg.min_level = Some("FATAL".into());
    cfg.sample_rate = 0.0;
    let (log, spy) = MicroLog::with_spy(cfg);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut names = Vec::new();
    while Instant::now() < deadline {
        names = spy
            .entries()
            .iter()
            .filter(|e| e.log_type == LogType::Metric)
            .map(|e| e.message.clone())
            .collect();
        if names.iter().any(|n| n == "microlog.queue_depth") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    log.shutdown();
    for expected in [
        "microlog.queue_depth",
        "microlog.wal_bytes",
        "microlog.dropped_entries",
        "microlog.delivery_latency_ms",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} missing: {names:?}"
        );
    }
}
