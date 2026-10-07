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

The package is the `wasm-bindgen` *web* target: initialise the module once
(`await init()`) before constructing a logger.

```js
import init, { MicroLog } from 'tpt-microlog';

await init();

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

In Node, initialise synchronously from the bundled `.wasm` file:

```js
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { initSync, MicroLog } from 'tpt-microlog';

const wasm = createRequire(import.meta.url).resolve('tpt-microlog/dist/tpt_log_wasm_bg.wasm');
initSync({ module: readFileSync(wasm) });

const log = new MicroLog({
  backend: 'custom',
  endpoint: 'https://my-api.com/logs',
  batchSize: 50,
  flushInterval: 2.0,
});

log.info('server booted', { port: 8080 });
await log.shutdown();
```

### pino (Node.js)

`tpt-microlog/pino` bridges [pino](https://getpino.io) to tpt-microlog and does
the WASM initialisation itself. Each pino JSON line becomes one entry: `msg` is
the message, `level` picks the severity, every other field is metadata.

```js
import pino from 'pino';
import { createStream } from 'tpt-microlog/pino';

// In-process destination
const logger = pino(createStream({ endpoint: 'https://logs.example.com/v1', service: 'api' }));

// ...or as a worker-thread transport (pino loads the default export)
const logger2 = pino(
  pino.transport({
    target: 'tpt-microlog/pino',
    options: { endpoint: 'https://logs.example.com/v1', redactKeys: ['password'] },
  }),
);

logger.info({ orderId: 5 }, 'order placed');
```

`options` are the normal `MicroLog` options (including `redactKeys`,
`redactBuiltin` and `onError`); pass `log` to reuse an existing instance.
Ending the stream flushes and shuts the logger down. The entry timestamp is the
time the line reaches the bridge; pino's own `time` field is kept as metadata.

### Browser error capture

```js
import { installErrorCapture } from 'tpt-microlog/browser';

const stop = installErrorCapture(log, { metadata: { release: '1.4.2' } });
```

Uncaught exceptions and unhandled promise rejections are logged at ERROR with
message, stack and location (failed `<img>`/`<script>` loads at WARN with
`captureResourceErrors: true`). Identical errors inside `dedupeWindowMs` are
folded and reported later with a `repeat_count`, so an error in a render loop
cannot flood the logger.

### Offline queue

```js
import { installOfflineQueue, createIndexedDbStore } from 'tpt-microlog/offline';

const queue = installOfflineQueue({
  endpoint: 'https://logs.example.com/v1',
  store: createIndexedDbStore('my-app-logs'),
});
```

Wraps `fetch` for requests to `endpoint`. When the network is down (or the
server answers 5xx/408/425/429) the request is stored and the engine is told it
was accepted, so logs survive a closed tab. Stored requests are replayed
oldest-first on `online`, every 30 s, and after the next successful send;
`maxItems` bounds the queue (oldest dropped, reported via `onDrop`). Entries
parked in the queue already count as delivered in `stats()`: use
`await queue.size()` for what is still waiting. Other 4xx responses are not
queued, since they can never succeed. The IndexedDB store targets browsers and
service workers; `createMemoryStore()` is available elsewhere.

### Edge runtimes (Cloudflare Workers, Vercel Edge)

Workers import `.wasm` as a pre-compiled module, so initialise synchronously
and flush per request (timers do not run between requests):

```js
import { initSync, MicroLog } from 'tpt-microlog';
import wasm from 'tpt-microlog/dist/tpt_log_wasm_bg.wasm';
initSync({ module: wasm });

const log = new MicroLog({ endpoint: env.LOG_ENDPOINT, apiKey: env.LOG_API_KEY });
// ...in the fetch handler:
ctx.waitUntil(log.flush());
```

See [`examples/js/cloudflare-worker`](../examples/js/cloudflare-worker). On
Vercel Edge import the module as `.../tpt_log_wasm_bg.wasm?module`. API keys
are safe in a Worker secret but must never be shipped to browsers.

> The error-capture and offline-queue logic is covered by Node tests with
> injected event targets and a memory store. The IndexedDB store, the Worker
> example and the Vercel variant have not been run in a real browser or edge
> runtime.

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
