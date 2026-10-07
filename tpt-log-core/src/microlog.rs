use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use parking_lot::Mutex;
use serde_json::{Map, Value};

use crate::batch::{Batch, CompressedBatch};
use crate::config::{Config, Durability};
use crate::entry::LogEntry;
use crate::filter::EntryProcessor;
use crate::health::Health;
use crate::queue::{LogQueue, PushOutcome};
use crate::retry::{policy_from_config, retry_after_hint, RetryOutcome, RetryPolicy};
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
    /// Entries rejected by the queue overflow policy (`drop_newest` /
    /// `drop_oldest`) or lost when the WAL was unavailable at shutdown.
    pub dropped_entries: AtomicU64,
    /// Entries removed by sampling (static, adaptive or daily budget).
    pub sampled_entries: AtomicU64,
    /// Entries removed by `rate_limit_per_sec`.
    pub rate_limited_entries: AtomicU64,
    /// Entries folded into an earlier identical one (`dedup_window_ms`).
    pub deduplicated_entries: AtomicU64,
    /// Entries parked in the flight recorder (shipped only if an error follows).
    pub recorded_entries: AtomicU64,
    /// Batches given up on (permanent error or retries exhausted).
    pub delivery_failures: AtomicU64,
    /// Unix-ms time of the last successful / failed delivery attempt (0 = never).
    pub last_success_ms: AtomicU64,
    pub last_failure_ms: AtomicU64,
    /// Latency of the last successful send, and the running sum/count for the mean.
    pub last_latency_ms: AtomicU64,
    pub latency_sum_ms: AtomicU64,
    pub latency_count: AtomicU64,
}

impl Stats {
    /// Every counter as `(name, value)`: the single source for the bindings'
    /// `stats()` and the health endpoint.
    pub fn counters(&self) -> Vec<(&'static str, u64)> {
        let r = Ordering::Relaxed;
        vec![
            ("delivered_batches", self.delivered_batches.load(r)),
            ("delivered_entries", self.delivered_entries.load(r)),
            ("wal_writes", self.wal_writes.load(r)),
            ("retried_entries", self.retried_entries.load(r)),
            ("dropped_batches", self.dropped_batches.load(r)),
            ("dropped_entries", self.dropped_entries.load(r)),
            ("sampled_entries", self.sampled_entries.load(r)),
            ("rate_limited_entries", self.rate_limited_entries.load(r)),
            ("deduplicated_entries", self.deduplicated_entries.load(r)),
            ("recorded_entries", self.recorded_entries.load(r)),
            ("delivery_failures", self.delivery_failures.load(r)),
            ("last_latency_ms", self.last_latency_ms.load(r)),
        ]
    }
}

/// Backpressure/dispatch channel capacity between the batcher and the single
/// delivery worker.
const DISPATCH_CAPACITY: usize = 512;

/// How many times a worker thread may panic before it is left dead.
const MAX_WORKER_RESTARTS: u32 = 5;

/// Guard marking a batch as being delivered (increments on entry, decrements
/// on drop) so `flush()` can wait for true delivery rather than hand-off.
struct BusyGuard<'a>(&'a AtomicU64);

impl BusyGuard<'_> {
    fn enter(counter: &AtomicU64) -> BusyGuard<'_> {
        counter.fetch_add(1, Ordering::AcqRel);
        BusyGuard(counter)
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct DeliveryEngine {
    transport: Arc<dyn Transport>,
    retry: RetryPolicy,
    wal: Option<Arc<Wal>>,
    durability: Durability,
    dead_letter_replays: u32,
    retry_after_max_ms: u64,
    stats: Arc<Stats>,
    rx: Receiver<CompressedBatch>,
    /// Number of batches currently being handed to the transport. `flush()`
    /// waits for this to reach zero so "flushed" means "delivered or
    /// persisted", not merely "handed off".
    busy: AtomicU64,
    stop_requested: Arc<AtomicBool>,
    tick_ms: u64,
}

impl DeliveryEngine {
    /// Number of batches waiting in the dispatch channel.
    fn inflight(&self) -> usize {
        self.rx.len()
    }

    /// No batch is queued or in the middle of a delivery attempt.
    fn idle(&self) -> bool {
        self.rx.is_empty() && self.busy.load(Ordering::Acquire) == 0
    }

