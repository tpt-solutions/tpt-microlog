//! End-to-end transport tests against a mock HTTP server (todo.md Phase 2).
//!
//! Uses a plain `TcpListener` so no extra test deps are needed. Retry timing is
//! shrunk via the `Config` knobs the core retry loop consumes.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use tpt_log_core::{Compressor, LogEntry, LogLevel, LogType, MicroLog, Transport};
use tpt_log_transport::{BodyMode, HttpTransport};

/// Minimal mock HTTP server that records request bodies and replays a fixed
/// status from a queue. `behavior` elements are consumed left-to-right and
/// reused when exhausted.
struct MockServer {
    addr: String,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    requests: Arc<AtomicUsize>,
}

impl MockServer {
    fn start(statuses: Vec<u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let bodies: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let bodies_clone = bodies.clone();
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_thread = requests.clone();

        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .ok();
                // Read headers + body (Content-Length framed).
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                let mut request_head = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    request_head.push_str(&line);
                }
                let mut content_length = 0usize;
                for line in request_head.lines() {
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; content_length];
                if content_length > 0 && reader.read_exact(&mut body).is_err() {
                    break;
                }
                bodies_clone.lock().unwrap().push(body);
                let n = requests_thread.fetch_add(1, Ordering::SeqCst);
                let status = statuses
                    .get(n)
                    .copied()
                    .unwrap_or_else(|| statuses.last().copied().unwrap_or(200));
                let reason = if status == 200 { "OK" } else { "Error" };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        MockServer {
            addr,
            bodies,
            requests,
        }
    }

    fn bodies(&self) -> Vec<Vec<u8>> {
        self.bodies.lock().unwrap().clone()
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

/// Counts how many times a wrapped transport's `send` is invoked (distinguishes
/// "core gave up after 1 attempt" from "the server never saw the retries").
struct CountingTransport {
    inner: HttpTransport,
    calls: Arc<AtomicUsize>,
}

impl Transport for CountingTransport {
    fn send(
        &self,
        batch: &tpt_log_core::CompressedBatch,
    ) -> Result<(), tpt_log_core::TransportError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.send(batch)
    }
}

fn sample_entries(n: usize) -> Vec<LogEntry> {
    (0..n)
        .map(|i| LogEntry {
            timestamp: 1_700_000_000_000 + i as u64,
            level: LogLevel::Info,
            log_type: LogType::Log,
            message: format!("hello-{i}"),
            metadata: Default::default(),
        })
        .collect()
}

/// HTTP transport delivers a batch as `Content-Encoding: zstd` bytes that the
/// server can round-trip through the core compressor.
#[test]
fn http_transport_delivers_zstd_batch_end_to_end() {
    let server = MockServer::start(vec![200]);
    let transport = HttpTransport::with_mode(
        &server.addr,
        vec![("X-Test".to_string(), "1".to_string())],
        BodyMode::Zstd,
    );
    let batch = tpt_log_core::Batch {
        entries: sample_entries(4),
    };
    let compressed = Compressor::new(3, false).compress_batch(&batch, 4);
    transport.send(&compressed).unwrap();

    let bodies = server.bodies();
    assert_eq!(bodies.len(), 1);
    let decompressed = Compressor::decompress(&bodies[0]).unwrap();
    let decoded = tpt_log_core::Batch::from_json(&decompressed).unwrap();
    assert_eq!(decoded.entries.len(), 4);
    assert_eq!(decoded.entries[0].message, "hello-0");
}

