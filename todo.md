# tpt-microlog — Development Checklist

**Project:** The Microscopic Telemetry & Log Router  
**Ecosystem:** TPT Solutions  
**License:** Dual MIT / Apache-2.0  

---

## Phase 1 — Core Logging Engine (Pure Rust)

- [x] Scaffold Cargo workspace with all member crates (`tpt-log-core`, `tpt-log-transport`, `tpt-log-ffi`, `tpt-log-py`, `tpt-log-wasm`)
- [x] Write `deny.toml` with approved/denied license lists (spec §3.2)
- [x] Add `LICENSE-MIT` and `LICENSE-APACHE` files (copyright TPT Solutions)
- [x] Set up `.github/workflows/ci.yml`: `cargo deny check`, `cargo test`, `cargo clippy --all-targets`
- [x] Implement `LogEntry` struct: `{ timestamp, level, message, metadata: HashMap<String, Value> }`
- [x] Implement `LogLevel` enum: TRACE, DEBUG, INFO, WARN, ERROR, FATAL
- [x] Implement `LogType` enum: Log, Metric, Trace
- [x] Implement lock-free MPSC queue using `crossbeam-channel` for log ingestion
- [x] Implement `MicroLog` public API: `info()`, `warn()`, `error()`, `debug()`, `trace()`, `fatal()`, `metric()`
- [x] Implement background batcher thread: time-based (1s), size-based (100 logs), memory-based (10 MB)
- [x] Implement zstd compression (level 3) for log batches
- [x] Implement `tpt-log.toml` config file parser + env-var overrides
- [x] Verify `cargo deny check licenses` passes clean (zero violations)
- [x] Write unit tests for queue, batching, and compression

## Phase 2 — Transport Layer

- [x] Define `Transport` trait: `async fn send(&self, batch: CompressedBatch) -> Result<(), TransportError>`
- [x] Implement HTTP/HTTPS transport via `reqwest` (rustls-tls only — no openssl-sys)
- [x] Implement retry logic: exponential backoff 1s → 2s → 4s → … → 60s max, 10 retries max
- [x] Implement Write-Ahead Log (WAL): append-only file, one zstd-compressed batch per line
- [x] Implement WAL size cap: default 100 MB, FIFO drop oldest when exceeded
- [x] Implement WAL path: platform-appropriate (`~/.tpt-microlog/wal/` on Linux/macOS, `%APPDATA%\tpt-microlog\wal\` on Windows)
- [x] Implement background WAL retry thread: reads WAL, retries delivery, deletes on success
- [x] Implement graceful `shutdown()`: flushes in-memory queue, drains WAL before exit
- [x] Write integration tests: mock HTTP server + WAL + retry round-trip

## Phase 3 — Backend Adapters

- [x] Implement `CustomHttpAdapter`: generic JSON POST to any endpoint with configurable headers
- [x] Implement `DatadogAdapter`: format as Datadog Logs API v2 JSON, send to `https://http-intake.logs.datadoghq.com/api/v2/logs`
- [x] Implement `ElasticsearchAdapter`: format as ES Bulk API (`/_bulk`), support index templates
- [x] Implement `CloudWatchAdapter`: format as CloudWatch Logs PutLogEvents, AWS SigV4 signing
- [x] Implement `PrometheusAdapter`: expose `/metrics` endpoint (pull model), embedded HTTP listener
- [x] Write adapter-level tests with mock backends

## Phase 4 — Advanced Protocols

- [x] Implement gRPC transport via `tonic` (supports OpenTelemetry Collector gRPC endpoint)
- [x] Implement UDP transport: fire-and-forget, lowest latency, no delivery guarantee
- [x] Implement TCP transport: reliable ordered delivery for custom TCP servers
- [x] Implement File transport: writes JSONL (one log entry per line) to local file
- [x] Implement log rotation for file transport (size-based and time-based)
- [x] Write protocol-level tests

## Phase 5 — Python Wrapper (PyO3)

- [x] Create `tpt-log-py/` crate with `pyo3` dependency
- [x] Expose `MicroLog` class: `__init__`, `info()`, `warn()`, `error()`, `debug()`, `trace()`, `fatal()`, `metric()`, `shutdown()`
- [x] Support keyword arguments for structured metadata: `log.info("msg", user_id=123, ip="1.2.3.4")`
- [x] Set up `maturin` for building Python wheels
- [x] Create `.github/workflows/publish-py.yml`: build wheels for Linux/macOS/Windows, publish to PyPI
- [x] Write Python package `__init__.py` and `py.typed` marker
- [x] Write Python documentation and usage examples (`examples/python/`)
- [x] Test Python wrapper against all backends (file backend end-to-end via maturin + venv)

## Phase 6 — WASM Wrapper (wasm-bindgen)

- [x] Create `tpt-log-wasm/` crate with `wasm-bindgen` dependency
- [x] Implement browser-compatible HTTP transport (uses browser `fetch` API, not reqwest)
- [x] Implement `navigator.sendBeacon()` fallback for guaranteed delivery on page unload
- [x] Document WASM limitations: no WAL (no disk access), degraded retry (in-memory only)
- [x] Optimize binary size with `wasm-opt -Oz` in CI; target < 3 MB WASM binary (~0.8 MB measured)
- [x] Create `package.json` with npm metadata
- [x] Set up `.github/workflows/publish-npm.yml`: build WASM, publish to npm as `tpt-microlog`
- [x] Write JavaScript/TypeScript type definitions (`.d.ts`)
- [x] Write browser and Node.js usage examples (`examples/js/`)
- [x] Verify end-to-end in a real browser (fetch delivery, batching, metadata, metrics)

## Phase 7 — Go Wrapper (cgo)

- [x] Implement C ABI in `tpt-log-ffi/`: `tpt_log_create()`, `tpt_log_info()`, `tpt_log_metric()`, `tpt_log_shutdown()`, `tpt_log_destroy()`
- [x] Write `tpt_microlog.h` C header file (`tpt-log-ffi/include/`)
- [x] Create `tpt-log-go/` Go module: `module github.com/tpt-solutions/tpt-microlog-go`
- [x] Implement Go `MicroLog` struct wrapping cgo FFI calls
- [x] Write Go usage examples (`examples/go/`)
- [x] Document cgo build requirements and cross-compilation notes
- [ ] Publish to Go module registry (needs tagged release / separate repo per Go hosting rules)

## Phase 8 — Advanced Features (Optional)

- [x] Implement distributed tracing spans (OpenTelemetry-compatible, behind a feature flag)
- [x] Implement log filtering: by level, by tag, by regex on message
- [x] Implement log sampling: configurable percentage (e.g. send only 10% of DEBUG logs)
- [x] Implement log enrichment: auto-add hostname, PID, environment variables to every entry
- [x] Implement "fast mode": disable zstd compression for extreme throughput (> 1M logs/sec)
- [x] Run performance benchmarks: validate > 100,000 logs/sec throughput, < 20 MB RAM, < 2% CPU (~450k logs/sec measured; see README)
- [x] Publish benchmark results to README
