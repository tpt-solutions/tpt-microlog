# tpt-microlog

**The microscopic telemetry & log router. Reliable delivery, zero infrastructure.**

`tpt-microlog` is an ultra-lightweight, embeddable logging and metrics engine written in pure Rust with bindings for **Python, JavaScript/WASM, Go, C/C++, and Rust**. It batches, compresses (zstd), and retries log delivery until your logs actually arrive — no sidecar collector, no heavy SDK.

```python
from tpt_microlog import MicroLog

log = MicroLog(backend="datadog", api_key="your-api-key")
log.info("User logged in", user_id=123, ip="1.2.3.4")
log.metric("cpu_usage", 42.5, tags={"host": "server-01"})
log.shutdown()  # flush + drain before exit
```

## Why

| Problem | tpt-microlog |
| --- | --- |
| Heavy agents (5–10 % CPU, 100s of MB of deps) | Minimal dependency tree, audited by `cargo-deny` on every commit |
| Logs silently dropped on network hiccups | Batching + zstd compression + exponential-backoff retry + disk write-ahead log (WAL) |
| One SDK per language | One Rust core, identical API via Python / JS / Go / C / Rust |
| Separate collector infrastructure (Fluent Bit, Vector) | Embeds directly in your process |

## Features

- **Lock-free ingestion** — `crossbeam` MPSC queue keeps `info()` on the hot path cheap.
- **Background batching** — flush by time (1 s), size (100 entries), or memory (10 MB), whichever comes first.
- **zstd compression** (level 3) shrinks network bandwidth and WAL disk I/O; disable with `fast_mode` for extreme throughput.
- **At-least-once delivery** — failed batches are persisted to a size-capped WAL (default 100 MB, FIFO drop-oldest) and replayed until the backend accepts them; on graceful shutdown everything accepted is delivered or spilled to the WAL, and `durability = "wal-first"` extends the guarantee across hard crashes.
- **Multi-backend** — Datadog, Elasticsearch, CloudWatch (SigV4), Prometheus (embedded `/metrics`), generic HTTP, gRPC (OTLP logs), UDP, TCP, and local JSONL files with rotation.
- **Structured + enriched** — arbitrary key/value metadata; optional hostname/PID/service enrichment; level filtering and sampling.
- **Zero configuration** — reads `tpt-log.toml`, then `TPT_LOG_*` environment variables, then your code's overrides.

## Installation

```bash
# Python (abi3 wheels for 3.9+)
pip install tpt-microlog

# JavaScript / TypeScript (browser + Node via WASM)
npm install tpt-microlog

# Go (cgo wrapper)
go get github.com/tpt-solutions/tpt-microlog-go

# Rust
cargo add tpt-log-core tpt-log-transport
```

## Usage

### Python

```python
from tpt_microlog import MicroLog, load_config

log = MicroLog(
    backend="datadog",
    api_key="your-api-key",
    batch_size=100,
    flush_interval=1.0,
)
log.info("User logged in", user_id=123, ip="1.2.3.4")
log.error("Database connection failed", error="timeout", retry_count=3)
log.metric("cpu_usage", 42.5, tags={"host": "server-01"})
log.span("checkout", trace_id="...", span_id="...", duration_us=1520)
log.shutdown()

# or load everything from a tpt-log.toml file
log = load_config("tpt-log.toml")
```

### JavaScript / WASM

```js
import { MicroLog } from 'tpt-microlog';

const log = new MicroLog({
  backend: 'custom',
  endpoint: 'https://my-api.com/logs',
  batchSize: 50,
  flushInterval: 2.0,
});

log.info('Page viewed', { page: '/home', userId: 123 });
log.metric('load_time', 1.23, { page: '/home' });
await log.shutdown();
```

WASM notes: there is no filesystem in the browser, so the WAL is unavailable and retries are in-memory only; `navigator.sendBeacon()` is used as a fallback during page unload.

### Go

```go
import microlog "github.com/tpt-solutions/tpt-microlog-go"

log := microlog.New(microlog.Config{
    Backend: "datadog",
    ApiKey:  "your-api-key",
})
log.Info("User logged in", map[string]any{"user_id": 123})
log.Metric("cpu_usage", 42.5, map[string]string{"host": "server-01"})
log.Shutdown()
```

