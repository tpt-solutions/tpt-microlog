//! Reliability regression tests (todo.md Phase 9): shutdown bounds, retry
//! classification, WAL isolation, queue policies, and filter guarantees.

use std::sync::atomic::{AtomicU32, Ordering as AtomicOrder};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tpt_log_core::batch::CompressedBatch;
use tpt_log_core::{
    Config, LogLevel, LogType, MicroLog, QueueFullPolicy, Transport, TransportError,
};

fn base_config(wal_dir: &std::path::Path) -> Config {
    Config {
        backend: "none".into(),
        wal_dir: wal_dir.to_string_lossy().to_string(),
        flush_interval_ms: 50,
        shutdown_timeout_ms: 500,
        ..Config::default()
    }
}

/// Transport whose `send` stalls ~250ms then fails — simulates a hung backend
/// while staying bounded so the test suite never wedges.
struct SlowTransport {
    started: AtomicU32,
}

impl Transport for SlowTransport {
    fn send(&self, _batch: &CompressedBatch) -> Result<(), TransportError> {
        self.started.fetch_add(1, AtomicOrder::Relaxed);
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(25));
        }
        Err(TransportError::Transient("slow backend".into()))
    }
    fn name(&self) -> &str {
        "slow"
    }
}

/// Transport that fails everything transiently.
#[derive(Default)]
struct DownTransport;

impl Transport for DownTransport {
    fn send(&self, _batch: &CompressedBatch) -> Result<(), TransportError> {
        Err(TransportError::Transient("backend down".into()))
    }
    fn name(&self) -> &str {
        "down"
    }
}

#[test]
fn shutdown_returns_within_timeout_and_spills_to_wal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = base_config(tmp.path());
    cfg.queue_full_policy = QueueFullPolicy::DropNewest;
    cfg.queue_capacity = 4;
    cfg.batch_size = 1;
    cfg.max_retries = 2;
    cfg.retry_base_ms = 10;
    cfg.shutdown_timeout_ms = 400;

    let slow = Arc::new(SlowTransport {
        started: AtomicU32::new(0),
    });
    let log = MicroLog::new(cfg.clone(), slow.clone());

    // Flood the pipeline: the transport hangs, so batches pile up and the
    // queue overflows (drop_newest keeps info() from blocking).
    let start = Instant::now();
    for i in 0..600 {
        log.info(format!("flood {i}"));
    }
    let emit_elapsed = start.elapsed();
    assert!(
        emit_elapsed < Duration::from_secs(5),
        "drop_newest must never block info(): {emit_elapsed:?}"
    );
    assert!(
        log.stats().dropped_entries.load(AtomicOrder::Relaxed) > 0,
        "overflow must be counted"
    );

    let start = Instant::now();
    log.shutdown();
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(3),
        "shutdown exceeded its timeout budget: {elapsed:?}"
    );
    // Either delivered to the hung transport (it eventually errored) or
    // spilled to the WAL — never silently vanished.
    let wal_writes = log.stats().wal_writes.load(AtomicOrder::Relaxed);
    let delivered = log.stats().delivered_batches.load(AtomicOrder::Relaxed);
    assert!(
        wal_writes > 0 || delivered > 0,
        "batches must be spilled or delivered, not lost"
    );
}

