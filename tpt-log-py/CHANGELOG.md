# Changelog

All notable changes to `tpt-log-py` are documented in this file.

This crate is published to PyPI as the `tpt-microlog` package; versions are
shared across the whole tpt-microlog workspace. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this crate adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed — Phase 9 reliability hardening

- Blocking calls (`shutdown`, `flush`, constructor) release the GIL while the
  engine works.
- NaN/Infinity metadata is kept as `"NaN"`/`"Infinity"` strings instead of
  raising `ValueError` — a float edge case can no longer break a log call.
- The Python-level `MicroLog` subclasses the native class and is registered
  for `atexit` shutdown and `os.register_at_fork` handling (children detach
  instead of deadlocking on the missing worker threads).
- `api_key`/`aws_secret_key` are redacted in repr/serde output.

### Added

- `flush()`, `stats()` (dict of delivery counters), context-manager support
  (`with MicroLog(...) as log:`), and `_detach_after_fork()`.

## [0.1.0] - 2026-09-29

Initial release on PyPI.

### Added

- `MicroLog` class with `trace`, `debug`, `info`, `warn`, `error`, and `fatal`
  methods accepting arbitrary keyword metadata.
- `metric(name, value, tags=None)` for numeric metrics and
  `span(name, trace_id, span_id, parent_span_id, duration_us)` for distributed
  traces.
- `queued()`, the `backend` property, and an idempotent `shutdown()`.
- `load_config(path)` constructor helper for `tpt-log.toml` files.
- Keyword-argument configuration covering backend selection, batching,
  compression, retries, WAL persistence, filtering, sampling, and enrichment.
- PyO3 0.29 extension module built with maturin against the stable ABI
  (`abi3-py39`), covering Python 3.9 through 3.13.
- Pure-Python `tpt_microlog` package with a type stub (`_native.pyi`) and a
  `py.typed` marker for full type-checker support.

[Unreleased]: https://github.com/tpt-solutions/tpt-microlog/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/tpt-solutions/tpt-microlog/releases/tag/v0.1.0
