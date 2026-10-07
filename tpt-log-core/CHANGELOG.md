# Changelog

All notable changes to `tpt-log-core` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed — Phase 10 adapter correctness

- New config knobs: `body_mode` (`json` default | `zstd` opt-in for the
  generic HTTP backend), `datadog_site` + `datadog_metrics`, `es_index` +
  `es_basic_auth`, `aws_session_token` + `aws_auto_create_stream`, and
  `tcp_ack`; all exposed via `tpt-log.toml` and the corresponding
  `TPT_LOG_*` environment variables.
- `Config::resolved_aws_credentials()` implements the standard `AWS_*`
  environment-variable credential chain for the CloudWatch adapter.


### Changed — Phase 9 reliability hardening

- **WAL**: per-identity namespace directories (hash of service+backend+endpoint)
  guarded by an OS file lock, so two processes never replay each other's
  batches; unique pid+random filenames written via temp-file + atomic rename.
- **WAL**: dead-letter directory (`wal/dead/`) after `wal_dead_letter_replays`
  failed replay passes, with per-file attempt counters; corrupt files are
  removed instead of blocking the queue forever.
- **WAL**: O(1) size accounting (no per-write directory rescan), persisted
  `entry_count` in the frame header (legacy frames still decode), and streaming
  replay that reads one file at a time.
- **Delivery**: replay passes stop at the first failure (circuit breaker) so a
  dead backend never blocks live delivery; `durability = "wal-first"` writes
  every batch to the WAL before the first send.
- **Shutdown**: bounded by `shutdown_timeout_ms` (default 5 s); whatever is
  still buffered when the budget expires spills to the WAL — `shutdown()` and
  `Drop` never hang the host app.
- **Queue**: configurable overflow policy (`block | drop_newest |
  drop_oldest`) with a `dropped_entries` stat; the hot path no longer clones
  each entry three times (processor clone → `Arc` → batch clone) and the
  global byte-counter mutex is gone.
- **Retry**: the first retry now waits the full base delay (off-by-one fixed),
  delays carry de-synchronizing jitter, HTTP 408/425/429 are classified as
  rate-limited (retrying with the server's `Retry-After`, capped), and the
  dead duplicate `deliver_with_retry` loop was removed.
- **Filtering**: metrics and trace spans bypass `min_level` and sampling;
  WARN and above are never sampled; per-level sampling overrides
  (`[sample_rate_by_level]`); `is_enabled()` accounts for regex/tag filters.
- **Config**: parse errors and unknown keys (with "did you mean" hints) are
  reported on stderr instead of being swallowed; `api_key`/`aws_secret_key`
  are redacted in `Debug`/serde output; only a leading `~/` is expanded (fixes
  Windows 8.3 short paths); `host_and_process()` returns the real hostname;
  config discovery follows `$TPT_LOG_CONFIG` → `./` → user config → `/etc`.
- **Env vars**: added `TPT_LOG_FILE_PATH`, `TPT_LOG_NET_ENDPOINT`,
  `TPT_LOG_GRPC_ENDPOINT`, `TPT_LOG_EXTRA_HEADERS`, `TPT_LOG_PROMETHEUS_BIND`,
  `TPT_LOG_QUEUE_CAPACITY`, `TPT_LOG_QUEUE_FULL_POLICY`,
  `TPT_LOG_WAL_RETRY_INTERVAL_MS`, `TPT_LOG_DURABILITY`,
  `TPT_LOG_SHUTDOWN_TIMEOUT_MS`, `TPT_LOG_ENRICH_ENV_VARS`, and the
  `TPT_LOG_AWS_*` family.
- **Workers**: batcher and delivery threads are supervised — a panic restarts
  the worker (bounded) instead of silently killing telemetry.
- Enrichment can capture selected environment variables into every entry
  (`enrich_env_vars`).

### Added

- `MicroLog::flush()` — bounded, synchronous flush of buffered entries.
- `MicroLog::inflight()`, `Stats::dropped_entries`, `detach_after_fork()`.
- `QueueFullPolicy`, `Durability`, and `Secret` (redacted credential wrapper)
  exported from the crate root.
- `WalFrame` carries `entry_count`; `Wal::pending_paths()`/`read_frame()` for
  streaming consumers; `Wal::namespace()` for identity derivation.

### Fixed

- Throughput re-measured after the hot-path rework: ~365k ingest /
  ~347k end-to-end entries/sec (1 M entries, `none` backend) — measurably
  faster than the 0.1.0 pipeline on identical hardware.

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