    /// Deliver a batch with backoff + jitter, honoring the stop flag so
    /// `shutdown()` never blocks for a full backoff sequence, and honoring
    /// `Retry-After` hints (capped by `retry_after_max_ms`).
    fn deliver_one(&self, batch: &CompressedBatch, policy: &RetryPolicy) -> RetryOutcome {
        let mut failures: u32 = 0;
        let attempts_allowed = policy.max_attempts.max(1);
        loop {
            let started = Instant::now();
            match self.transport.send(batch) {
                Ok(()) => {
                    self.record_success(started.elapsed());
                    return RetryOutcome::Delivered {
                        attempts: failures + 1,
                    };
                }
                Err(e @ TransportError::Permanent(_)) | Err(e @ TransportError::Config(_)) => {
                    self.record_failure();
                    crate::diag::report("delivery", &format!("permanent delivery failure: {e}"));
                    return RetryOutcome::Permanent;
                }
                Err(err) => {
                    failures += 1;
                    if failures >= attempts_allowed {
                        self.record_failure();
                        crate::diag::report(
                            "delivery",
                            &format!("delivery gave up after {failures} attempts: {err}"),
                        );
                        return RetryOutcome::Exhausted { attempts: failures };
                    }
                    if self.stop_requested.load(Ordering::Relaxed) {
                        self.record_failure();
                        return RetryOutcome::Exhausted { attempts: failures };
                    }
                    // First retry waits the base delay (no off-by-one); a
                    // server-provided Retry-After takes precedence, capped.
                    let mut delay = policy.delay_ms(failures - 1);
                    if let Some(after) = retry_after_hint(&err) {
                        delay = delay.max(after.min(self.retry_after_max_ms.max(1)));
                    }
                    sleep_interruptible(&self.stop_requested, delay);
                }
            }
        }
    }

    fn record_success(&self, took: Duration) {
        let ms = took.as_millis() as u64;
        let s = &self.stats;
        s.last_success_ms
            .store(crate::entry::now_millis(), Ordering::Relaxed);
        s.last_latency_ms.store(ms, Ordering::Relaxed);
        s.latency_sum_ms.fetch_add(ms, Ordering::Relaxed);
        s.latency_count.fetch_add(1, Ordering::Relaxed);
    }

    fn record_failure(&self) {
        self.stats.delivery_failures.fetch_add(1, Ordering::Relaxed);
        self.stats
            .last_failure_ms
            .store(crate::entry::now_millis(), Ordering::Relaxed);
    }