The Go module wraps the C ABI in `tpt-log-ffi`; you need the native library built once per platform (see [examples/go](examples/go)).

### C / C++

```c
#include "tpt_microlog.h"

int main(void) {
    tpt_log_config config = {0};
    config.backend = "datadog";
    config.api_key = "your-api-key";

    tpt_log_handle* log = tpt_log_create(&config);
    tpt_log_info(log, "User logged in", "user_id=123,ip=1.2.3.4");
    tpt_log_metric(log, "cpu_usage", 42.5, "host=server-01");
    tpt_log_shutdown(log);
    tpt_log_destroy(log);
}
```

### Rust

```rust
use std::sync::Arc;
use tpt_log_core::{Config, MicroLog};
use tpt_log_transport::build_transport;

let cfg = Config::from_toml_str(r#"backend = "datadog""#)?;
let transport = build_transport(&cfg)?;
let log = MicroLog::new(cfg, Arc::from(transport));
log.info("user logged in");
log.metric("cpu", 0.5, None);
log.shutdown();
```

## Configuration

Configuration is layered: code overrides > `TPT_LOG_*` environment variables > `tpt-log.toml` > built-in defaults.

```toml
# tpt-log.toml
backend = "datadog"          # none|http|datadog|elasticsearch|cloudwatch|prometheus|grpc|udp|tcp|file
service = "checkout-api"
endpoint = "https://logs.example.com/v1"
batch_size = 100             # entries per batch
flush_interval_ms = 1000     # max time between flushes
memory_limit_bytes = 10485760
compression_level = 3        # zstd; 0 + fast_mode = true disables compression
max_retries = 10             # exponential backoff 1s -> 60s, jittered; honors Retry-After
queue_full_policy = "block"  # block | drop_newest | drop_oldest
durability = "best-effort"   # wal-first writes every batch to the WAL before the first send
shutdown_timeout_ms = 5000   # hard budget; leftovers spill to the WAL, never hang the app
wal_enabled = true
wal_max_size_bytes = 104857600
```

Key environment variables: `TPT_LOG_CONFIG` (explicit config file path), `TPT_LOG_BACKEND`, `TPT_LOG_ENDPOINT`, `TPT_LOG_API_KEY`, `TPT_LOG_SERVICE`, `TPT_LOG_BATCH_SIZE`, `TPT_LOG_FLUSH_INTERVAL_MS`, `TPT_LOG_QUEUE_CAPACITY`, `TPT_LOG_QUEUE_FULL_POLICY`, `TPT_LOG_DURABILITY`, `TPT_LOG_SHUTDOWN_TIMEOUT_MS`, `TPT_LOG_WAL_ENABLED`, `TPT_LOG_WAL_DIR`, `TPT_LOG_WAL_RETRY_INTERVAL_MS`, `TPT_LOG_MIN_LEVEL`, `TPT_LOG_SAMPLE_RATE`, `TPT_LOG_ENRICHMENT`, `TPT_LOG_ENRICH_ENV_VARS`, `TPT_LOG_FAST_MODE`, `TPT_LOG_FILE_PATH`, `TPT_LOG_NET_ENDPOINT`, `TPT_LOG_GRPC_ENDPOINT`, `TPT_LOG_EXTRA_HEADERS`, `TPT_LOG_PROMETHEUS_BIND`, `TPT_LOG_AWS_REGION`, `TPT_LOG_AWS_ACCESS_KEY`, `TPT_LOG_AWS_SECRET_KEY`, `TPT_LOG_AWS_LOG_GROUP`, `TPT_LOG_AWS_LOG_STREAM`.

Config files are discovered in this order: `$TPT_LOG_CONFIG`, then `./tpt-log.toml`, then `~/.config/tpt-microlog/tpt-log.toml` (`%APPDATA%\tpt-microlog` on Windows), then `/etc/tpt-microlog/tpt-log.toml`. Parse errors and unknown keys are reported on stderr — a typo can never silently disable a feature. Secret values (`api_key`, `aws_secret_key`) are redacted in `Debug`/serde output.

