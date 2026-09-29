# Changelog

All notable changes to `tpt-log-wasm` are documented in this file.

This crate is published to npm as the `tpt-microlog` package; versions are
shared across the whole tpt-microlog workspace. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this crate adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Planned

- `wasm-opt -Oz` pass in CI to shrink the binary further.
- Optional Content-Encoding negotiation for endpoints that accept gzip.
- Source maps and a browser debug build target.

## [0.1.0] - 2026-09-29

Initial release on npm.

### Added

- `MicroLog` class with `trace`, `debug`, `info`, `warn`, `error`, and `fatal`
  methods, plus `logWithLevel` for explicit level strings.
- `metric` and `span` entry points with optional tag objects.
- `flush()` and idempotent `shutdown()` returning the number of entries still
  queued, so callers can detect a failed delivery.
- `dispose()` and `free()` (also invoked by `Symbol.dispose`) for releasing the
  background timer, unload hooks, and the underlying WASM object.
- Browser `fetch` delivery with configurable endpoint, backend label, API key
  (`DD-API-KEY` for Datadog), and extra headers.
- Client-side `minLevel` filtering, `sampleRate` sampling, and `service`
  enrichment.
- Batching by `batchSize` and `flushInterval` with a bounded `maxBuffer` that
  drops oldest entries during outages.
- In-memory retry of failed batches, plus a `navigator.sendBeacon()` last-ditch
  delivery bound to the `pagehide` event.
- Self-contained build with no dependency on the native engine, since browsers
  have no threads, no disk WAL, and no blocking HTTP stack.
- Committed `wasm-bindgen` output under `dist/` and hand-written public typings
  in `index.d.ts`, targeting Node.js 18+ and modern browsers.

[Unreleased]: https://github.com/tpt-solutions/tpt-microlog/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/tpt-solutions/tpt-microlog/releases/tag/v0.1.0
