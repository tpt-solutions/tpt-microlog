//! Backend adapter end-to-end tests against mocked HTTP endpoints (Phase 3).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use tpt_log_core::batch::CompressedBatchPayload;
use tpt_log_core::{CompressedBatch, Config, MicroLog, Transport};
use tpt_log_transport::adapters::prometheus::PrometheusAdapter;
use tpt_log_transport::build_transport;

/// Mock server recording full request heads (headers) and bodies, replaying a
/// fixed 200.
struct Mock {
    addr: String,
    heads: Arc<Mutex<Vec<String>>>,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    count: Arc<AtomicUsize>,
}

impl Mock {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let heads: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let bodies: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let (ht, bt, ct) = (heads.clone(), bodies.clone(), count.clone());

        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .ok();
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
                let mut head = String::new();
                let mut final_request_line = String::new();
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                    if final_request_line.is_empty() {
                        final_request_line = line.clone();
                    }
                    head.push_str(&line);
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; content_length];
                if content_length > 0 && reader.read_exact(&mut body).is_err() {
                    break;
                }
                ht.lock().unwrap().push(head);
                bt.lock().unwrap().push(body);
                ct.fetch_add(1, Ordering::SeqCst);
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(resp.as_bytes());
                let _ = final_request_line;
            }
        });

        Mock {
            addr,
            heads,
            bodies,
            count,
        }
    }

    fn head(&self) -> String {
        self.heads
            .lock()
            .unwrap()
            .first()
            .cloned()
            .unwrap_or_default()
            .to_ascii_lowercase()
    }

    fn body(&self) -> Option<Vec<u8>> {
        self.bodies.lock().unwrap().first().cloned()
    }

    fn request_count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

fn slim_config(backend: &str, endpoint: &str) -> Config {
    Config {
        backend: backend.into(),
        endpoint: Some(endpoint.into()),
        flush_interval_ms: 10,
        ..Config::default()
    }
}

/// Datadog adapter POSTs DD Logs v2 JSON with the DD-API-KEY header.
#[test]
fn datadog_adapter_ships_v2_json_with_key_header() {
    let mock = Mock::start();
    let cfg = Config {
        api_key: Some("tops3cret".into()),
        ..slim_config("datadog", &mock.addr)
    };
    let transport = build_transport(&cfg).unwrap();
    let log = MicroLog::new(cfg, Arc::from(transport));
    log.error("disk full");
    log.error("oops");
    log.shutdown();

    assert_eq!(mock.request_count(), 1);
    assert!(mock.head().contains("dd-api-key: tops3cret"));
    let value: serde_json::Value = serde_json::from_slice(&mock.body().unwrap()).unwrap();
    let events = value.as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["status"], "error");
    assert_eq!(events[0]["ddsource"], "tpt-microlog");
    assert!(events[0]["timestamp"].is_number());
}

/// Elasticsearch adapter POSTs NDJSON Bulk API actions.
#[test]
fn elasticsearch_adapter_ships_ndjson_bulk() {
    let mock = Mock::start();
    let cfg = slim_config("elasticsearch", &mock.addr);
    let transport = build_transport(&cfg).unwrap();
    let log = MicroLog::new(cfg, Arc::from(transport));
    log.info("ready");
    log.warn("slow");
    log.shutdown();

    assert_eq!(mock.request_count(), 1);
    assert!(mock.head().contains("application/x-ndjson"));
    let body = String::from_utf8(mock.body().unwrap()).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 4, "2 actions + 2 docs");
    assert!(lines[0].contains("\"index\""));
    assert!(lines[1].contains("\"message\":\"ready\""));
    assert!(lines[2].contains("\"index\""));
}

/// CloudWatch adapter POSTs PutLogEvents payload with a SigV4 Authorization.
#[test]
fn cloudwatch_adapter_signs_put_log_events() {
    let mock = Mock::start();
    let cfg = Config {
        aws_region: Some("eu-central-1".into()),
        aws_access_key: Some("AKIDTEST".into()),
        aws_secret_key: Some(tpt_log_core::Secret::new("SECRETTEST")),
        aws_log_group: Some("my-group".into()),
        aws_log_stream: Some("my-stream".into()),
        ..slim_config("cloudwatch", &mock.addr)
    };
    let transport = build_transport(&cfg).unwrap();
    let log = MicroLog::new(cfg, Arc::from(transport));
    log.info("hello aws");
    log.shutdown();

    assert_eq!(mock.request_count(), 1);
    let head = mock.head();
    assert!(head.contains("x-amz-target: logs_20140328.putlogevents"));
    assert!(head.contains("authorization: aws4-hmac-sha256 credential=akidtest/"));
    assert!(head.contains("/eu-central-1/logs/aws4_request"));
    assert!(head.contains("x-amz-date:"));

    let value: serde_json::Value = serde_json::from_slice(&mock.body().unwrap()).unwrap();
    assert_eq!(value["logGroupName"], "my-group");
    assert_eq!(value["logStreamName"], "my-stream");
    assert_eq!(value["logEvents"][0]["message"], "hello aws");
}