#[test]
fn rate_limited_is_retried_and_then_delivered() {
    struct FlakyThenOk {
        calls: AtomicU32,
    }
    impl Transport for FlakyThenOk {
        fn send(&self, _b: &CompressedBatch) -> Result<(), TransportError> {
            let n = self.calls.fetch_add(1, AtomicOrder::Relaxed);
            if n == 0 {
                Err(TransportError::RateLimited {
                    message: "HTTP 429".into(),
                    retry_after_ms: Some(1),
                })
            } else {
                Ok(())
            }
        }
        fn name(&self) -> &str {
            "flaky"
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = base_config(tmp.path());
    cfg.wal_enabled = false;
    cfg.batch_size = 1;
    cfg.retry_base_ms = 1;
    let log = MicroLog::new(
        cfg,
        Arc::new(FlakyThenOk {
            calls: AtomicU32::new(0),
        }),
    );
    log.info("survives a 429");
    // `flush()` waits for delivery including the retry; `shutdown()` alone
    // deliberately abandons backoff and would race the 1 ms Retry-After.
    assert!(log.flush(), "flush must wait for the retried delivery");
    log.shutdown();
    assert!(
        log.stats().delivered_entries.load(AtomicOrder::Relaxed) >= 1,
        "rate-limited batch must be retried, not dropped"
    );
}

#[test]
fn permanent_failure_dead_letters_after_n_replays() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = base_config(tmp.path());
    cfg.service = Some("dead-letter-svc".into());
    cfg.max_retries = 1;
    cfg.retry_base_ms = 1;
    cfg.wal_retry_interval_ms = 30;
    cfg.wal_dead_letter_replays = 2;

    let wal_root = cfg.resolved_wal_dir();
    let ns = tpt_log_core::wal::namespace(
        cfg.service.as_deref(),
        &cfg.backend,
        cfg.endpoint.as_deref(),
    );
    let ns_dir = wal_root.join(&ns);

    let log = MicroLog::new(cfg.clone(), Arc::new(DownTransport));
    log.error("rejected forever");

    // The logger must stay alive: replay passes run on its background
    // worker. Files gain attempt counters per failed pass, then dead-letter.
    let dead_dir = ns_dir.join("dead");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if dead_dir.is_dir() && std::fs::read_dir(&dead_dir).unwrap().count() > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    log.shutdown();
    drop(log);

    // Background replay pass: files gain attempt counters, then move to dead/.
    let dead_dir = ns_dir.join("dead");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if dead_dir.is_dir() && std::fs::read_dir(&dead_dir).unwrap().count() > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let dead_count = std::fs::read_dir(&dead_dir)
        .map(|d| d.flatten().count())
        .unwrap_or(0);
    assert!(
        dead_count > 0,
        "batch must be dead-lettered after N replays"
    );
}

#[test]
fn different_services_never_cross_deliver_wal() {
    let tmp = tempfile::tempdir().unwrap();
    let wal_root = tmp.path().to_path_buf();

    // Service "alpha" fails to deliver → batch lands in its namespace.
    let mut alpha_cfg = base_config(&wal_root);
    alpha_cfg.service = Some("alpha".into());
    alpha_cfg.max_retries = 1;
    alpha_cfg.retry_base_ms = 1;
    let log_a = MicroLog::new(alpha_cfg.clone(), Arc::new(DownTransport));
    log_a.error("alpha-only message");
    log_a.shutdown();
    drop(log_a);

    // Service "beta" (healthy) must not see or deliver alpha's batch.
    let mut beta_cfg = base_config(&wal_root);
    beta_cfg.service = Some("beta".into());
    beta_cfg.max_retries = 1;
    beta_cfg.retry_base_ms = 1;
    let (log_b, spy_b) = MicroLog::with_spy(beta_cfg);
    log_b.info("beta message");
    std::thread::sleep(Duration::from_millis(400));
    log_b.shutdown();

    let beta_entries = spy_b.entries();
    assert!(
        !beta_entries
            .iter()
            .any(|e| e.message == "alpha-only message"),
        "beta must never deliver alpha's WAL batches"
    );

    // Alpha's batch is still on disk, untouched.
    let ns = tpt_log_core::wal::namespace(Some("alpha"), &alpha_cfg.backend, None);
    let alpha_dir = wal_root.join(ns);
    let wal_files: Vec<_> = std::fs::read_dir(&alpha_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "wal"))
        .collect();
    assert!(!wal_files.is_empty(), "alpha's WAL batch must remain");
}