    fn dispatch(&self, batch: &CompressedBatch) {
        // The sender already counted this batch as in flight (see
        // `flush_batch`), so the channel→worker hand-off never looks idle;
        // this guard only releases the count when delivery finishes.
        let _busy = BusyGuard(&self.busy);
        if self.durability == Durability::WalFirst {
            if let Some(wal) = &self.wal {
                match wal.write(batch) {
                    Ok(path) => {
                        self.stats.wal_writes.fetch_add(1, Ordering::Relaxed);
                        match self.deliver_one(batch, &self.retry) {
                            RetryOutcome::Delivered { attempts: _ } => {
                                self.stats.delivered_batches.fetch_add(1, Ordering::Relaxed);
                                self.stats
                                    .delivered_entries
                                    .fetch_add(batch.payload.entry_count as u64, Ordering::Relaxed);
                                let _ = wal.remove(&path);
                            }
                            outcome => {
                                // The pre-write stays; the WAL retry pass will
                                // pick it up without another write.
                                self.debug_unexpected(outcome);
                            }
                        }
                        return;
                    }
                    Err(e) => {
                        crate::diag::report("wal", &format!("wal-first pre-write failed: {e}"));
                        // fall through to best-effort delivery below
                    }
                }
            }
        }
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

    fn debug_unexpected(&self, _outcome: RetryOutcome) {
        // Kept as a hook point; delivery failures are expected here and the
        // batch is already durable in the WAL.
    }

    /// Replay pending WAL batches, oldest first, one file in memory at a time.
    /// The pass stops at the first failure (circuit breaker) so a dead backend
    /// never blocks live delivery for the whole directory; failed files gain
    /// an attempt counter and move to `wal/dead/` after
    /// `dead_letter_replays` failures (spec §4.2 steps 6–8).
    fn replay_pending_wal(&self, policy: &RetryPolicy) {
        let Some(wal) = &self.wal else { return };
        let Ok(paths) = wal.pending_paths() else {
            return;
        };
        let _busy = BusyGuard::enter(&self.busy);
        for path in paths {
            let frame = match wal.read_frame(&path) {
                Ok(frame) => frame,
                Err(_) => {
                    // Unreadable/corrupt: remove so it cannot block forever.
                    let _ = wal.remove(&path);
                    continue;
                }
            };
            if frame.bytes.is_empty() {
                let _ = wal.remove(&path);
                continue;
            }
            let batch = frame.to_compressed_batch();
            match self.deliver_one(&batch, policy) {
                RetryOutcome::Delivered { .. } => {
                    let entries = if frame.entry_count > 0 {
                        frame.entry_count as u64
                    } else {
                        batch
                            .decompress_entries()
                            .map(|e| e.len() as u64)
                            .unwrap_or(0)
                    };
                    self.stats
                        .retried_entries
                        .fetch_add(entries, Ordering::Relaxed);
                    let _ = wal.remove(&path);
                }
                _ => {
                    let _ = wal.mark_failed(&path, self.dead_letter_replays);
                    return; // circuit breaker: retry on the next tick
                }
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
            jitter: self.retry.jitter,
        }
    }

    /// Spill batches still sitting in the dispatch channel to the WAL (or
    /// count them dropped) — used when `shutdown()` runs out of budget.
    fn spill_dispatch_to_wal(&self) {
        while let Ok(batch) = self.rx.try_recv() {
            // Counted at send time; this batch leaves the pipeline here.
            self.busy.fetch_sub(1, Ordering::AcqRel);
            let stored = self
                .wal
                .as_ref()
                .map(|w| w.write(&batch).is_ok())
                .unwrap_or(false);
            if stored {
                self.stats.wal_writes.fetch_add(1, Ordering::Relaxed);
            } else {
                let entries = batch.payload.entry_count as u64;
                self.stats.dropped_batches.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .dropped_entries
                    .fetch_add(entries, Ordering::Relaxed);
            }
        }
    }

    fn run(&self) {
        let tick = Duration::from_millis(self.tick_ms);
        let poll = Duration::from_millis(50);
        let mut last_replay = Instant::now();
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
                        last_replay = Instant::now();
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
        jitter: false,
    }
}

/// Sleep in 50ms slices so a shutdown request can interrupt backoff.
fn sleep_interruptible(stop: &AtomicBool, ms: u64) {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One supervised worker thread: its join handle plus an exit notification
/// channel, so shutdown can wait with a timeout instead of blocking forever
/// (a forked child, for example, has neither thread alive).
struct Worker {
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
    exit_rx: Mutex<Option<Receiver<()>>>,
}

impl Worker {
    fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> std::io::Result<Worker> {
        let (exit_tx, exit_rx) = crossbeam_channel::bounded::<()>(1);
        let handle = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                body();
                let _ = exit_tx.send(());
            })?;
        Ok(Worker {
            handle: Mutex::new(Some(handle)),
            exit_rx: Mutex::new(Some(exit_rx)),
        })
    }

    /// Wait up to `budget` for the worker to exit. Returns `false` on timeout
    /// (the worker keeps running as a daemon; nobody is blocked on it).
    fn join_within(&self, budget: Duration) -> bool {
        let Some(exit_rx) = self.exit_rx.lock().take() else {
            return true; // already waited on (and thus exited)
        };
        let exited = match exit_rx.recv_timeout(budget) {
            Ok(()) => true,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => false,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => true,
        };
        if exited {
            if let Some(handle) = self.handle.lock().take() {
                let _ = handle.join();
            }
        }
        exited
    }
}

/// Run `body`, restarting it if it panics (bounded), so a bug in one batch
/// cannot silently kill telemetry for the rest of the process lifetime.
fn supervised(name: &'static str, body: impl Fn() + Send + Sync + 'static) {
    let body = Arc::new(body);
    for restart in 0..=MAX_WORKER_RESTARTS {
        let body = body.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || body()));
        match result {
            Ok(()) => return,
            Err(payload) => {
                if restart == MAX_WORKER_RESTARTS {
                    crate::diag::report(
                        "worker",
                        &format!("{name} worker exceeded panic restart budget; giving up"),
                    );
                    return;
                }
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_string());
                crate::diag::report(
                    "worker",
                    &format!("{name} worker panicked ({msg}); restarting"),
                );
                std::thread::sleep(Duration::from_millis(100));
            }
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
    /// Nudges the batcher to flush immediately (used by `flush()`).
    flush_tx: Sender<()>,
    flush_rx: Receiver<()>,
    /// One-shot ack channels for in-flight `flush()` callers.
    flush_waiters: Mutex<Vec<Sender<()>>>,
    engine: Arc<DeliveryEngine>,
    batcher: Worker,
    delivery: Worker,
    stop_requested: Arc<AtomicBool>,
    /// Set after a fork in the child process: worker threads do not exist
    /// there, so calls become no-ops instead of hanging.
    detached: AtomicBool,
    shutdown_done: AtomicBool,
    stats: Arc<Stats>,
    /// Bound address of the `/healthz` endpoint, when `health_bind` is set.
    health_addr: Mutex<Option<std::net::SocketAddr>>,
}

