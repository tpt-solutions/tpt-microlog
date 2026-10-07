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

---

## Phase 9 — Reliability Hardening (from platform review)

### WAL & delivery
- [x] WAL: namespace dir by hash(service+backend+endpoint); add lock file so processes never replay each other's batches
- [x] WAL: unique filenames (pid + random) and atomic write (temp file + rename)
- [x] WAL: dead-letter dir (`wal/dead/`) after N failed replays; per-file attempt counter
- [x] WAL: O(1) size accounting (stop re-listing dir on every write); stream `pending()` instead of loading all files
- [x] WAL: persist `entry_count` so `Stats.retried_entries` is correct
- [x] `shutdown()`/`Drop`: add `shutdown_timeout_ms`; spill remaining batches to WAL on expiry (never hang the host app)
- [x] Replay: stop pass at first failure (circuit breaker) or move to separate thread so live delivery is never blocked
- [x] Queue overflow policy: `block | drop_newest | drop_oldest` + `dropped_entries` stat
- [x] Supervise batcher/delivery threads (panic recovery, no `unwrap`/`expect` in worker paths)
- [x] Optional `durability = "wal-first"` mode; reword README "100% delivery" claim to at-least-once on graceful shutdown

### Retry & classification
- [x] Treat HTTP 408/425/429 as transient; honor `Retry-After`
- [x] Fix backoff off-by-one (first retry currently immediate) and add jitter
- [x] Remove dead duplicate `deliver_with_retry` in `retry.rs`

### Config & security
- [x] Surface `tpt-log.toml` parse errors instead of silently defaulting; warn on unknown keys (`deny_unknown_fields`)
- [x] Fix README sample config (`[endpoint]` table breaks parsing)
- [x] Redact secrets in `Debug`/`Serialize` (`Secret<String>` for `api_key`, `aws_secret_key`)
- [x] Only expand leading `~/` in `wal_dir` (currently corrupts Windows 8.3 paths)
- [x] `host_and_process()` must return real hostname, not `service`; implement env-var enrichment
- [x] Expand env-var coverage: FILE_PATH, NET_ENDPOINT, GRPC_ENDPOINT, AWS_*, EXTRA_HEADERS, PROMETHEUS_BIND, QUEUE_CAPACITY, WAL_RETRY_INTERVAL_MS, `TPT_LOG_CONFIG`

### Filtering
- [x] Metrics/traces bypass `min_level` and sampling
- [x] Per-level sampling map; never sample WARN and above
- [x] `is_enabled()` should account for regex/tag filters

### Bindings
- [x] FFI/C: fix `wal_enabled` tri-state (zero-init must mean default, not off)
- [x] Go: fix `WalEnabled:false` (currently sends "default" = on); add `WalDir`, `CompressionLevel`
- [x] Go: `sync.RWMutex` around handle (Close race) and `runtime.LockOSThread` around `lastError()`
- [x] FFI: add `tpt_log_log_json()` for nested metadata; wrap all entry points in `catch_unwind`
- [x] Python: release GIL (`allow_threads`) on blocking calls; never raise on NaN/Inf metadata
- [x] Python: `flush()`, context manager, `atexit` shutdown hook, `os.register_at_fork` handling
- [x] Expose `stats()` and `flush()` in every binding (Py/JS/Go/C)

### Hot path performance
- [x] Remove 3x entry copy (processor clone -> Arc -> batch clone); make `process(&mut entry)`
- [x] Remove global `Mutex<usize>` from queue push/pop (and dead `buffered` counter)
- [x] Carry entries/raw JSON with `CompressedBatch` to avoid decompress -> parse -> re-serialize in every adapter
- [x] `approx_size` should account for real metadata value sizes
- [x] Re-run throughput benchmark; record RSS/CPU so README targets are measured

### Regression tests
- [x] Backend down + `shutdown()` returns within timeout
- [x] 429 retried; 400 dead-lettered after N replays
- [x] Two instances with different services never cross-deliver WAL files
- [x] Queue full + `drop_newest` never blocks `info()`
- [x] `sample_rate`/`min_level` leave metrics and ERRORs intact
- [x] Python: NaN metadata, GIL release, fork; Go: `go test`; WASM: 4xx head-of-line case

## Phase 10 — Adapter Correctness

- [x] HTTP/custom backend: default to plain JSON body; zstd/gzip opt-in
- [x] Prometheus: key samples by name + label set; escape label values; sanitize label keys; one `# TYPE` per metric; stoppable listener
- [x] Datadog: send metadata as top-level attributes (not `ddtags`); `site` option; split requests at 1000 entries / 5 MB; optional Metrics API for `metric()`
- [x] Elasticsearch: sane default index (not hostname, lowercase); parse bulk `errors:true` responses; basic-auth / API-key options; retry on 429
- [x] CloudWatch: include metadata in message; sort events by timestamp; enforce 10k events / 1 MB / 14-day limits; session token + env/IAM credentials; optional auto-create group/stream
- [x] UDP: add batch id to frame header; correct compressed flag; MTU-safe chunk size (~1200-1400 B)
- [x] TCP: write timeout; compressed flag in frame; optional ack; remove `expect` panics
- [x] WASM: distinguish transient/permanent errors, cap retries (fix 4xx head-of-line blocking)
- [x] WASM: use `fetch(..., { keepalive: true })` instead of `sendBeacon` when headers are needed
- [x] WASM: emit Datadog-format payload for `backend: "datadog"`; document "never ship API keys in browser bundles"
- [x] WASM: publish feature-parity table vs native (filters, enrichment, compression, default min level)