/// A MicroLog wired to HTTP honors per-config retry: two 500s then a 200, with
/// a slow-fast backoff, delivering exactly once. Backoff is let to complete
/// before `shutdown()` (which aborts in-flight backoffs by design).
#[test]
fn microlog_retries_then_delivers_over_http() {
    let server = MockServer::start(vec![500, 500, 200]);
    let transport = HttpTransport::new(&server.addr, vec![]);

    let cfg = tpt_log_core::Config {
        backend: "http".into(),
        endpoint: Some(server.addr.clone()),
        wal_enabled: false,
        flush_interval_ms: 10,
        retry_base_ms: 5,
        retry_max_backoff_ms: 20,
        max_retries: 5,
        ..tpt_log_core::Config::default()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let counting = CountingTransport {
        inner: transport,
        calls: calls.clone(),
    };
    let log = MicroLog::new(cfg, Arc::new(counting));

    for i in 0..3 {
        log.info(format!("msg-{i}"));
    }
    // Give the flush + retry sequence (2×500 backoff then 200) time to finish.
    thread::sleep(std::time::Duration::from_millis(250));
    log.shutdown();

    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "core must retry the transient 500s"
    );
    assert_eq!(server.request_count(), 3);
    let bodies = server.bodies();
    let decoded = tpt_log_core::Batch::from_json(bodies.last().unwrap()).unwrap();
    assert_eq!(decoded.entries.len(), 3);
}

/// Failure beyond max retries persists to the WAL; a second instance pointed at
/// a healthy server drains it on the next poll (guaranteed delivery round-trip).
#[test]
fn wal_replay_after_permanent_outage() {
    let tmp = tempfile::tempdir().unwrap();
    let wal_dir = tmp.path().join("wal");

    // Instance 1: backend down for the whole run.
    let down = MockServer::start(vec![503]);
    let down_transport = HttpTransport::new(&down.addr, vec![]);
    let cfg1 = tpt_log_core::Config {
        backend: "http".into(),
        endpoint: Some(down.addr),
        wal_enabled: true,
        wal_dir: wal_dir.to_string_lossy().to_string(),
        flush_interval_ms: 5,
        retry_base_ms: 2,
        retry_max_backoff_ms: 5,
        max_retries: 2,
        wal_retry_interval_ms: 20,
        ..tpt_log_core::Config::default()
    };
    let log1 = MicroLog::new(cfg1, Arc::new(down_transport));
    for i in 0..5 {
        log1.info(format!("lost-{i}"));
    }
    log1.shutdown();

    // The batch landed in the WAL.
    let wal = tpt_log_core::wal::Wal::open(&wal_dir, 10 * 1024 * 1024).unwrap();
    assert_eq!(
        wal.len(),
        1,
        "batch must be spooled to WAL after max retries"
    );

    // Instance 2: backend healthy now; drains the WAL within one poll interval.
    let up = MockServer::start(vec![200]);
    let up_transport = HttpTransport::new(&up.addr, vec![]);
    let cfg2 = tpt_log_core::Config {
        backend: "http".into(),
        endpoint: Some(up.addr.clone()),
        wal_enabled: true,
        wal_dir: wal_dir.to_string_lossy().to_string(),
        flush_interval_ms: 5,
        retry_base_ms: 2,
        retry_max_backoff_ms: 5,
        max_retries: 2,
        wal_retry_interval_ms: 20,
        ..tpt_log_core::Config::default()
    };
    let log2 = MicroLog::new(cfg2, Arc::new(up_transport));
    // Give the replay thread a couple of poll intervals to succeed.
    thread::sleep(std::time::Duration::from_millis(120));
    log2.shutdown();

    let mutual = tpt_log_core::wal::Wal::open(&wal_dir, 10 * 1024 * 1024).unwrap();
    assert_eq!(mutual.len(), 0, "WAL must drain after backend recovers");

    // The replayed batch was delivered exactly once to the healthy server.
    // `HttpTransport::new` uses JSON mode, so bodies decode without zstd.
    let decoded_bodies = up
        .bodies()
        .iter()
        .map(|b| tpt_log_core::Batch::from_json(b).unwrap())
        .collect::<Vec<_>>();
    let total_entries: usize = decoded_bodies.iter().map(|b| b.entries.len()).sum();
    assert_eq!(
        total_entries, 5,
        "all five spooled entries replayed to healthy backend"
    );
}
