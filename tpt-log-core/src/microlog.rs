use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use serde_json::{Map, Value};

use crate::batch::{Batch, CompressedBatch};
use crate::config::Config;
use crate::entry::LogEntry;
use crate::filter::EntryProcessor;
use crate::queue::LogQueue;
use crate::retry::{policy_from_config, RetryOutcome, RetryPolicy};
use crate::transport::{NullTransport, SpyTransport, Transport, TransportError};
use crate::wal::Wal;
use crate::{Compressor, LogLevel, LogType};

/// Delivery statistics exposed for diagnostics.
#[derive(Debug, Default)]
pub struct Stats {
    pub delivered_batches: AtomicU64,
    pub delivered_entries: AtomicU64,
    pub wal_writes: AtomicU64,
    pub retried_entries: AtomicU64,
    pub dropped_batches: AtomicU64,
}

/// Backpressure/dispatch channel capacity between the batcher and the single
/// delivery worker.
const DISPATCH_CAPACITY: usize = 512;

struct DeliveryEngine {
    transport: Arc<dyn Transport>,
    retry: RetryPolicy,
    wal: Option<Arc<Wal>>,
    stats: Arc<Stats>,
    rx: Receiver<CompressedBatch>,
    stop_requested: Arc<AtomicBool>,
    tick_ms: u64,
}

impl DeliveryEngine {
    /// Deliver a batch, honoring the stop flag so backoff is aborted quickly
    /// once `shutdown()` is called (`shutdown()` never blocks for a full
    /// backoff sequence).
    fn deliver_one(&self, batch: &CompressedBatch, policy: &RetryPolicy) -> RetryOutcome {
        let mut attempt: u32 = 0;
        loop {
            if self.stop_requested.load(Ordering::Relaxed) && attempt > 0 {
                return RetryOutcome::Exhausted { attempts: attempt };
            }
            match self.transport.send(batch) {
                Ok(()) => {
                    return RetryOutcome::Delivered {
                        attempts: attempt + 1,
                    }
                }
                Err(TransportError::Permanent(_)) | Err(TransportError::Config(_)) => {
                    return RetryOutcome::Permanent;
                }
                Err(_) => {
                    if attempt + 1 >= policy.max_attempts.max(1) {
                        return RetryOutcome::Exhausted {
                            attempts: attempt + 1,
                        };
                    }
                    sleep_interruptible(&self.stop_requested, policy.backoff_ms(attempt));
                    attempt += 1;
                }
            }
        }
    }

