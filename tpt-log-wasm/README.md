# tpt-log-wasm

WebAssembly binding for [tpt-microlog](https://github.com/tpt-solutions/tpt-microlog) — the microscopic telemetry & log router.

This crate is the [`wasm-bindgen`](https://rustwasm.github.io/wasm-bindgen/)
wrapper published to npm as `tpt-microlog`. It targets **browsers and Node.js**
via WebAssembly, and is deliberately **self-contained**: it does not depend on
`tpt-log-core`, because the native engine relies on threads, a disk-backed write
-ahead log, and a blocking HTTP stack that do not exist in a browser. The
in-browser pipeline — batching, level filtering, sampling, enrichment, retries —
is reimplemented in Rust with the same semantics.

- **API docs:** <https://docs.rs/tpt-log-wasm>
- **Repository:** <https://github.com/tpt-solutions/tpt-microlog/tree/master/tpt-log-wasm>
- **Published package:** <https://www.npmjs.com/package/tpt-microlog>

## Install

```sh
npm install tpt-microlog
```

Requires Node.js 18 or newer for the global `fetch` and `setTimeout` APIs.
The package ships as an ES module with bundled TypeScript declarations.

## Usage

### Browser

```js
import { MicroLog } from 'tpt-microlog';

const log = new MicroLog({
  backend: 'datadog',
  apiKey: 'your-api-key',
  service: 'web-frontend',
  batchSize: 50,
  flushInterval: 2.0,
});

log.info('Page viewed', { page: '/home', userId: 123 });
log.error('Checkout failed', { cartId: 'abc', retry: 1 });
log.metric('load_time', 1.23, { page: '/home' });

window.addEventListener('pagehide', () => log.flush());
```

### Node.js

```js
import { MicroLog } from 'tpt-microlog';

const log = new MicroLog({
  backend: 'custom',
  endpoint: 'https://my-api.com/logs',
  batchSize: 50,
  flushInterval: 2.0,
});

log.info('server booted', { port: 8080 });
await log.shutdown();
```

## API

| Member | Description |
| --- | --- |
| `new MicroLog(options?)` | Creates a logger; omitting `endpoint` yields a no-op logger |
| `trace` / `debug` / `info` / `warn` / `error` / `fatal` | Level logging with an optional metadata object |
| `logWithLevel(level, message, metadata?)` | Logging with an explicit level string |
| `metric(name, value, tags?)` | Numeric metric with optional tags |
| `span(name, traceId, spanId, parentSpanId?, durationUs?)` | Distributed-trace span |
| `flush()` | Delivers buffered entries; resolves with the number still queued |
| `shutdown()` | Stops the timer, flushes, and stops retrying (idempotent) |
| `dispose()` | Releases the background timer and unload hooks |
| `free()` | Frees the underlying WASM object, also invoked by `Symbol.dispose` |
| `queued()` | Entries currently buffered |
| `endpoint` | The configured delivery endpoint |
| `version()` | Library version string |

### Options

| Option | Default | Description |
| --- | --- | --- |
| `endpoint` | — | Delivery URL; omit for a no-op logger |
| `backend` | — | `"datadog"` sends `apiKey` as the `DD-API-KEY` header; other values are informational |
| `apiKey` | — | Secret token |
| `headers` | — | Extra request headers, e.g. `{ Authorization: 'Bearer ...' }` |
| `batchSize` | `50` | Flush when this many entries are buffered |
| `flushInterval` | `2` | Seconds between flushes |
| `service` | — | Service name attached to every entry's metadata |
| `minLevel` | `DEBUG` | Drop entries below this level |
| `sampleRate` | `1.0` | Fraction of entries kept (0.0–1.0) |
| `maxBuffer` | `10000` | Cap during outages; oldest entries are dropped |

## Platform differences

There is **no filesystem in the browser**, so the native WAL is unavailable and
retries are held in memory only:

- Failed batches stay buffered and are retried on the next flush tick, up to
  `maxBuffer` entries, and are dropped after `maxRetries` attempts.
- Delivery outcomes are classified like the native core: network errors, 5xx,
  and 408/425/429 retry; other 4xx drop the batch immediately (it can never
  succeed) instead of blocking every batch behind it.
- On `pagehide`, pending entries are delivered with `navigator.sendBeacon()`;
  when custom headers (e.g. Datadog's `DD-API-KEY`) are configured, a
  `fetch(..., { keepalive: true })` is used instead, because `sendBeacon`
  cannot set headers. The listener is removed by `dispose()`.
- Entries beyond `maxBuffer` are dropped oldest-first rather than growing
  without bound.

**Never ship secret API keys in browser bundles** — any key in a WASM build is
visible to every visitor. For browsers, proxy log intake through your own
backend (the generic `endpoint` + `headers` mode) or issue short-lived,
scoped ingest tokens.

## Feature parity vs the native core

| Feature | Native | WASM |
| --- | --- | --- |
| Batching (count / interval) | ✓ | ✓ (`batchSize`, `flushInterval`) |
| Delivery retry with backoff | ✓ persistent (WAL) | ✓ in-memory only |
| Crash/offline durability | ✓ WAL replay + `wal-first` | ✗ (page reload loses the buffer) |
| Transient/permanent error classification | ✓ | ✓ (4xx drop, 408/425/429/5xx retry) |
| Retry cap | `max_retries` | ✓ (`maxRetries`) |
| Level filtering | ✓ `min_level` + tags + regex | ✓ `minLevel` only |
| Sampling | ✓ global + per-level; WARN+ never sampled | ✓ `sampleRate` (global, incl. WARN+) |
| Enrichment (host/pid/service/env) | ✓ | `service` metadata only |
| Compression (zstd) | ✓ (`fast_mode` to disable) | ✗ (plain JSON) |
| Queue overflow policies | ✓ block / drop_newest / drop_oldest | drop-oldest at `maxBuffer` |
| `metric()` | ✓ (log + optional Prometheus/Metrics API) | ✓ (log stream only) |
| `span()` tracing | ✓ | ✓ (log stream only) |
| `stats()` / `flush()` | ✓ synchronous | ✓ (`flush()` is a Promise) |
| Default min level | `TRACE` (unfiltered) | `DEBUG` |

## Building

```sh
# from the repository root
wasm-pack build tpt-log-wasm --target web --out-dir dist --release
```

The generated `dist/` glue, `.d.ts` files, and `.wasm` binary are committed for
reference; the hand-written public typings live in `index.d.ts`. The compiled
module is roughly 0.8 MB before `wasm-opt`, against a < 3 MB target.

Run the Rust unit tests with:

```sh
cargo test -p tpt-log-wasm
```

## License

Dual-licensed under [MIT](https://github.com/tpt-solutions/tpt-microlog/blob/master/LICENSE-MIT) or [Apache-2.0](https://github.com/tpt-solutions/tpt-microlog/blob/master/LICENSE-APACHE),
at your option.