## Phase 11 — Adoption & Usability

### Onboarding
- [x] `console`/`stdout` backend (pretty on TTY, JSON lines when piped); make it the default with a startup banner
- [x] Named presets (`dev`, `prod`, `datadog-eu`, `elastic-cloud`, `grafana-loki`, `otel-collector`, `cloudwatch-lambda`) with actionable validation errors
- [x] `tpt-microlog` CLI: `init`, `doctor` (config/DNS/TLS/auth/WAL checks), `wal ls|replay|purge|dead`, `tail`, `send-test`
- [x] Config search paths (`./`, `~/.config/tpt-microlog/`, `/etc/`) + `TPT_LOG_CONFIG`
- [x] JSON Schema for `tpt-log.toml`; "did you mean...?" hints for unknown keys; complete `.pyi`
- [x] `on_error` callback / internal diagnostics channel so failures are never silent

### Integrations
- [x] Python `logging.Handler`
- [x] Go `slog.Handler`
- [x] Rust `tracing` Layer and `log` facade; panic hook; builder API; structured log macro
- [x] Node/pino or console transport
- [x] Multi-backend fan-out and failover routing
- [x] TLS/proxy config (`ca_cert`, `proxy`); OTLP/HTTP; Loki; Splunk HEC

### Examples & templates
- [x] `examples/stacks/`: ELK, Loki+Grafana, OTel Collector, Prometheus+Grafana (docker-compose + dashboards) — configs written; only the Prometheus app side verified, containers not started
- [ ] `examples/frameworks/`: FastAPI, Flask, Django, Express, Next.js, Gin, net/http, Axum, AWS Lambda, Cloudflare Worker, CLI tool
- [x] `examples/recipes/`: request-id correlation, PII redaction, sampling noisy DEBUG, offline-first WAL demo, migration from `logging`
- [ ] Recipes: migration from winston and zap
- [ ] Starter templates per language (cookiecutter/degit), GitHub template repo, `.env.example`, Dockerfile + K8s ConfigMap/Helm snippets
- [x] `tpt-log.toml` gallery in `docs/` (parsed and validated by a test)

### Distribution & docs
- [x] Go: prebuilt static libs as release assets + fetcher; module path now matches its subdirectory (`.../tpt-microlog/tpt-log-go`, tag `tpt-log-go/vX.Y.Z`). Fetcher verified end to end against a local server; the build matrix in `release.yml` is not run yet; Windows is amd64 only (no cgo toolchain for arm64)
- [ ] Publish `tpt-log-core` / `tpt-log-transport` to crates.io
- [x] Release automation: `scripts/release.py` (bump + changelog roll, tested) and `release.yml` (binaries, SBOM, Sigstore signing, draft release). Workflow not run on GitHub yet
- [x] Docs site (mdBook, `docs/book`): quickstarts, backend cookbook, "delivery guarantees explained", troubleshooting, benchmark methodology. Builds with no broken links; hosting/deployment not set up
- [x] README: badges, corrected delivery-guarantee wording
- [ ] README: 30-second demo GIF; registry badges once the packages are published
- [x] `CONTRIBUTING.md` (issues only), `SECURITY.md`, `CODE_OF_CONDUCT.md`, issue forms (no PR template: PRs are not accepted), `CHANGELOG.md`

## Phase 12 — Differentiators (pick 2-3)

- [x] PII/secret redaction (`redact` keys + regex + built-in detectors) applied before WAL and network
- [x] Adaptive sampling under pressure (always keep WARN+)
- [x] Burst dedup / rate limiting with `repeat_count`
- [x] Error-triggered "flight recorder" (ship buffered DEBUG context only on ERROR/FATAL)
- [x] Daily volume/cost budget with auto-tightening sampling
- [x] Automatic trace correlation (W3C `traceparent` / OTel context) + span guard/context manager
- [x] Self-telemetry (`queue_depth`, `wal_bytes`, `dropped`, latency) + `/healthz`
- [x] WAL query/inspection + redacted support bundle export
- [x] `tpt-microlog-agent` sidecar (UDP/Unix socket/HTTP) for languages without bindings
- [x] Edge/WASM extras: Cloudflare Worker/Vercel Edge build, browser error auto-capture, IndexedDB offline queue — logic tested under Node; real browser/edge runtimes not exercised
- [x] Schema-aware typed events

## CI & Repo Hygiene

- [x] Real OS matrix (ubuntu/macos/windows); fix `--target` so the windows-gnu leg actually cross-builds
- [x] Add Python (`maturin develop` + `pytest`), Go (`go test`), and Node/WASM smoke-test jobs
- [x] MSRV job
- [x] Benchmark job that fails on >20% throughput regression
- [x] Dependabot/renovate
- [x] Verify `wasm-opt -Oz` actually runs in `publish-npm.yml`
