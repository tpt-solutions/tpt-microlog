# tpt-log-core

Core logging engine for [tpt-microlog](https://github.com/tpt-solutions/tpt-microlog) — the microscopic telemetry & log router.

`tpt-log-core` is a pure-Rust, transport-agnostic logging pipeline. It owns the
hot path (lock-free ingestion), background batching, zstd compression, the
write-ahead log (WAL), exponential-backoff retry, and level filtering,
sampling, and enrichment. Network transports live in
[`tpt-log-transport`](../tpt-log-transport) and plug in through the `Transport`
trait, so this crate builds and tests without any network dependency.

- **API docs:** <https://docs.rs/tpt-log-core>
- **Repository:** <https://github.com/tpt-solutions/tpt-microlog/tree/master/tpt-log-core>

## Install

```toml
[dependencies]
tpt-log-core = "0.1"
```

You also need a transport to actually deliver batches. For a complete setup:

```toml
[dependencies]
tpt-log-core = "0.1"
tpt-log-transport = "0.1"
```

## Usage

```rust
use std::sync::Arc;
use tpt_log_core::{Config, MicroLog, Transport};
use tpt_log_transport::build_transport;

let config = Config::from_toml_str(r#"
    backend = "datadog"
    api_key = "your-api-key"
    service = "checkout-api"
"#)?;

let transport = build_transport(&config)?;
let log = MicroLog::new(config, Arc::from(transport));

log.info("user logged in");
log.error("db connection failed"); // level filter and retry apply here too
log.metric("cpu_usage", 42.5, None);
log.shutdown();
```

Without `tpt-log-transport`, bring your own [`Transport`] implementation:

```rust
use std::sync::Arc;
use tpt_log_core::{CompressedBatch, Config, MicroLog, Transport, TransportError};

struct StdoutTransport;

impl Transport for StdoutTransport {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        println!("{}", String::from_utf8_lossy(&batch.payload));
        Ok(())
    }
    fn name(&self) -> &str {
        "stdout"
    }
}

let log = MicroLog::new(Config::default(), Arc::new(StdoutTransport));
log.warn("something looks off");
log.shutdown();
```

For tests, `with_spy` hands you a `SpyTransport` that records every batch:

```rust
use tpt_log_core::{Config, MicroLog};

let (log, spy) = MicroLog::with_spy(Config::default());
log.info("captured");
log.shutdown();
assert_eq!(spy.batches(), 1);
```

## Pipeline

```
log.info(msg) -> LogQueue (bounded MPSC)
    -> EntryProcessor  (level filter, sampling, tag/regex filter, enrichment)
    -> Batch           (flush on 100 entries, 1 s, or 10 MB)
    -> Compressor      (zstd level 3; skipped in fast_mode)
    -> Transport::send -> on failure: RetryPolicy -> Wal -> replay
```

| Stage | Type | Notes |
| --- | --- | --- |
| Queue | `LogQueue` | `crossbeam` MPSC, `queue_capacity` (default 10 000), bounded backpressure |
| Processing | `EntryProcessor` | `min_level`, `sample_rate` (0.0–1.0), `filter_tags`, `filter_regex`, `enrichment` |
| Batching | `Batch`, `CompressedBatch` | EOF by size / time / memory, whichever fires first |
| Compression | `Compressor` | zstd, `compression_level` (default 3) |
| Delivery | `RetryPolicy`, `deliver_with_retry` | exponential backoff 1 s → 60 s, `max_retries` (default 10) |
| Durability | `Wal`, `SharedWal` | size-capped (default 100 MB), FIFO drop-oldest, replay thread every 5 s |

## Configuration

Layering is **code overrides > `TPT_LOG_*` env vars > `tpt-log.toml` > defaults**.

```toml
# tpt-log.toml
backend = "none"             # none|custom|datadog|elasticsearch|cloudwatch|prometheus|grpc|udp|tcp|file
service = "checkout-api"
batch_size = 100
flush_interval_ms = 1000
memory_limit_bytes = 10485760
queue_capacity = 10000
compression_level = 3
fast_mode = false
max_retries = 10
wal_enabled = true
wal_dir = "~/.local/share/tpt-microlog/wal"
min_level = "INFO"
sample_rate = 1.0
enrichment = true
```

Environment variables: `TPT_LOG_BACKEND`, `TPT_LOG_ENDPOINT`, `TPT_LOG_API_KEY`,
`TPT_LOG_SERVICE`, `TPT_LOG_BATCH_SIZE`, `TPT_LOG_FLUSH_INTERVAL_MS`,
`TPT_LOG_WAL_ENABLED`, `TPT_LOG_WAL_DIR`, `TPT_LOG_MIN_LEVEL`,
`TPT_LOG_SAMPLE_RATE`, `TPT_LOG_ENRICHMENT`, `TPT_LOG_FAST_MODE`.

Defaults live in the [`defaults`] module (`BATCH_SIZE`, `FLUSH_INTERVAL_MS`,
`MEMORY_LIMIT_BYTES`, `QUEUE_CAPACITY`, `COMPRESSION_LEVEL`, `RETRY_BASE_MS`,
`RETRY_MAX_BACKOFF_MS`, `MAX_RETRIES`, `WAL_MAX_SIZE_BYTES`,
`WAL_RETRY_INTERVAL_MS`).

## Logging API

| Method | Purpose |
| --- | --- |
| `trace` / `debug` / `info` / `warn` / `error` / `fatal` | Level-based logging |
| `log_with(level, message)` | Explicit `LogLevel` |
| `metric(name, value, tags)` | Numeric metric with optional tags |
| `span(name, trace_id, span_id, ...)` | Distributed-trace span |
| `queued()` | Entries still buffered in the queue |
| `stats()` | Ingest/flush/drop counters |
| `shutdown()` | Flush and drain (idempotent) |

Entries serialize to JSON via serde, with `message`, `level`, `log_type`,
`timestamp`, and a free-form `metadata` map.

## Features

- `test-utils` — exposes test-only helpers such as `SpyTransport`.

## Benchmark

```sh
cargo run --release -p tpt-log-core --example throughput -- 1000000
```

Measured ~450,000 logs/sec ingest on the `none` backend (release build,
development laptop) against a > 100,000 logs/sec target.

## License

Dual-licensed under [MIT](https://github.com/tpt-solutions/tpt-microlog/blob/master/LICENSE-MIT) or [Apache-2.0](https://github.com/tpt-solutions/tpt-microlog/blob/master/LICENSE-APACHE),
at your option.
