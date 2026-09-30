# Changelog

All notable changes to `tpt-microlog-go` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this module adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Versions are shared across the whole tpt-microlog workspace; this module follows
the versions published by `tpt-log-ffi`.

## [Unreleased]

### Changed — Phase 9 reliability hardening

- `Config.WalEnabled` is now explicit: `false` really disables the WAL (the
  old mapping sent "default", which is on).
- `MicroLog` guards the native handle with a `sync.RWMutex`, so `Close` cannot
  free the handle while another goroutine is mid-call (verified under
  `-race`); error reads pin the OS thread (`runtime.LockOSThread`) because the
  native error buffer is thread-local.
- Added `Config.WalDir` and `Config.CompressionLevel`.

### Added

- `MicroLog.Flush()` — bounded synchronous flush.
- `MicroLog.Stats()` — delivery counters (`Stats` struct).

## [0.1.0] - 2026-09-29

Initial release.

### Added

- `Config` struct mapping to the native `tpt_log_config`, covering backend,
  endpoint, API key, service name, batch size, flush interval, retry count, and
  WAL toggling.
- `New` returning a `*MicroLog` handle, with native errors surfaced through
  `tpt_log_last_error`.
- `Trace`, `Debug`, `Info`, `Warn`, `Error`, and `Fatal` methods accepting a
  `map[string]any` of structured metadata.
- `Metric` for numeric values with a `map[string]string` tag set.
- `Queued`, `Shutdown`, `Close`, and package-level `Version` helpers.
- JSON encoding of Go metadata into the C ABI's `k=v` representation, with
  quoted-value-aware comma splitting.
- cgo build directives and documentation for Linux, macOS, and Windows,
  including the GNU-toolchain static link recipe for Windows.
- Test coverage for configuration mapping, metadata encoding, and error
  propagation.

[Unreleased]: https://github.com/tpt-solutions/tpt-microlog/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/tpt-solutions/tpt-microlog/releases/tag/v0.1.0
