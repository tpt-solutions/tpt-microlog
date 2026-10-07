# JavaScript / WebAssembly

The npm package is not published yet. Build it from a checkout:

```sh
cargo build --release -p tpt-log-wasm --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir tpt-log-wasm/dist \
  target/wasm32-unknown-unknown/release/tpt_log_wasm.wasm
```

The `wasm-bindgen` CLI version must match the one in `Cargo.lock` exactly.

The WebAssembly build is self-contained and a little different from the native
engine: payloads are plain JSON (no zstd), retries live in memory (there is no
disk WAL on the web), and delivery uses `fetch`. See the parity table in the
package README.

## Browser

The package is the `wasm-bindgen` *web* target, so initialise it once first:

```js
import init, { MicroLog } from 'tpt-microlog';

await init();
const log = new MicroLog({ endpoint: '/logs', service: 'web', batchSize: 50 });
log.info('Page viewed', { page: '/home' });
window.addEventListener('pagehide', () => log.flush());
```

Never put an API key in a browser bundle: send to your own endpoint and let the
server forward.

### Error capture and an offline queue

```js
import { installErrorCapture } from 'tpt-microlog/browser';
import { installOfflineQueue, createIndexedDbStore } from 'tpt-microlog/offline';

installErrorCapture(log);                                   // uncaught errors, rejections
installOfflineQueue({ endpoint: '/logs', store: createIndexedDbStore('app-logs') });
```

The queue survives a closed tab: failed sends are stored in IndexedDB and
replayed, oldest first, when the browser is back online.

## Node.js

```js
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { initSync, MicroLog } from 'tpt-microlog';

initSync({ module: readFileSync(createRequire(import.meta.url).resolve('tpt-microlog/dist/tpt_log_wasm_bg.wasm')) });
const log = new MicroLog({ endpoint: 'https://logs.example.com/v1' });
```

### pino

```js
import pino from 'pino';
import { createStream } from 'tpt-microlog/pino';

const logger = pino(createStream({ endpoint: 'https://logs.example.com/v1' }));
// or: pino.transport({ target: 'tpt-microlog/pino', options: { ... } })
```

## Edge runtimes

Cloudflare Workers and Vercel Edge import the `.wasm` as a pre-compiled module
and flush per request (`ctx.waitUntil(log.flush())`). See
`examples/js/cloudflare-worker` in the repository; this path has not been run on
a real edge runtime yet.

## Typed events

```ts
declare module 'tpt-microlog' {
  interface EventMap { order_placed: { orderId: number; totalCents: number } }
}
log.event('order_placed', { orderId: 7, totalCents: 1999 });   // names and fields are checked
```