impl MicroLog {
    /// Build a logger with an explicit transport.
    pub fn new(config: Config, transport: Arc<dyn Transport>) -> MicroLog {
        let (delivery_tx, delivery_rx) = crossbeam_channel::bounded(DISPATCH_CAPACITY);

        let queue_capacity = config.queue_capacity;
        let queue_policy = config.queue_full_policy;
        let wal_enabled = config.wal_enabled;
        let wal_retry_interval = config.wal_retry_interval_ms;
        let durability = config.durability;
        let dead_letter_replays = config.wal_dead_letter_replays;
        let retry_after_max_ms = config.retry_after_max_ms;
        let config_for_compressor = config.clone();
        let config_for_processor = config.clone();

        let wal = if wal_enabled {
            let ns = crate::wal::namespace(
                config.service.as_deref(),
                &config.backend,
                config.endpoint.as_deref(),
            );
            let dir = config.resolved_wal_dir().join(ns);
            match Wal::open(dir, config.wal_max_size_bytes) {
                Ok(w) => Some(Arc::new(w)),
                Err(e) => {
                    crate::diag::report("wal", &format!("WAL disabled: {e}"));
                    None
                }
            }
        } else {
            None
        };

        let stats = Arc::new(Stats::default());
        let stop_requested = Arc::new(AtomicBool::new(false));

        let engine = Arc::new(DeliveryEngine {
            transport,
            retry: policy_from_config(&config),
            wal: wal.clone(),
            durability,
            dead_letter_replays,
            retry_after_max_ms,
            stats: stats.clone(),
            rx: delivery_rx,
            busy: AtomicU64::new(0),
            stop_requested: stop_requested.clone(),
            tick_ms: wal_retry_interval,
        });

        let mut sample_rate_by_level = Vec::new();
        for (name, rate) in &config_for_processor.sample_rate_by_level {
            if let Ok(level) = name.parse::<LogLevel>() {
                sample_rate_by_level.push((level, *rate));
            }
        }

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
                        crate::diag::report(
                            "config",
                            &format!("ignoring invalid filter_regex: {e}"),
                        );
                    }
                    compiled.ok()
                }),
            redactor: crate::redact::Redactor::new(
                &config_for_processor.redact_keys,
                &config_for_processor.redact_patterns,
                config_for_processor.redact_builtin,
            ),
            dedup: (config_for_processor.dedup_window_ms > 0).then(|| {
                Arc::new(crate::governor::Dedup::new(
                    config_for_processor.dedup_window_ms,
                    10_000,
                ))
            }),
            rate_limiter: (config_for_processor.rate_limit_per_sec > 0).then(|| {
                Arc::new(crate::governor::RateLimiter::new(
                    config_for_processor.rate_limit_per_sec,
                ))
            }),
            adaptive_floor: config_for_processor
                .adaptive_sampling
                .then_some(config_for_processor.adaptive_sampling_floor),
            budget: (config_for_processor.daily_budget_entries > 0).then(|| {
                Arc::new(crate::governor::DailyBudget::new(
                    config_for_processor.daily_budget_entries,
                ))
            }),
            recorder: (config_for_processor.flight_recorder_size > 0).then(|| {
                Arc::new(crate::governor::FlightRecorder::new(
                    config_for_processor.flight_recorder_size,
                    config_for_processor
                        .flight_recorder_max_level
                        .parse()
                        .unwrap_or(LogLevel::Debug),
                ))
            }),
            sample_rate: config_for_processor.sample_rate,
            sample_rate_by_level,
            enrichment: if config_for_processor.enrichment {
                let (host, pid) = config_for_processor.host_and_process();
                let service = config_for_processor
                    .service
                    .clone()
                    .unwrap_or_else(|| host.clone());
                Some(crate::filter::EnrichmentContext {
                    host,
                    pid,
                    service,
                    env_vars: config_for_processor.enrichment_env_vars(),
                })
            } else {
                None
            },
        };

        let (batcher_stop_tx, batcher_stop_rx) = crossbeam_channel::bounded(0);
        let (flush_tx, flush_rx) = crossbeam_channel::bounded::<()>(8);

        let inner = Arc::new(MicroLogInner {
            flush_waiters: Mutex::new(Vec::new()),
            config,
            queue: LogQueue::new(queue_capacity, queue_policy),
            processor,
            compressor: Compressor::from_config(&config_for_compressor),
            delivery_tx,
            batcher_stop_tx,
            batcher_stop_rx,
            flush_tx,
            flush_rx,
            engine: engine.clone(),
            batcher: Worker {
                handle: Mutex::new(None),
                exit_rx: Mutex::new(None),
            },
            delivery: Worker {
                handle: Mutex::new(None),
                exit_rx: Mutex::new(None),
            },
            stop_requested,
            detached: AtomicBool::new(false),
            shutdown_done: AtomicBool::new(false),
            stats,
            health_addr: Mutex::new(None),
        });

        // Delivery worker (supervised against panics). Spawned after `inner`
        // so both worker slots can be filled directly.
        let engine_arc = inner.engine.clone();
        let delivery = Worker::spawn("tpt-microlog-delivery", move || {
            supervised("delivery", move || engine_arc.run());
        })
        .expect("spawn delivery thread");
        {
            let slot = &inner.delivery;
            *slot.handle.lock() = delivery.handle.lock().take();
            *slot.exit_rx.lock() = delivery.exit_rx.lock().take();
        }

        // Batcher thread (supervised against panics).
        let inner2 = inner.clone();
        let batcher = Worker::spawn("tpt-microlog-batcher", move || {
            supervised("batcher", move || run_batcher(inner2.clone()));
        })
        .expect("spawn batcher thread");

        {
            let slot = &inner.batcher;
            *slot.handle.lock() = batcher.handle.lock().take();
            *slot.exit_rx.lock() = batcher.exit_rx.lock().take();
        }

        if let Some(bind) = inner.config.health_bind.clone() {
            health_server::spawn(&inner, &bind);
        }

        MicroLog { inner }
    }

    /// Point-in-time health: queue depth, WAL backlog, delivery latency and a
    /// coarse status. Cheap enough to poll.
    pub fn health(&self) -> Health {
        self.inner.health()
    }

    /// Address the `/healthz` endpoint is listening on (`health_bind`).
    pub fn health_addr(&self) -> Option<std::net::SocketAddr> {
        *self.inner.health_addr.lock()
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

    /// Batches still waiting for the delivery worker.
    pub fn inflight(&self) -> usize {
        self.inner.engine.inflight()
    }

    /// `true` when nothing is buffered, queued, or mid-delivery.
    pub fn is_idle(&self) -> bool {
        self.inner.queue.is_empty() && self.inner.engine.idle()
    }

    /// Ask the batcher to flush buffered entries now and wait (bounded by
    /// `shutdown_timeout_ms`) until the ingestion queue and dispatch channel
    /// have drained *and* the delivery worker has finished with the batches.
    /// Returns `true` when everything was delivered or made durable.
    pub fn flush(&self) -> bool {
        let inner = &self.inner;
        if inner.detached.load(Ordering::Relaxed) || inner.shutdown_done.load(Ordering::SeqCst) {
            return self.is_idle();
        }
        let (ack_tx, ack_rx) = crossbeam_channel::bounded::<()>(1);
        inner.flush_waiters.lock().push(ack_tx);
        // Best-effort nudge; if a flush signal is already queued we are fine.
        let _ = inner.flush_tx.try_send(());
        let budget = Duration::from_millis(inner.config.shutdown_timeout_ms.max(1));
        let _ = ack_rx.recv_timeout(budget);
        // Wait for the queue and dispatch channel to drain.
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if self.is_idle() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.is_idle()
    }

    /// Called once in a forked child: worker threads do not exist there, so
    /// the logger detaches (all calls become cheap no-ops) instead of
    /// deadlocking the child.
    pub fn detach_after_fork(&self) {
        let inner = &self.inner;
        inner.detached.store(true, Ordering::SeqCst);
        inner.shutdown_done.store(true, Ordering::SeqCst);
        inner.stop_requested.store(true, Ordering::SeqCst);
    }

    fn emit(&self, mut entry: LogEntry) {
        let inner = &self.inner;
        if inner.detached.load(Ordering::Relaxed) {
            return;
        }
        // Correlate with the thread's active span (explicit metadata wins).
        if entry.log_type == LogType::Log {
            if let Some(ctx) = crate::trace::current() {
                entry
                    .metadata
                    .entry("trace_id")
                    .or_insert_with(|| Value::String(ctx.trace_id.clone()));
                entry
                    .metadata
                    .entry("span_id")
                    .or_insert_with(|| Value::String(ctx.span_id.clone()));
            }
        }
        use crate::governor::Decision;
        let stats = &inner.stats;
        match inner
            .processor
            .process_with(&mut entry, inner.queue.fill_ratio())
        {
            Decision::Keep => {
                // An error ships the recorded low-severity context first.
                for context in inner.processor.take_flight_context(&entry) {
                    self.enqueue(context);
                }
                self.enqueue(entry);
            }
            Decision::Sampled => {
                stats.sampled_entries.fetch_add(1, Ordering::Relaxed);
            }
            Decision::RateLimited => {
                stats.rate_limited_entries.fetch_add(1, Ordering::Relaxed);
            }
            Decision::Deduplicated => {
                stats.deduplicated_entries.fetch_add(1, Ordering::Relaxed);
            }
            Decision::Recorded => {
                stats.recorded_entries.fetch_add(1, Ordering::Relaxed);
            }
            Decision::Filtered => {}
        }
    }

    fn enqueue(&self, entry: LogEntry) {
        let inner = &self.inner;
        match inner.queue.push(entry) {
            PushOutcome::Accepted => {}
            PushOutcome::Dropped | PushOutcome::EvictedOldest => {
                inner.stats.dropped_entries.fetch_add(1, Ordering::Relaxed);
            }
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

    /// Graceful shutdown (spec §2.7): stops the batcher, flushes the final
    /// batch, drains the delivery channel, then replays the WAL once before
    /// worker threads exit. Idempotent, and bounded by
    /// [`Config::shutdown_timeout_ms`] — when the budget expires, everything
    /// still buffered spills to the WAL so the host app is never hung.
    pub fn shutdown(&self) {
        let inner = &self.inner;
        if inner.shutdown_done.swap(true, Ordering::SeqCst) {
            return;
        }
        if inner.detached.load(Ordering::SeqCst) {
            return;
        }
        let budget = Duration::from_millis(inner.config.shutdown_timeout_ms.max(1));
        let start = Instant::now();
        let remaining = || budget.saturating_sub(start.elapsed());

        // 1. Ask the batcher to drain its queue, flush the final batch, and
        //    exit (rendezvous on the stop channel).
        if !remaining().is_zero() {
            let _ = inner.batcher_stop_tx.send_timeout((), remaining());
        }
        // 2. Wait for the batcher. Only when it is done may the delivery
        //    worker be told to stop, so the final batch is never abandoned
        //    mid-drain.
        if !inner.batcher.join_within(remaining()) {
            // Out of budget: stop the workers and spill everything still
            // buffered so nothing silently disappears.
            inner.stop_requested.store(true, Ordering::SeqCst);
            inner.spill_queue_to_wal();
            inner.engine.spill_dispatch_to_wal();
        }
        // 3. Stop + join the delivery worker (drains channel + replays WAL).
        inner.stop_requested.store(true, Ordering::SeqCst);
        if !inner.delivery.join_within(remaining()) {
            // Daemon: it holds only `Arc`s and will finish on its own, so the
            // WAL lock must stay held while it may still write.
            inner.engine.spill_dispatch_to_wal();
        } else if let Some(wal) = &inner.engine.wal {
            // Both workers have exited: free the directory so a logger
            // recreated in this process can open the same WAL right away
            // (not only after this object is garbage-collected).
            wal.release_lock();
        }
    }
}

impl MicroLogInner {
    fn health(&self) -> Health {
        use crate::health::{classify, Health};
        let now = crate::entry::now_millis();
        let s = &self.stats;
        let r = Ordering::Relaxed;
        let (wal_files, wal_bytes) = self
            .engine
            .wal
            .as_ref()
            .map(|w| (w.len(), w.size_bytes()))
            .unwrap_or((0, 0));
        let (queue_depth, queue_capacity) = (self.queue.len(), self.config.queue_capacity);
        let (ok_ms, fail_ms) = (s.last_success_ms.load(r), s.last_failure_ms.load(r));
        let ago = |t: u64| (t > 0).then(|| now.saturating_sub(t));
        let count = s.latency_count.load(r);
        Health {
            status: classify(
                self.shutdown_done.load(Ordering::SeqCst),
                queue_depth,
                queue_capacity,
                wal_files,
                ok_ms,
                fail_ms,
                now,
            ),
            queue_depth,
            queue_capacity,
            inflight_batches: self.engine.inflight(),
            wal_files,
            wal_bytes,
            delivered_entries: s.delivered_entries.load(r),
            dropped_entries: s.dropped_entries.load(r),
            delivery_failures: s.delivery_failures.load(r),
            last_success_ms_ago: ago(ok_ms),
            last_failure_ms_ago: ago(fail_ms),
            last_delivery_latency_ms: s.last_latency_ms.load(r),
            avg_delivery_latency_ms: if count == 0 {
                0.0
            } else {
                s.latency_sum_ms.load(r) as f64 / count as f64
            },
        }
    }
    /// Drain the ingestion queue straight to the WAL, bypassing the (stuck)
    /// batcher. Entries without a WAL are counted dropped.
    fn spill_queue_to_wal(&self) {
        let rx = self.queue.receiver();
        let mut pending: Vec<LogEntry> = Vec::new();
        while let Ok(entry) = rx.try_recv() {
            pending.push(entry);
        }
        if pending.is_empty() {
            return;
        }
        let count = pending.len();
        let batch = Batch { entries: pending };
        let cb = self.compressor.compress_batch(&batch, count);
        let stored = self
            .engine
            .wal
            .as_ref()
            .map(|w| w.write(&cb).is_ok())
            .unwrap_or(false);
        if stored {
            self.stats.wal_writes.fetch_add(1, Ordering::Relaxed);
        } else {
            self.stats.dropped_batches.fetch_add(1, Ordering::Relaxed);
            self.stats
                .dropped_entries
                .fetch_add(count as u64, Ordering::Relaxed);
        }
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
/// or memory (10MB) — whichever comes first (spec §4.2 step 3) — plus on
/// explicit `flush()` requests.
fn run_batcher(inner: Arc<MicroLogInner>) {
    let rx = inner.queue.receiver();
    let flush_interval = std::time::Duration::from_millis(inner.config.flush_interval_ms);
    let ticker = crossbeam_channel::tick(flush_interval);
    let metrics_ticker = match inner.config.self_metrics_interval_ms {
        0 => crossbeam_channel::never(),
        ms => crossbeam_channel::tick(std::time::Duration::from_millis(ms)),
    };
    let mut pending: Vec<LogEntry> = Vec::new();
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
            recv(metrics_ticker) -> _ => {
                emit_self_metrics(&inner, &mut pending, &mut pending_bytes);
            },
            recv(ticker) -> _ => {
                absorb_summaries(&inner, &mut pending, &mut pending_bytes, false);
                flush_batch(&inner, &mut pending, &mut pending_bytes);
            },
            recv(inner.flush_rx) -> _ => {
                // Pull everything already queued so a flush covers every
                // entry logged before the flush request, then flush + ack.
                while let Ok(entry) = rx.try_recv() {
                    pending_bytes += entry.approx_size();
                    pending.push(entry);
                }
                absorb_summaries(&inner, &mut pending, &mut pending_bytes, true);
                flush_batch(&inner, &mut pending, &mut pending_bytes);
                notify_flush_waiters(&inner);
            },
            recv(inner.batcher_stop_rx) -> _ => {
                // Graceful stop: keep pulling entries that arrive during the
                // drain so nothing buffered in the queue is lost.
                while let Ok(entry) = rx.recv_timeout(std::time::Duration::from_millis(5)) {
                    pending_bytes += entry.approx_size();
                    pending.push(entry);
                }
                absorb_summaries(&inner, &mut pending, &mut pending_bytes, true);
                flush_batch(&inner, &mut pending, &mut pending_bytes);
                return;
            }
        }
    }
}

/// Append `repeat_count` summaries for closed de-duplication windows (all open
/// ones when `force`) to the batch being assembled.
fn absorb_summaries(
    inner: &MicroLogInner,
    pending: &mut Vec<LogEntry>,
    pending_bytes: &mut usize,
    force: bool,
) {
    for entry in inner
        .processor
        .drain_summaries(crate::entry::now_millis(), force)
    {
        *pending_bytes += entry.approx_size();
        pending.push(entry);
    }
}

/// Reply to every `flush()` caller waiting for the current flush.
fn notify_flush_waiters(inner: &MicroLogInner) {
    let waiters: Vec<Sender<()>> = inner.flush_waiters.lock().drain(..).collect();
    for waiter in waiters {
        let _ = waiter.try_send(());
    }
}

/// Compress + dispatch a pending batch; resets accumulation.
fn flush_batch(inner: &MicroLogInner, pending: &mut Vec<LogEntry>, pending_bytes: &mut usize) {
    if pending.is_empty() {
        return;
    }
    let entries: Vec<LogEntry> = std::mem::take(pending);
    *pending_bytes = 0;
    let count = entries.len();
    let batch = Batch { entries };
    let cb = inner.compressor.compress_batch(&batch, count);
    // Count the batch as in flight *before* it becomes visible to the
    // delivery worker; otherwise there is a window between `recv` and the
    // worker's own bookkeeping where `flush()` would see an idle engine and
    // return before the batch was delivered.
    inner.engine.busy.fetch_add(1, Ordering::AcqRel);
    if inner.delivery_tx.send(cb).is_err() {
        inner.engine.busy.fetch_sub(1, Ordering::AcqRel);
        inner.stats.dropped_batches.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for MicroLog {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Add the logger's own health numbers to the batch as metric entries
/// (`microlog.queue_depth`, `microlog.wal_bytes`, ...). They bypass sampling
/// and filters: a throttled logger is exactly when they matter most.
fn emit_self_metrics(
    inner: &MicroLogInner,
    pending: &mut Vec<LogEntry>,
    pending_bytes: &mut usize,
) {
    let health = inner.health();
    let mut tags = Map::new();
    tags.insert(
        "status".into(),
        Value::String(health.status.as_str().into()),
    );
    for (name, value) in health.metrics() {
        let mut entry = LogEntry::metric(name, value, Some(&tags));
        inner.processor.finalize(&mut entry);
        *pending_bytes += entry.approx_size();
        pending.push(entry);
    }
}

/// Minimal blocking HTTP/1.1 server for `/healthz` and `/stats` (std only).
mod health_server {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Weak};
    use std::time::Duration;

    use super::MicroLogInner;

    pub(super) fn spawn(inner: &Arc<MicroLogInner>, bind: &str) {
        let listener = match TcpListener::bind(bind) {
            Ok(l) => l,
            Err(e) => {
                crate::diag::report("health", &format!("cannot bind health_bind {bind}: {e}"));
                return;
            }
        };
        let _ = listener.set_nonblocking(true);
        *inner.health_addr.lock() = listener.local_addr().ok();
        let weak = Arc::downgrade(inner);
        let _ = std::thread::Builder::new()
            .name("tpt-microlog-health".into())
            .spawn(move || serve(listener, weak));
    }

    /// Serves until the logger is dropped (the thread only holds a `Weak`).
    fn serve(listener: TcpListener, weak: Weak<MicroLogInner>) {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let Some(inner) = weak.upgrade() else { return };
                    handle(stream, &inner);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if weak.strong_count() == 0 {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(40));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }

    fn handle(mut stream: TcpStream, inner: &MicroLogInner) {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let Ok(read_half) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(read_half);
        let mut request_line = String::new();
        if reader
            .by_ref()
            .take(2048)
            .read_line(&mut request_line)
            .is_err()
        {
            return;
        }
        // Drain the headers (bounded) so the client sees a clean response.
        let mut header = String::new();
        for _ in 0..64 {
            header.clear();
            match reader.by_ref().take(4096).read_line(&mut header) {
                Ok(0) => break,
                Ok(_) if header.trim().is_empty() => break,
                Ok(_) => {}
                Err(_) => return,
            }
        }
        let mut parts = request_line.split_whitespace();
        let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let path = target.split('?').next().unwrap_or("");
        let (code, body) = if method != "GET" {
            (405, serde_json::json!({"error": "method not allowed"}))
        } else {
            match path {
                "/healthz" | "/health" => {
                    let h = inner.health();
                    (h.status.http_status(), h.to_json())
                }
                "/stats" => {
                    let mut body = inner.health().to_json();
                    let counters: serde_json::Map<String, serde_json::Value> = inner
                        .stats
                        .counters()
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), v.into()))
                        .collect();
                    body["counters"] = serde_json::Value::Object(counters);
                    (200, body)
                }
                _ => (404, serde_json::json!({"error": "not found"})),
            }
        };
        let reason = match code {
            200 => "OK",
            404 => "Not Found",
            405 => "Method Not Allowed",
            _ => "Service Unavailable",
        };
        let payload = body.to_string();
        let _ = write!(
            stream,
            "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let _ = stream.flush();
    }
}
