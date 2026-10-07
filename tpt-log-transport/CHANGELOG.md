# Changelog

All notable changes to `tpt-log-transport` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed — Phase 10 adapter correctness

- **HTTP/custom**: plain JSON bodies by default; zstd bodies (`Content-Encoding:
  zstd`) are opt-in via `body_mode = "zstd"` (`fast_mode` still implies JSON).
- **Prometheus**: series are keyed by metric name + label set (same name with
  different tags no longer overwrite each other), label keys are sanitized,
  label values are escaped per the text format, exactly one `# TYPE` line is
  emitted per metric name, and the embedded listener is stoppable
  (`PrometheusAdapter::stop()`, also on `Drop`).
- **Datadog**: metadata travels as top-level attributes instead of the legacy
  `ddtags` string; requests split at the 1000-event / 5 MB intake caps;
  `datadog_site` derives the intake and Metrics API endpoints; with
  `datadog_metrics = true`, `metric()` entries also ship to
  `/api/v2/series` as gauges.
- **Elasticsearch**: the index defaults to `tpt-microlog` (never the machine
  hostname) and is lowercased/sanitized; bulk responses with `errors: true`
  are inspected per item (item 429 → transient and retried, other statuses →
  permanent); auth via `api_key` (`Authorization: ApiKey …`) or
  `es_basic_auth` (`user:pass` → Basic); HTTP-level 429 was already retried.
- **CloudWatch**: metadata is inlined into the event message as sorted
  `key=value` pairs; events are sorted by timestamp (PutLogEvents requires
  it); batches respect the service limits (10 000 events / ~1 MB per call)
  and events older than 14 days are skipped; STS session tokens are attached
  and signed; credentials fall back to `AWS_ACCESS_KEY_ID` /
  `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN`; optional
  `aws_auto_create_stream` creates the group/stream (idempotently) on first
  send.
- **UDP**: frames carry a batch id (13-byte header), the compressed flag now
  reflects the actual payload encoding, and chunks are MTU-safe (1 300 B).
- **TCP**: frames carry the compressed flag (`u32 len | u8 flags | payload`),
  writes are time-bounded (10 s), `tcp_ack` waits for a receiver
  acknowledgement, and all `expect` panics are replaced with error mapping.

### Added

- `BodyMode` is exported from the crate root (re-export of the core type).
- `TcpTransport::with_options`, `net::tcp::parse_frame` /
  `net::tcp::FRAME_HEADER_LEN`, `net::tcp::ACK_BYTE`,
  `net::udp::HEADER_LEN` (now 13 B including the batch id).
- `PrometheusAdapter::stop()`.


### Changed — Phase 9 reliability hardening

- HTTP status classification now treats 408/425/429 as rate-limited
  (`TransportError::RateLimited`, honoring `Retry-After`) instead of
  permanent; other 4xx stay permanent, 5xx/network stay transient.
- `HttpTransport` JSON mode posts the JSON carried alongside the compressed
  bytes (no decompress → parse → re-serialize round trip).
- Datadog/CloudWatch adapters read credentials through the redacted `Secret`
  wrapper; secret values never appear in `Debug`/serde output.

### Added

- `TransportError::RateLimited { message, retry_after_ms }`.

## [0.1.0] - 2026-09-29

Initial release of the transport layer.

### Added

- `build_transport` / `describe`: config-to-transport factory with validation
  for every backend name in `tpt_log_core::config::backend`.
- `HttpTransport` with `BodyMode` variants, a pooled blocking `reqwest` client
  over `rustls-tls`, and centralized transient/permanent failure
  classification in `http::request_once`.
- `GrpcTransport`: OTLP logs over gRPC using `tonic`, with protobuf definitions
  vendored under `proto/opentelemetry/` and compiled by `build.rs`.
- `UdpTransport` and `TcpTransport` for `net_endpoint` targets.
- `FileTransport`: local JSONL output with size- and time-based rotation and a
  configurable number of retained files.
- Adapters: Datadog Logs API v2, Elasticsearch Bulk API, AWS CloudWatch
  `PutLogEvents`, embedded Prometheus `/metrics`, and AWS SigV4 signing.
- Re-exports of `Config`, `Transport`, and `TransportError` so downstream crates
  need only a single transport dependency.
- End-to-end integration tests for the HTTP and adapter paths.

[Unreleased]: https://github.com/tpt-solutions/tpt-microlog/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/tpt-solutions/tpt-microlog/releases/tag/v0.1.0
