# Changelog

All notable changes to `tpt-log-ffi` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

C ABI changes are treated as breaking and will require a major version bump.

## [Unreleased]

### Changed — Phase 9 reliability hardening

- `tpt_log_config.wal_enabled` is now zero-init friendly: `0` keeps the
  default, positive forces on, negative forces off (previously `0` disabled,
  which made a zero-initialized struct silently turn the WAL off).
- Every entry point is wrapped in a panic barrier; panics surface as the new
  `TPT_LOG_PANIC` (5) code instead of unwinding across the C ABI.

### Added

- `tpt_log_log_json()` — log with metadata as a JSON object string, supporting
  nested structures the flat `k=v,k2=v2` syntax cannot express.
- `tpt_log_flush()` — bounded synchronous flush.
- `tpt_log_stats_json()` — delivery statistics as JSON into a caller buffer
  (`TPT_LOG_STATS_TOO_SMALL` = -2 when the buffer is too small).

## [0.1.0] - 2026-09-29

Initial release of the C ABI.

### Added

- `tpt_log_create` / `tpt_log_destroy` for opaque `tpt_log_handle` lifecycle
  management.
- `tpt_log_trace`, `tpt_log_debug`, `tpt_log_info`, `tpt_log_warn`,
  `tpt_log_error`, `tpt_log_fatal`, and `tpt_log_log` for level-based logging.
- `tpt_log_metric` for numeric metrics with tag support.
- `tpt_log_shutdown` for flushing and draining the write-ahead log.
- `tpt_log_queued`, `tpt_log_version`, and `tpt_log_last_error` for
  introspection and diagnostics.
- `tpt_log_config` struct, `tpt_log_code` error enum, and `tpt_log_level`
  severity enum.
- `include/tpt_microlog.h`, a single-header, `extern "C"`-guarded declaration of
  the entire surface with documentation comments.
- `staticlib`, `cdylib`, and `rlib` crate types so the crate can be linked
  statically, loaded dynamically, or consumed from Rust.

[Unreleased]: https://github.com/tpt-solutions/tpt-microlog/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/tpt-solutions/tpt-microlog/releases/tag/v0.1.0