/// The generic HTTP backend posts plain JSON by default (no content-encoding);
/// zstd bodies are opt-in via `body_mode`.
#[test]
fn http_backend_posts_plain_json_by_default_and_zstd_optin() {
    let mock = Mock::start();
    let cfg = slim_config("http", &mock.addr);
    let transport = build_transport(&cfg).unwrap();
    let log = MicroLog::new(cfg, Arc::from(transport));
    log.info("plain json");
    log.shutdown();

    assert!(!mock.head().contains("content-encoding"), "default is JSON");
    let value: serde_json::Value = serde_json::from_slice(&mock.body().unwrap()).unwrap();
    assert_eq!(value["entries"][0]["message"], "plain json");

    let mock2 = Mock::start();
    let cfg2 = Config {
        body_mode: tpt_log_core::BodyMode::Zstd,
        ..slim_config("http", &mock2.addr)
    };
    let transport2 = build_transport(&cfg2).unwrap();
    let log2 = MicroLog::new(cfg2, Arc::from(transport2));
    log2.info("compressed");
    log2.shutdown();
    assert!(mock2.head().contains("content-encoding: zstd"));
}

/// Datadog requests split at the 1000-event intake cap.
#[test]
fn datadog_splits_requests_at_event_cap() {
    use tpt_log_transport::adapters::datadog::DatadogAdapter;

    let mock = Mock::start();
    let cfg = Config {
        api_key: Some(tpt_log_core::Secret::new("k")),
        ..slim_config("datadog", &mock.addr)
    };
    let adapter = DatadogAdapter::from_config(&cfg).unwrap();
    let entries: Vec<tpt_log_core::LogEntry> = (0..1_500u64)
        .map(|i| tpt_log_core::LogEntry {
            timestamp: 1_700_000_000_000 + i,
            level: tpt_log_core::LogLevel::Info,
            log_type: tpt_log_core::LogType::Log,
            message: format!("split-{i}"),
            metadata: Default::default(),
        })
        .collect();
    let batch = tpt_log_core::Compressor::new(0, true)
        .compress_batch(&tpt_log_core::Batch { entries }, 1_500);
    adapter.send(&batch).unwrap();

    assert_eq!(mock.request_count(), 2, "1500 events must split into 2");
    let bodies = mock.bodies.lock().unwrap();
    let first: serde_json::Value = serde_json::from_slice(&bodies[0]).unwrap();
    let second: serde_json::Value = serde_json::from_slice(&bodies[1]).unwrap();
    assert_eq!(first.as_array().unwrap().len(), 1000);
    assert_eq!(second.as_array().unwrap().len(), 500);
}

/// Elasticsearch per-item bulk errors surface even on HTTP 200; item 429 is
/// transient (so the core retries it) while 400 is permanent.
#[test]
fn elasticsearch_bulk_item_errors_are_classified() {
    use tpt_log_transport::adapters::elasticsearch::ElasticsearchAdapter;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let body: &'static [u8] = br#"{"errors":true,"items":[{"index":{"status":429,"error":{"type":"es_rejected_execution_exception","reason":"too many requests"}}}]}"#;
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            use std::io::Write as _;
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(body);
        }
    });

    let cfg = slim_config("elasticsearch", &addr);
    let adapter = ElasticsearchAdapter::from_config(&cfg).unwrap();
    let entries = vec![tpt_log_core::LogEntry::new(
        tpt_log_core::LogLevel::Info,
        "rejected",
    )];
    let batch = tpt_log_core::Compressor::new(0, true).compress_batch(
        &tpt_log_core::Batch {
            entries: entries.clone(),
        },
        1,
    );
    let err = adapter.send(&batch).unwrap_err();
    assert!(
        matches!(err, tpt_log_core::TransportError::RateLimited { .. }),
        "item 429 must be transient: {err}"
    );
}

/// Prometheus adapter serves `log.metric(...)` samples on `/metrics`.
#[test]
fn prometheus_adapter_serves_metrics_via_pull() {
    let adapter = PrometheusAdapter::bind("127.0.0.1:0").unwrap();
    let url = adapter.metrics_url().to_string();

    let entries = vec![
        tpt_log_core::LogEntry {
            timestamp: 1,
            level: tpt_log_core::LogLevel::Info,
            log_type: tpt_log_core::LogType::Metric,
            message: "cpu.usage".into(),
            metadata: serde_json::Map::new(),
        },
        tpt_log_core::LogEntry {
            timestamp: 2,
            level: tpt_log_core::LogLevel::Info,
            log_type: tpt_log_core::LogType::Metric,
            message: "cpu.usage".into(),
            metadata: {
                let mut m = serde_json::Map::new();
                m.insert("value".into(), serde_json::Value::from(42.5));
                m.insert("tags".into(), serde_json::json!({ "host": "box-9" }));
                m
            },
        },
    ];
    let batch = CompressedBatch {
        payload: CompressedBatchPayload {
            bytes: serde_json::to_vec(&tpt_log_core::Batch { entries }).unwrap(),
            entry_count: 2,
            compressed: false,
            raw_json: None,
        },
    };
    adapter.send(&batch).unwrap();

    let resp = reqwest::blocking::get(&url).unwrap();
    assert!(resp.status().is_success());
    let text = resp.text().unwrap();
    assert!(text.contains("# TYPE cpu_usage gauge"), "scraped: {text}");
    assert!(
        text.contains("cpu_usage{host=\"box-9\"} 42.5"),
        "scraped: {text}"
    );
}
