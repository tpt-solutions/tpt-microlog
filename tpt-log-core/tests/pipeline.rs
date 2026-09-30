//! End-to-end pipeline tests for `tpt-log-core` (spec §4.2).

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrder};
use std::sync::Arc;

use tpt_log_core::batch::Batch;
use tpt_log_core::{
    CompressedBatch, Compressor, Config, LogEntry, MicroLog, Transport, TransportError,
};

/// Transport that always fails with a transient error.
#[derive(Default)]
struct FailingTransport {
    attempts: AtomicUsize,
}

impl Transport for FailingTransport {
    fn send(&self, _batch: &CompressedBatch) -> Result<(), TransportError> {
        self.attempts.fetch_add(1, AtomicOrder::Relaxed);
        Err(TransportError::Transient("backend down".into()))
    }
    fn name(&self) -> &str {
        "failing"
    }
}

fn test_config(wal_dir: &Path) -> Config {
    Config {
        backend: "none".into(),
        wal_enabled: true,
        wal_dir: wal_dir.to_string_lossy().to_string(),
        flush_interval_ms: 50,
        ..Config::default()
    }
}

#[test]
fn logs_are_batched_and_delivered() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config(tmp.path());
    cfg.batch_size = 5;
    cfg.wal_enabled = false;
    let (log, spy) = MicroLog::with_spy(cfg);
    for i in 0..12 {
        log.info(format!("message {i}"));
    }
    log.shutdown();

    let entries = spy.entries();
    assert_eq!(entries.len(), 12, "all entries must arrive");
    for i in 0..12 {
        assert!(
            entries.iter().any(|e| e.message == format!("message {i}")),
            "missing message {i}"
        );
    }
}

#[test]
fn time_based_flush_delivers_partial_batch() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config(tmp.path());
    // batch_size huge so only the timer flushes.
    cfg.batch_size = 100_000;
    cfg.flush_interval_ms = 80;
    let (log, spy) = MicroLog::with_spy(cfg);
    log.info("lonely log");
    log.shutdown();

    let entries = spy.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].message, "lonely log");
}

#[test]
fn failed_delivery_lands_in_wal_and_is_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config(tmp.path());
    cfg.max_retries = 1;
    cfg.retry_base_ms = 5;
    cfg.wal_retry_interval_ms = 20;
    // WAL files live in a per-identity namespace directory.
    let wal_dir = cfg.resolved_wal_dir().join(tpt_log_core::wal::namespace(
        cfg.service.as_deref(),
        &cfg.backend,
        cfg.endpoint.as_deref(),
    ));

    let failing = Arc::new(FailingTransport::default());
    let log = MicroLog::new(cfg.clone(), failing.clone());

    log.info("must survive a network outage");
    log.shutdown();
    let stats = log.stats();
    let wal_writes = stats.wal_writes.load(AtomicOrder::SeqCst);
    let delivered = stats.delivered_batches.load(AtomicOrder::SeqCst);
    drop(log); // release the WAL directory lock

    // Batch could not be delivered → must be persisted in the WAL.
    assert_eq!(wal_writes, 1);
    assert_eq!(delivered, 0);

    let files: Vec<_> = std::fs::read_dir(&wal_dir).unwrap().flatten().collect();
    assert!(!files.is_empty(), "WAL must contain the undelivered batch");

    // Re-open a logger pointed at the same WAL with a working transport: the
    // retry thread should drain the WAL and deliver the batch.
    let (log2, spy2) = MicroLog::with_spy(cfg);
    std::thread::sleep(std::time::Duration::from_millis(300));
    log2.shutdown();
    let entries = spy2.entries();
    assert!(
        entries
            .iter()
            .any(|e| e.message == "must survive a network outage"),
        "WAL replay must redeliver the batch"
    );
    let remaining: Vec<_> = std::fs::read_dir(&wal_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_file() && e.path().extension().is_some_and(|x| x == "wal"))
        .collect();
    assert!(remaining.is_empty(), "delivered WAL files must be removed");
}

#[test]
fn fast_mode_skips_compression() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = test_config(tmp.path());
    cfg.fast_mode = true;
    let (log, spy) = MicroLog::with_spy(cfg);
    log.info("uncompressed hello");
    log.shutdown();
    let snap = spy.snapshot();
    assert_eq!(snap.len(), 1);
    assert!(!snap[0].compressed);
    let decoded = snap[0].decode_entries().unwrap();
    assert_eq!(decoded[0].message, "uncompressed hello");
}

#[test]
fn compression_ratio_visible_on_wire() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = test_config(tmp.path());
    let (log, spy) = MicroLog::with_spy(cfg);
    for i in 0..200 {
        log.info(format!(
            "repetitive log line number {i} for a compressible payload yes"
        ));
    }
    log.shutdown();
    let snap = spy.snapshot();
    let raw: Vec<LogEntry> = table_entries(&snap);
    assert_eq!(raw.len(), 200);
    let json_len = Batch { entries: raw }.to_json().len();
    let wire_len: usize = snap.iter().map(|p| p.bytes.len()).sum();
    assert!(
        wire_len < json_len / 2,
        "expected >2x compression, json={json_len} wire={wire_len}"
    );
}

#[test]
fn metadata_roundtrips_through_pipeline() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = test_config(tmp.path());
    let (log, spy) = MicroLog::with_spy(cfg);
    log.log_with(
        tpt_log_core::LogLevel::Error,
        "db down",
        [("retry_count", serde_json::json!(3))],
    );
    log.metric("cpu_usage", 42.5, None);
    log.shutdown();
    let entries = spy.entries();
    let err = entries.iter().find(|e| e.message == "db down").unwrap();
    assert_eq!(err.metadata["retry_count"], serde_json::json!(3));
    let m = entries.iter().find(|e| e.message == "cpu_usage").unwrap();
    assert_eq!(m.metadata["value"], serde_json::json!(42.5));
}

fn table_entries(payloads: &[tpt_log_core::batch::CompressedBatchPayload]) -> Vec<LogEntry> {
    payloads
        .iter()
        .flat_map(|p| p.decode_entries().unwrap_or_default())
        .collect()
}

#[test]
fn compressor_helper_roundtrip() {
    let c = Compressor::new(3, false);
    let b = Batch {
        entries: vec![LogEntry::new(tpt_log_core::LogLevel::Info, "x")],
    };
    let cb = c.compress_batch(&b, 1);
    assert_eq!(cb.decompress_entries().unwrap()[0].message, "x");
}