    fn dispatch(&self, batch: &CompressedBatch) {
        match self.deliver_one(batch, &self.retry) {
            RetryOutcome::Delivered { .. } => {
                self.stats.delivered_batches.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .delivered_entries
                    .fetch_add(batch.payload.entry_count as u64, Ordering::Relaxed);
            }
            _ => {
                let stored = self
                    .wal
                    .as_ref()
                    .map(|w| w.write(batch).is_ok())
                    .unwrap_or(false);
                if stored {
                    self.stats.wal_writes.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.stats.dropped_batches.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// Replay pending WAL batches. Files are only removed on success; failures
    /// remain on disk for a later pass (spec §4.2 steps 6–8).
    fn replay_pending_wal(&self, policy: &RetryPolicy) {
        let Some(wal) = &self.wal else { return };
        let Ok(pending) = wal.pending() else { return };
        for (path, frame) in pending {
            if frame.bytes.is_empty() {
                let _ = wal.remove(&path);
                continue;
            }
            let batch = frame.to_compressed_batch();
            match self.deliver_one(&batch, policy) {
                RetryOutcome::Delivered { .. } => {
                    self.stats
                        .retried_entries
                        .fetch_add(batch.payload.entry_count as u64, Ordering::Relaxed);
                    let _ = wal.remove(&path);
                }
                _ => { /* leave on disk for next pass */ }
            }
        }
    }

    /// Tunable replay policy: bounded backoff while running, single attempt
    /// during shutdown.
    fn replay_policy(&self) -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_ms: self.retry.base_ms,
            max_backoff_ms: self.retry.max_backoff_ms,
        }
    }

    fn run(&self) {
        let tick = std::time::Duration::from_millis(self.tick_ms);
        let poll = std::time::Duration::from_millis(50);
        let mut last_replay = std::time::Instant::now();
        loop {
            if self.stop_requested.load(Ordering::Relaxed) && self.rx.is_empty() {
                // Drains the WAL once (single attempt) before exiting.
                self.replay_pending_wal(&single_attempt_policy());
                return;
            }
            match self.rx.recv_timeout(poll) {
                Ok(batch) => self.dispatch(&batch),
                Err(RecvTimeoutError::Timeout) => {
                    if !self.stop_requested.load(Ordering::Relaxed) && last_replay.elapsed() >= tick
                    {
                        self.replay_pending_wal(&self.replay_policy());
                        // Re-arm the replay timer after the pass completes (the
                        // pass itself may block on backoff).
                        last_replay = std::time::Instant::now();
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // All senders dropped: begin graceful stop.
                    self.stop_requested.store(true, Ordering::Relaxed);
                }
            }
        }
    }
}

/// A policy that attempts just once (used for shutdown drains).
fn single_attempt_policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 1,
        base_ms: 1,
        max_backoff_ms: 1,
    }
}

/// Sleep in 50ms slices so a shutdown request can interrupt backoff.
fn sleep_interruptible(stop: &AtomicBool, ms: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Coordinates worker-thread lifecycle so `shutdown()` can join them in the
/// right order: the batcher must finish flushing before the delivery worker
/// is told to stop, otherwise the final batch could be abandoned mid-drain.
#[derive(Default)]
struct Sentinel {
    batcher: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    delivery: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Sentinel {
    fn track_batcher(&self, handle: std::thread::JoinHandle<()>) {
        *self.batcher.lock().unwrap() = Some(handle);
    }
    fn track_delivery(&self, handle: std::thread::JoinHandle<()>) {
        *self.delivery.lock().unwrap() = Some(handle);
    }
    fn join_batcher(&self) {
        if let Some(h) = self.batcher.lock().unwrap().take() {
            let _ = h.join();
        }
    }
    fn join_delivery(&self) {
        if let Some(h) = self.delivery.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

/// The public entry point (spec §5, §6).
///
/// ```no_run
/// use tpt_log_core::{MicroLog, Config};
/// let cfg = Config::from_toml_str(r#"backend="none""#).unwrap();
/// let log = MicroLog::with_default_transport(cfg);
/// log.info("user logged in");
/// log.metric("cpu", 0.5, None);
/// log.shutdown();
/// ```
pub struct MicroLog {
    inner: Arc<MicroLogInner>,
}

struct MicroLogInner {
    config: Config,
    queue: LogQueue,
    processor: EntryProcessor,
    compressor: Compressor,
    delivery_tx: Sender<CompressedBatch>,
    /// Stop signal for the batcher thread (rendezvous channel).
    batcher_stop_tx: Sender<()>,
    batcher_stop_rx: Receiver<()>,
    engine: Arc<DeliveryEngine>,
    sentinel: Sentinel,
    stop_requested: Arc<AtomicBool>,
    shutdown_done: AtomicBool,
    stats: Arc<Stats>,
}

impl MicroLog {
    /// Build a logger with an explicit transport.
    pub fn new(config: Config, transport: Arc<dyn Transport>) -> MicroLog {
        let (delivery_tx, delivery_rx) = crossbeam_channel::bounded(DISPATCH_CAPACITY);

        let queue_capacity = config.queue_capacity;
        let wal_enabled = config.wal_enabled;
        let wal_retry_interval = config.wal_retry_interval_ms;
        let config_for_compressor = config.clone();
        let config_for_processor = config.clone();

        let wal = if wal_enabled {
            Wal::open(config.resolved_wal_dir(), config.wal_max_size_bytes)
                .ok()
                .map(Arc::new)
        } else {
            None
        };

        let stats = Arc::new(Stats::default());
        let stop_requested = Arc::new(AtomicBool::new(false));

        let engine = Arc::new(DeliveryEngine {
            transport,
            retry: policy_from_config(&config),
            wal,
            stats: stats.clone(),
            rx: delivery_rx,
            stop_requested: stop_requested.clone(),
            tick_ms: wal_retry_interval,
        });

        let processor = EntryProcessor {
            min_level: config_for_processor
                .min_level
                .as_ref()
                .and_then(|l| l.parse().ok()),
            drop_tags: parse_filter_tags(&config_for_processor.filter_tags),
            drop_regex: config_for_processor
                .filter_regex
                .as_deref()
                .and_then(|pattern| {
                    let compiled = regex::Regex::new(pattern);
                    if let Err(e) = &compiled {
                        eprintln!("tpt-microlog: ignoring invalid filter_regex: {e}");
                    }
                    compiled.ok()
                }),
            sample_rate: config_for_processor.sample_rate,
            enrichment: if config_for_processor.enrichment {
                let (host, pid) = config_for_processor.host_and_process();
                let service = config_for_processor
                    .service
                    .clone()
                    .unwrap_or_else(|| host.clone());
                Some(crate::filter::EnrichmentContext { host, pid, service })
            } else {
                None
            },
        };

        let (batcher_stop_tx, batcher_stop_rx) = crossbeam_channel::bounded(0);

        let inner = Arc::new(MicroLogInner {
            sentinel: Sentinel::default(),
            config,
            queue: LogQueue::new(queue_capacity),
            processor,
            compressor: Compressor::from_config(&config_for_compressor),
            delivery_tx,
            batcher_stop_tx,
            batcher_stop_rx,
            engine: engine.clone(),
            stop_requested,
            shutdown_done: AtomicBool::new(false),
            stats,
        });

        // Delivery worker.
        let engine_arc = inner.engine.clone();
        let handle = std::thread::Builder::new()
            .name("tpt-microlog-delivery".into())
            .spawn(move || engine_arc.run())
            .expect("spawn delivery thread");
        inner.sentinel.track_delivery(handle);

        // Batcher thread.
        let inner2 = inner.clone();
        let handle = std::thread::Builder::new()
            .name("tpt-microlog-batcher".into())
            .spawn(move || run_batcher(inner2))
            .expect("spawn batcher thread");
        inner.sentinel.track_batcher(handle);

        MicroLog { inner }
    }

    /// Build a logger with "none" backend (in-memory discard).
    pub fn with_default_transport(config: Config) -> MicroLog {
        let t: Arc<dyn Transport> = Arc::new(NullTransport);
        MicroLog::new(config, t)
    }

    /// Build a logger that records all delivered payloads (test vehicle).
    pub fn with_spy(config: Config) -> (MicroLog, Arc<SpyTransport>) {
        let spy = Arc::new(SpyTransport::default());
        let log = MicroLog::new(config, spy.clone());
        (log, spy)
    }

    /// Access the configuration.
    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    /// Current delivery statistics.
    pub fn stats(&self) -> &Stats {
        &self.inner.stats
    }

    /// Number of entries still buffered in the ingestion queue.
    pub fn queued(&self) -> usize {
        self.inner.queue.len()
    }

    fn emit(&self, entry: LogEntry) {
        if let Some(processed) = self.inner.processor.process(&entry) {
            self.inner.queue.push(processed);
        }
    }

    /// Log a TRACE message.
    pub fn trace(&self, message: impl Into<String>) {
        self.emit(LogEntry::new(LogLevel::Trace, message));
    }
    /// Log a DEBUG message.
    pub fn debug(&self, message: impl Into<String>) {
        self.emit(LogEntry::new(LogLevel::Debug, message));
    }
    /// Log an INFO message.
    pub fn info(&self, message: impl Into<String>) {
        self.emit(LogEntry::new(LogLevel::Info, message));
    }
    /// Log a WARN message.
    pub fn warn(&self, message: impl Into<String>) {
        self.emit(LogEntry::new(LogLevel::Warn, message));
    }
    /// Log an ERROR message.
    pub fn error(&self, message: impl Into<String>) {
        self.emit(LogEntry::new(LogLevel::Error, message));
    }
    /// Log a FATAL message.
    pub fn fatal(&self, message: impl Into<String>) {
        self.emit(LogEntry::new(LogLevel::Fatal, message));
    }

    /// Log with explicit level and structured metadata pairs.
    pub fn log_with(
        &self,
        level: LogLevel,
        message: impl Into<String>,
        metadata: impl IntoIterator<Item = (impl Into<String>, Value)>,
    ) {
        let mut entry = LogEntry::new(level, message);
        for (k, v) in metadata {
            entry.metadata.insert(k.into(), v);
        }
        self.emit(entry);
    }

    /// Log a numeric metric (spec §5.1).
    pub fn metric(&self, name: impl Into<String>, value: f64, tags: Option<&Map<String, Value>>) {
        let entry = LogEntry::metric(name, value, tags);
        self.emit(entry);
    }

    /// Record a distributed-trace span (spec §5.1, Phase 8). Metadata carries
    /// `trace_id`, `span_id`, `parent_span_id` and `duration_us`.
    pub fn span(
        &self,
        name: impl Into<String>,
        trace_id: impl Into<String>,
        span_id: impl Into<String>,
        parent_span_id: Option<String>,
        duration_us: u64,
    ) {
        let mut entry = LogEntry::new(LogLevel::Info, name);
        entry.log_type = LogType::Trace;
        entry.metadata.insert(
            "span".to_string(),
            serde_json::json!({
                "trace_id": trace_id.into(),
                "span_id": span_id.into(),
                "parent_span_id": parent_span_id,
                "duration_us": duration_us,
            }),
        );
        self.emit(entry);
    }

    /// Graceful shutdown: stops the batcher, flushes the final batch, drains
    /// the delivery channel, then replays the WAL once before worker threads
    /// exit (spec §2.7). Idempotent.
    pub fn shutdown(&self) {
        if self.inner.shutdown_done.swap(true, Ordering::SeqCst) {
            return;
        }
        // 1. Ask the batcher to drain its queue, flush the final batch, and
        //    exit (rendezvous on the stop channel).
        let _ = self
            .inner
            .batcher_stop_tx
            .send_timeout((), std::time::Duration::from_secs(30));
        // 2. Wait until the final batch is buffered in the delivery channel.
        //    Only then flag the worker to stop, so it can never observe
        //    `stop_requested && rx.is_empty()` before the final batch lands.
        self.inner.sentinel.join_batcher();
        self.inner.stop_requested.store(true, Ordering::SeqCst);
        // 3. Join the delivery worker (drains channel + replays WAL once).
        self.inner.sentinel.join_delivery();
    }
}

/// Parse `tag=value` filter pairs; values decode as JSON when they parse.
fn parse_filter_tags(raw: &[String]) -> Vec<(String, serde_json::Value)> {
    raw.iter()
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            let value = serde_json::from_str(value.trim())
                .unwrap_or_else(|_| serde_json::Value::String(value.trim().to_string()));
            Some((key.trim().to_string(), value))
        })
        .collect()
}

/// Background batching thread (spec §5.2): flushes on time (1s), size (100),
/// or memory (10MB) — whichever comes first (spec §4.2 step 3).
fn run_batcher(inner: Arc<MicroLogInner>) {
    let rx = inner.queue.receiver();
    let flush_interval = std::time::Duration::from_millis(inner.config.flush_interval_ms);
    let ticker = crossbeam_channel::tick(flush_interval);
    let mut pending: Vec<Arc<LogEntry>> = Vec::new();
    let mut pending_bytes: usize = 0;

    loop {
        crossbeam_channel::select! {
            recv(rx) -> recv => match recv {
                Ok(entry) => {
                    pending_bytes += entry.approx_size();
                    pending.push(entry);
                    if pending.len() >= inner.config.batch_size
                        || pending_bytes >= inner.config.memory_limit_bytes
                    {
                        flush_batch(&inner, &mut pending, &mut pending_bytes);
                    }
                }
                Err(_) => {
                    // Queue closed (not reachable in the current design):
                    // drain what we can, flush, and exit.
                    while let Ok(entry) = rx.recv_timeout(std::time::Duration::from_millis(5)) {
                        pending_bytes += entry.approx_size();
                        pending.push(entry);
                    }
                    flush_batch(&inner, &mut pending, &mut pending_bytes);
                    return;
                }
            },
            recv(ticker) -> _ => {
                flush_batch(&inner, &mut pending, &mut pending_bytes);
            },
            recv(inner.batcher_stop_rx) -> _ => {
                // Graceful stop: keep pulling entries that arrive during the
                // drain so nothing buffered in the queue is lost.
                while let Ok(entry) = rx.recv_timeout(std::time::Duration::from_millis(5)) {
                    pending_bytes += entry.approx_size();
                    pending.push(entry);
                }
                flush_batch(&inner, &mut pending, &mut pending_bytes);
                return;
            }
        }
    }
}

/// Compress + dispatch a pending batch; resets accumulation.
fn flush_batch(inner: &MicroLogInner, pending: &mut Vec<Arc<LogEntry>>, pending_bytes: &mut usize) {
    if pending.is_empty() {
        return;
    }
    let entries: Vec<LogEntry> = pending.drain(..).map(|a| (*a).clone()).collect();
    *pending_bytes = 0;
    let count = entries.len();
    let batch = Batch { entries };
    let cb = inner.compressor.compress_batch(&batch, count);
    if inner.delivery_tx.send(cb).is_err() {
        inner.stats.dropped_batches.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for MicroLog {
    fn drop(&mut self) {
        self.shutdown();
    }
}
