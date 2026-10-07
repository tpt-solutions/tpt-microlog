//! Ingestion → batch → compress → deliver throughput against a null transport.
//!
//! `cargo run --release -p tpt-log-core --example ci_bench` prints
//! `entries_per_sec=<n>` (median of 5 runs). CI runs it on the base and head
//! commits on the same machine and fails on a >20% regression.

use std::time::Instant;

use tpt_log_core::{Config, MicroLog};

const ENTRIES: usize = 200_000;
const RUNS: usize = 5;

fn one_run() -> f64 {
    let cfg = Config {
        backend: "none".into(),
        wal_enabled: false,
        batch_size: 500,
        queue_capacity: 50_000,
        ..Config::default()
    };
    let log = MicroLog::with_default_transport(cfg);
    let start = Instant::now();
    for i in 0..ENTRIES {
        log.log_with(
            tpt_log_core::LogLevel::Info,
            "request handled",
            [
                ("i", serde_json::json!(i)),
                ("route", serde_json::json!("/api/v1/items")),
            ],
        );
    }
    log.flush();
    let secs = start.elapsed().as_secs_f64();
    log.shutdown();
    ENTRIES as f64 / secs
}

fn main() {
    let mut runs: Vec<f64> = (0..RUNS).map(|_| one_run()).collect();
    runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("entries_per_sec={:.0}", runs[RUNS / 2]);
}