The WAL lives at `%APPDATA%\tpt-microlog\wal\` (Windows) or `~/.local/share/tpt-microlog/wal/` (Linux/macOS) by default, namespaced per service+backend so concurrent processes never replay each other's batches (guarded by an OS file lock). Files that keep failing replay move to `wal/dead/` after `wal_dead_letter_replays` attempts.

## Delivery guarantees

- **At-least-once on graceful shutdown**: everything accepted before `shutdown()` is delivered or persisted to the WAL (bounded by `shutdown_timeout_ms`; leftovers spill to the WAL when the budget expires — the host app is never hung).
- **At-least-once across crashes with `durability = "wal-first"`**: each batch is written to the WAL *before* the first delivery attempt.
- **Best-effort during operation with the default `durability = "best-effort"`**: a hard process kill can lose the in-memory buffer between WAL writes (use `wal-first` if that matters to you).
- `queue_full_policy = "block"` never drops (at the cost of backpressure); `drop_newest`/`drop_oldest` trade drops for latency, counted in `dropped_entries`.

## Performance

Spec targets and measured results (1 M entries, `none` backend, release build; re-measured on the same machine before and after the Phase 9 reliability hardening):

| Metric | Target | Measured |
| --- | --- | --- |
| Ingest throughput | > 100,000 logs/sec | ~365,000 logs/sec |
| End-to-end incl. batching/compression | — | ~347,000 logs/sec |
| WASM binary size | < 3 MB | ~0.8 MB (pre-`wasm-opt`) |

Reproduce the benchmark:

```sh
cargo run --release -p tpt-log-core --example throughput -- 1000000
```

Remaining resource targets (validated by design and covered by tests, not yet tracked by CI dashboards): < 20 MB constant memory, < 2 % CPU, > 10x zstd compression ratio, at-least-once delivery on graceful shutdown via WAL replay/spill.

## Repository layout

| Path | Purpose |
| --- | --- |
| [`tpt-log-core/`](tpt-log-core/) | Pure Rust engine: lock-free queue, batching, zstd, WAL, retry, filtering/sampling/enrichment |
| [`tpt-log-transport/`](tpt-log-transport/) | HTTP, Datadog/Elasticsearch/CloudWatch/Prometheus adapters, gRPC (OTLP), UDP, TCP, rotating JSONL file |
| [`tpt-log-ffi/`](tpt-log-ffi/) | C ABI boundary (`include/tpt_microlog.h`) for C/C++/Go consumers |
| [`tpt-log-py/`](tpt-log-py/) | PyO3 wrapper shipped to PyPI as `tpt-microlog` (maturin, abi3 3.9+) |
| [`tpt-log-wasm/`](tpt-log-wasm/) | Self-contained wasm-bindgen wrapper shipped to npm as `tpt-microlog` |
| [`tpt-log-go/`](tpt-log-go/) | cgo wrapper module (`github.com/tpt-solutions/tpt-microlog-go`) |
| `examples/` | Runnable examples for Python, JavaScript/Node, Go, and C |
| `.github/workflows/` | CI (deny/clippy/test/fmt/wasm) and PyPI/npm publish pipelines |

### Per-crate documentation

Each package carries its own README and CHANGELOG:

| Crate | Docs | Changelog |
| --- | --- | --- |
| `tpt-log-core` | [README](tpt-log-core/README.md) | [CHANGELOG](tpt-log-core/CHANGELOG.md) |
| `tpt-log-transport` | [README](tpt-log-transport/README.md) | [CHANGELOG](tpt-log-transport/CHANGELOG.md) |
| `tpt-log-ffi` | [README](tpt-log-ffi/README.md) | [CHANGELOG](tpt-log-ffi/CHANGELOG.md) |
| `tpt-log-py` | [README](tpt-log-py/README.md) | [CHANGELOG](tpt-log-py/CHANGELOG.md) |
| `tpt-log-wasm` | [README](tpt-log-wasm/README.md) | [CHANGELOG](tpt-log-wasm/CHANGELOG.md) |
| `tpt-log-go` | [README](tpt-log-go/README.md) | [CHANGELOG](tpt-log-go/CHANGELOG.md) |

Release notes that affect several crates are summarized in the section above;
consult the individual changelogs for crate-specific detail.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. All transitive dependencies are license-audited in CI via `cargo-deny`; copyleft licenses are rejected.
