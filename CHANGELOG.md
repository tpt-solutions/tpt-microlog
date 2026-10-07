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
