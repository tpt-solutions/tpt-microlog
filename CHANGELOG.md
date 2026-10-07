# Changelog

Workspace-level release notes. Each package also keeps its own changelog
(`tpt-log-*/CHANGELOG.md`) with crate-specific detail. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions are shared
across the workspace and the project is pre-1.0.

## [Unreleased]

### Added

- **Diagnostics channel**: `set_on_error` (Rust, C, Python, Go, and an `onError`
  option for WASM) receives delivery, WAL, worker and config failures instead
  of stderr; identical messages are rate-limited.
- **Redaction**: `redact_keys`, `redact_patterns` and `redact_builtin` mask
  secrets/PII before entries reach the WAL or any transport.
- **Ecosystem integrations**: Python `logging.Handler`, Go `slog.Handler`, Rust
  `tracing` layer (feature `tracing`), `log` backend (feature `log`), panic
  hook, `mlog!` macro, `Builder`, and a pino bridge (`tpt-microlog/pino`).
- **`tpt-microlog` CLI**: `init`, `doctor`, `wal ls|replay|dead|purge`, `tail`,
  `send-test`.
- **Volume governors**: `dedup_window_ms` (burst folding with `repeat_count`),
  `rate_limit_per_sec`, `adaptive_sampling`, `daily_budget_entries` and an
  error-triggered flight recorder (`flight_recorder_size`); WARN+ is never
  sampled or limited. New `stats()` counters for each.
- **Trace correlation**: W3C `traceparent` parsing, span guards that stamp
  `trace_id`/`span_id` on every entry (Rust `trace`, Python `trace_span`, the
  `tracing` layer), and **typed events** (Rust `Event` trait, Python
  `log.event`, TypeScript `EventMap`).
- **CLI**: `wal query` (decode/search undelivered entries) and
  `support-bundle` (masked diagnostics to attach to an issue).
- **`tpt-microlog-agent`** sidecar (`tpt-log-agent`): UDP, TCP, HTTP and Unix-socket
  ingestion of JSON/text logs and metrics for languages without a binding.
- **Edge/WASM extras**: browser error auto-capture, an offline queue with
  IndexedDB persistence, and a Cloudflare Worker example.
- **Documentation site** (`docs/book`, mdBook): per-language quickstarts, backend
  cookbook, delivery guarantees explained, volume control, privacy, observability,
  CLI, troubleshooting and benchmark methodology.
- **Release tooling**: `scripts/release.py` (version bump + changelog roll,
  never publishes), `release.yml` (CLI/agent binaries, Go static libraries,
  CycloneDX SBOMs, Sigstore signatures, draft GitHub release) and
  `docs/RELEASING.md`.
- **Go**: the module path is now `github.com/tpt-solutions/tpt-microlog/tpt-log-go`
  (resolvable from this repository via `tpt-log-go/vX.Y.Z` tags), the C header
  is vendored, link flags are per platform, and `cmd/fetchlib` downloads and
  verifies a prebuilt static library.
- **Health**: `health()` snapshot (also Python/Go/C `tpt_log_health_json`),
  `health_bind` serving `/healthz` and `/stats`, and `self_metrics_interval_ms`
  for `microlog.*` metrics.
- **Backends**: Grafana Loki, Splunk HEC, OTLP/HTTP, console, and
  fan-out/failover routing; named presets and TLS/proxy options.
- **`tpt-log.toml` JSON Schema** and a validated [config gallery](docs/toml-gallery.md).
- `examples/recipes/` and `examples/stacks/`.
- `Config`/FFI: `tpt_log_create_from_toml`; Python options for sampling by
  level, durability, queue policy, presets and more.
- CI: OS matrix, MSRV (1.89), Python/Go/WASM smoke tests, benchmark regression
  gate, Dependabot. `CONTRIBUTING.md` (issues only), `SECURITY.md`,
  `CODE_OF_CONDUCT.md`, issue forms.

### Fixed

- `flush()` could return before the last batch was delivered (a window between
  the delivery worker taking a batch and counting it as in flight).
- A WAL batch could be dead-lettered on its first failure when its random file
  suffix looked like an attempt counter (e.g. `a8125429`).
- `shutdown()` kept the WAL directory locked until the object was garbage
  collected, so a recreated logger silently ran without a WAL.
- The gRPC backend never delivered through `MicroLog` (its channel was bound to
  the constructing thread's runtime) and refused to construct while the
  collector was down; it now owns a runtime and connects lazily.
- FFI metadata strings split nested JSON values (`req={"a":1,"b":2}`) at inner
  commas and rejected the call.
- WASM: `shutdown()`/`dispose()` left a pending timer that fired into a dropped
  closure; the documented JS usage omitted the required module initialisation.
- npm publish workflow: invalid YAML, `wasm-bindgen` CLI version not matching
  `Cargo.lock`, and `wasm-opt` running before the glue was generated.
- WAL overflow (oldest batches discarded) and dead-lettering are now reported
  instead of happening silently.

### Changed

- Minimum supported Rust version is 1.89 (`File::try_lock`).
- `cargo test` runs on Linux, macOS and Windows; the Windows-gnu build is a
  cross-compile job.

## [0.1.0]

Initial scaffold: core pipeline (lock-free queue, batching, zstd, WAL, retry),
HTTP/Datadog/Elasticsearch/CloudWatch/Prometheus/gRPC/UDP/TCP/file transports,
and Python, WASM, Go and C bindings.
