//! Throughput benchmark for the ingestion pipeline (spec §7 targets:
//! > 100 000 logs/sec, < 2 % CPU steady-state).
//!
//! Run with:
//!
//! ```sh
//! cargo run --release -p tpt-log-core --example throughput -- [entries]
//! ```
//!
//! Ingestion measures the hot path (`info()` under the lock-free queue);
//! total additionally includes background batching, compression, and delivery
//! to the discarding "none" transport, ended by `shutdown()`.

use std::time::Instant;

use tpt_log_core::{Config, MicroLog};

fn main() {
    let n: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);

    let cfg = Config {
        backend: "none".into(),
        wal_enabled: false,
        queue_capacity: 200_000,
        batch_size: 100,
        flush_interval_ms: 1_000,
        ..Config::default()
    };
    let log = MicroLog::with_default_transport(cfg);

    let start = Instant::now();
    for i in 0..n {
        log.info(format!(
            "benchmark message number {i} with some payload text"
        ));
    }
    let ingest = start.elapsed();

    log.shutdown();
    let total = start.elapsed();

    println!(
        "entries:       {n}
ingest:        {ingest:?}  ({:.0} entries/s)
total+drain:   {total:?}  ({:.0} entries/s)",
        n as f64 / ingest.as_secs_f64(),
        n as f64 / total.as_secs_f64(),
    );
}