#[test]
fn sampling_and_min_level_leave_metrics_and_errors_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = base_config(tmp.path());
    cfg.wal_enabled = false;
    cfg.sample_rate = 0.0; // would drop everything if applied blindly
    cfg.min_level = Some("DEBUG".into()); // everything passes the level gate
    let (log, spy) = MicroLog::with_spy(cfg);

    log.debug("noise 1"); // sampled away (rate 0)
    log.info("noise 2"); // sampled away (rate 0)
    log.warn("important warning"); // never sampled
    log.error("important error"); // never sampled
    log.metric("cpu", 0.5, None); // exempt from both
    log.span("request", "tid", "sid", None, 12); // exempt from both
    log.shutdown();

    let entries = spy.entries();
    let messages: Vec<&str> = entries.iter().map(|e| e.message.as_str()).collect();
    assert!(!messages.contains(&"noise 1"), "DEBUG below min_level");
    assert!(!messages.contains(&"noise 2"), "INFO below min_level");
    assert!(
        messages.contains(&"important warning"),
        "WARN never sampled"
    );
    assert!(messages.contains(&"important error"), "ERROR never sampled");
    let metric = entries
        .iter()
        .find(|e| e.log_type == LogType::Metric)
        .expect("metric must survive sampling + min level");
    assert_eq!(metric.message, "cpu");
    assert!(
        entries.iter().any(|e| e.log_type == LogType::Trace),
        "span must survive sampling + min level"
    );
}

#[test]
fn wal_first_persists_before_delivery() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = base_config(tmp.path());
    cfg.durability = tpt_log_core::Durability::WalFirst;
    cfg.wal_enabled = true;
    let (log, spy) = MicroLog::with_spy(cfg);
    log.info("durable message");
    log.flush();
    log.shutdown();
    // wal-first: the batch is pre-written to WAL, then delivered + removed.
    assert!(
        log.stats().wal_writes.load(AtomicOrder::Relaxed) >= 1,
        "wal-first must pre-write batches"
    );
    assert!(
        spy.entries().iter().any(|e| e.message == "durable message"),
        "delivery still happens"
    );
}

#[test]
fn flush_delivers_without_shutdown() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = base_config(tmp.path());
    cfg.wal_enabled = false;
    cfg.batch_size = 100_000; // only the flush call would push these out
    cfg.flush_interval_ms = 60_000;
    let (log, spy) = MicroLog::with_spy(cfg);
    log.info("flushed on demand");
    assert!(log.flush(), "flush must drain queue + dispatch");
    assert_eq!(log.queued(), 0);
    assert_eq!(log.inflight(), 0);
    assert!(
        spy.entries()
            .iter()
            .any(|e| e.message == "flushed on demand"),
        "flush must deliver buffered entries"
    );
    log.shutdown();
}

#[test]
fn log_level_parse_roundtrip() {
    // Sanity for the min_level pipeline path.
    assert!("WARN".parse::<LogLevel>().is_ok());
    assert!("garbage".parse::<LogLevel>().is_err());
}

#[test]
fn redaction_masks_data_before_it_reaches_the_transport() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = base_config(tmp.path());
    cfg.wal_enabled = false;
    cfg.redact_keys = vec!["token".into()];
    cfg.redact_builtin = true;
    let (log, spy) = MicroLog::with_spy(cfg);

    log.log_with(
        tpt_log_core::LogLevel::Info,
        "signup by alice@example.com",
        [("token", serde_json::json!("abc123"))],
    );
    log.shutdown();

    let entries = spy.entries();
    let e = entries
        .iter()
        .find(|e| e.message.starts_with("signup"))
        .unwrap();
    assert_eq!(e.message, "signup by [REDACTED]");
    assert_eq!(e.metadata["token"], "[REDACTED]");
}

#[test]
fn shutdown_releases_the_wal_lock_for_a_recreated_logger() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = base_config(tmp.path());
    cfg.wal_enabled = true;
    cfg.service = Some("relock".into());
    let ns = tpt_log_core::wal::namespace(
        cfg.service.as_deref(),
        &cfg.backend,
        cfg.endpoint.as_deref(),
    );
    let dir = tmp.path().join(ns);

    let first = MicroLog::with_default_transport(cfg.clone());
    first.info("one");
    first.shutdown();
    // `first` is still alive here (as in `log = make_logger()` reloads), yet
    // the WAL directory must already be available to a new instance.
    let reopened = tpt_log_core::Wal::open(&dir, 1_000_000);
    assert!(
        reopened.is_ok(),
        "WAL lock still held after shutdown: {:?}",
        reopened.err()
    );
    drop(reopened);
    drop(first);
}
