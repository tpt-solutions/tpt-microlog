# Changelog

All notable changes to `tpt-log-core` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Planned

- Replay statistics exposed through `Stats` for WAL recoveries.
- Optional `mpsc` backpressure mode that blocks instead of dropping.

## [0.1.0] - 2026-09-29

Initial release of the core engine.

### Added

- `MicroLog` with `trace`/`debug`/`info`/`warn`/`error`/`fatal`, `log_with`,
  `metric`, and `span` entry points.
- `LogQueue`: bounded `crossbeam-channel` MPSC ingestion queue with
  backpressure and `drain_available`.
- `Batch` / `CompressedBatch` / `Compressor`: EOF batching by entry count,
  elapsed time, or in-memory byte budget, with zstd compression at a
  configurable level and a `fast_mode` bypass.
- `Wal` / `SharedWal`: size-capped write-ahead log with FIFO drop-oldest
  eviction and background replay.
- `RetryPolicy` / `deliver_with_retry`: exponential backoff from 1 s to a 60 s
  ceiling with a configurable attempt limit and permanent-failure short-circuit.
- `EntryProcessor`: minimum-level filtering, sampling, tag and regex filtering,
  and hostname/PID/service enrichment.
- `Config`: `tpt-log.toml` parsing layered with `TPT_LOG_*` environment
  overrides and built-in defaults, plus the `defaults` constants module.
- `Transport` trait with `NullTransport` and a test-only `SpyTransport`.
- `LogEntry` / `LogLevel` / `LogType` with serde JSON round-tripping.
- `throughput` example for reproducible ingest benchmarks.

[Unreleased]: https://github.com/tpt-solutions/tpt-microlog/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/tpt-solutions/tpt-microlog/releases/tag/v0.1.0
