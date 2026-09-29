# tpt-microlog

**The microscopic telemetry & log router. Guaranteed delivery, zero infrastructure.**

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
- **Guaranteed delivery** — failed batches are persisted to a size-capped WAL (default 100 MB, FIFO drop-oldest) and replayed until the backend accepts them.
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
batch_size = 100             # entries per batch
flush_interval_ms = 1000     # max time between flushes
memory_limit_bytes = 10485760
compression_level = 3        # zstd; 0 + fast_mode = true disables compression
max_retries = 10             # exponential backoff 1s -> 60s
wal_enabled = true
wal_max_size_bytes = 104857600

[endpoint]                   # or a plain `endpoint = "..."` string
# endpoint = "https://logs.example.com/v1"
```

Key environment variables: `TPT_LOG_BACKEND`, `TPT_LOG_ENDPOINT`, `TPT_LOG_API_KEY`, `TPT_LOG_SERVICE`, `TPT_LOG_BATCH_SIZE`, `TPT_LOG_FLUSH_INTERVAL_MS`, `TPT_LOG_WAL_ENABLED`, `TPT_LOG_WAL_DIR`, `TPT_LOG_MIN_LEVEL`, `TPT_LOG_SAMPLE_RATE`, `TPT_LOG_ENRICHMENT`, `TPT_LOG_FAST_MODE`.

The WAL lives at `%APPDATA%\tpt-microlog\wal\` (Windows) or `~/.local/share/tpt-microlog/wal/` (Linux/macOS) by default.

## Performance

Spec targets and measured results (1 M entries, `none` backend, release build on a development laptop):

| Metric | Target | Measured |
| --- | --- | --- |
| Ingest throughput | > 100,000 logs/sec | ~450,000 logs/sec |
| End-to-end incl. batching/compression | — | ~425,000 logs/sec |
| WASM binary size | < 3 MB | ~0.8 MB (pre-`wasm-opt`) |

Reproduce the benchmark:

```sh
cargo run --release -p tpt-log-core --example throughput -- 1000000
```

Remaining resource targets (validated by design and covered by tests, not yet tracked by CI dashboards): < 20 MB constant memory, < 2 % CPU, > 10x zstd compression ratio, 100 % delivery guarantee via WAL replay.

## Repository layout

| Path | Purpose |
| --- | --- |
| `tpt-log-core/` | Pure Rust engine: lock-free queue, batching, zstd, WAL, retry, filtering/sampling/enrichment |
| `tpt-log-transport/` | HTTP, Datadog/Elasticsearch/CloudWatch/Prometheus adapters, gRPC (OTLP), UDP, TCP, rotating JSONL file |
| `tpt-log-ffi/` | C ABI boundary (`include/tpt_microlog.h`) for C/C++/Go consumers |
| `tpt-log-py/` | PyO3 wrapper shipped to PyPI as `tpt-microlog` (maturin, abi3 3.9+) |
| `tpt-log-wasm/` | Self-contained wasm-bindgen wrapper shipped to npm as `tpt-microlog` |
| `tpt-log-go/` | cgo wrapper module (`github.com/tpt-solutions/tpt-microlog-go`) |
| `examples/` | Runnable examples for Python, JavaScript/Node, Go, and C |
| `.github/workflows/` | CI (deny/clippy/test/fmt/wasm) and PyPI/npm publish pipelines |

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. All transitive dependencies are license-audited in CI via `cargo-deny`; copyleft licenses are rejected.
