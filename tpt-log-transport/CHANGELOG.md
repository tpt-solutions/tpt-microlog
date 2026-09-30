# Changelog

All notable changes to `tpt-log-transport` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
