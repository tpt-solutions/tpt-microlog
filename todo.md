# tpt-microlog — Development Checklist

**Project:** The Microscopic Telemetry & Log Router  
**Ecosystem:** TPT Solutions  
**License:** Dual MIT / Apache-2.0  

---

## Phase 1 — Core Logging Engine (Pure Rust)

- [ ] Scaffold Cargo workspace with all member crates (`tpt-log-core`, `tpt-log-transport`, `tpt-log-ffi`, `tpt-log-py`, `tpt-log-wasm`)
- [ ] Write `deny.toml` with approved/denied license lists (spec §3.2)
- [ ] Add `LICENSE-MIT` and `LICENSE-APACHE` files (copyright TPT Solutions)
- [ ] Set up `.github/workflows/ci.yml`: `cargo deny check`, `cargo test`, `cargo clippy --all-targets`
- [ ] Implement `LogEntry` struct: `{ timestamp, level, message, metadata: HashMap<String, Value> }`
- [ ] Implement `LogLevel` enum: TRACE, DEBUG, INFO, WARN, ERROR, FATAL
- [ ] Implement `LogType` enum: Log, Metric, Trace
- [ ] Implement lock-free MPSC queue using `crossbeam-channel` for log ingestion
- [ ] Implement `MicroLog` public API: `info()`, `warn()`, `error()`, `debug()`, `trace()`, `fatal()`, `metric()`
- [ ] Implement background batcher thread: time-based (1s), size-based (100 logs), memory-based (10 MB)
- [ ] Implement zstd compression (level 3) for log batches
- [ ] Implement `tpt-log.toml` config file parser + env-var overrides
- [ ] Verify `cargo deny check licenses` passes clean (zero violations)
- [ ] Write unit tests for queue, batching, and compression

---

## Phase 2 — Transport Layer

- [ ] Define `Transport` trait: `async fn send(&self, batch: CompressedBatch) -> Result<(), TransportError>`
- [ ] Implement HTTP/HTTPS transport via `reqwest` (rustls-tls only — no openssl-sys)
- [ ] Implement retry logic: exponential backoff 1s → 2s → 4s → … → 60s max, 10 retries max
- [ ] Implement Write-Ahead Log (WAL): append-only file, one zstd-compressed batch per line
- [ ] Implement WAL size cap: default 100 MB, FIFO drop oldest when exceeded
- [ ] Implement WAL path: platform-appropriate (`~/.tpt-microlog/wal/` on Linux/macOS, `%APPDATA%\tpt-microlog\wal\` on Windows)
- [ ] Implement background WAL retry thread: reads WAL, retries delivery, deletes on success
- [ ] Implement graceful `shutdown()`: flushes in-memory queue, drains WAL before exit
- [ ] Write integration tests: mock HTTP server + WAL + retry round-trip

---

## Phase 3 — Backend Adapters

- [ ] Implement `CustomHttpAdapter`: generic JSON POST to any endpoint with configurable headers
- [ ] Implement `DatadogAdapter`: format as Datadog Logs API v2 JSON, send to `https://http-intake.logs.datadoghq.com/api/v2/logs`
- [ ] Implement `ElasticsearchAdapter`: format as ES Bulk API (`/_bulk`), support index templates
- [ ] Implement `CloudWatchAdapter`: format as CloudWatch Logs PutLogEvents, AWS SigV4 signing
- [ ] Implement `PrometheusAdapter`: expose `/metrics` endpoint (pull model), embedded HTTP listener
- [ ] Write adapter-level tests with mock backends

---

## Phase 4 — Advanced Protocols

- [ ] Implement gRPC transport via `tonic` (supports OpenTelemetry Collector gRPC endpoint)
- [ ] Implement UDP transport: fire-and-forget, lowest latency, no delivery guarantee
- [ ] Implement TCP transport: reliable ordered delivery for custom TCP servers
- [ ] Implement File transport: writes JSONL (one log entry per line) to local file
- [ ] Implement log rotation for file transport (size-based and time-based)
- [ ] Write protocol-level tests

---

## Phase 5 — Python Wrapper (PyO3)

- [ ] Create `tpt-log-py/` crate with `pyo3` dependency
- [ ] Expose `MicroLog` class: `__init__`, `info()`, `warn()`, `error()`, `debug()`, `trace()`, `fatal()`, `metric()`, `shutdown()`
- [ ] Support keyword arguments for structured metadata: `log.info("msg", user_id=123, ip="1.2.3.4")`
- [ ] Set up `maturin` for building Python wheels
- [ ] Create `.github/workflows/publish-py.yml`: build wheels for Linux/macOS/Windows, publish to PyPI
- [ ] Write Python package `__init__.py` and `py.typed` marker
- [ ] Write Python documentation and usage examples (`examples/python/`)
- [ ] Test Python wrapper against all backends

---

## Phase 6 — WASM Wrapper (wasm-bindgen)

- [ ] Create `tpt-log-wasm/` crate with `wasm-bindgen` dependency
- [ ] Implement browser-compatible HTTP transport (uses browser `fetch` API, not reqwest)
- [ ] Implement `navigator.sendBeacon()` fallback for guaranteed delivery on page unload
- [ ] Document WASM limitations: no WAL (no disk access), degraded retry (in-memory only)
- [ ] Optimize binary size with `wasm-opt -Oz` in CI; target < 3 MB WASM binary
- [ ] Create `package.json` with npm metadata
- [ ] Set up `.github/workflows/publish-npm.yml`: build WASM, publish to npm as `tpt-microlog`
- [ ] Write JavaScript/TypeScript type definitions (`.d.ts`)
- [ ] Write browser and Node.js usage examples (`examples/js/`)

---

## Phase 7 — Go Wrapper (cgo)

- [ ] Implement C ABI in `tpt-log-ffi/`: `tpt_log_create()`, `tpt_log_info()`, `tpt_log_metric()`, `tpt_log_shutdown()`, `tpt_log_destroy()`
- [ ] Write `tpt_microlog.h` C header file
- [ ] Create `tpt-log-go/` Go module: `module github.com/tpt-solutions/tpt-microlog-go`
- [ ] Implement Go `MicroLog` struct wrapping cgo FFI calls
- [ ] Write Go usage examples (`examples/go/`)
- [ ] Document cgo build requirements and cross-compilation notes
- [ ] Publish to Go module registry

---

## Phase 8 — Advanced Features (Optional)

- [ ] Implement distributed tracing spans (OpenTelemetry-compatible, behind a feature flag)
- [ ] Implement log filtering: by level, by tag, by regex on message
- [ ] Implement log sampling: configurable percentage (e.g. send only 10% of DEBUG logs)
- [ ] Implement log enrichment: auto-add hostname, PID, environment variables to every entry
- [ ] Implement "fast mode": disable zstd compression for extreme throughput (> 1M logs/sec)
- [ ] Run performance benchmarks: validate > 100,000 logs/sec throughput, < 20 MB RAM, < 2% CPU
- [ ] Publish benchmark results to README
