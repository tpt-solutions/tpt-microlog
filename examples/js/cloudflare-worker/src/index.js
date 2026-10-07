// tpt-microlog in a Cloudflare Worker (also the shape used by Vercel Edge).
//
// UNTESTED against the real runtime: the library is exercised under Node, and
// this file follows the documented module-import conventions of Wrangler.
//
// Build the package first (from the repository root):
//   cargo build --release -p tpt-log-wasm --target wasm32-unknown-unknown
//   wasm-bindgen --target web --out-dir tpt-log-wasm/dist \
//     target/wasm32-unknown-unknown/release/tpt_log_wasm.wasm
//
// Workers import `.wasm` files as pre-compiled `WebAssembly.Module`s, so the
// module is initialised synchronously, once per isolate.

import { initSync, MicroLog } from '../../../../tpt-log-wasm/dist/tpt_log_wasm.js';
import wasmModule from '../../../../tpt-log-wasm/dist/tpt_log_wasm_bg.wasm';

initSync({ module: wasmModule });

let log;
function logger(env) {
  // One logger per isolate. `flushInterval` is irrelevant at the edge: timers
  // do not run between requests, so every request flushes explicitly below.
  log ??= new MicroLog({
    endpoint: env.LOG_ENDPOINT,
    backend: env.LOG_BACKEND ?? 'custom',
    apiKey: env.LOG_API_KEY, // a Worker secret: safe here, never ship it to browsers
    service: 'edge-worker',
    batchSize: 100,
    redactKeys: ['authorization', 'cookie'],
  });
  return log;
}

export default {
  async fetch(request, env, ctx) {
    const l = logger(env);
    const started = Date.now();
    let response;
    try {
      response = new Response('hello from the edge');
      l.info('request handled', {
        method: request.method,
        path: new URL(request.url).pathname,
        colo: request.cf?.colo,
        ms: Date.now() - started,
      });
    } catch (err) {
      l.error('request failed', { message: String(err), stack: err?.stack });
      response = new Response('error', { status: 500 });
    }
    // Keep the isolate alive until the buffered logs have been sent.
    ctx.waitUntil(l.flush());
    return response;
  },
};
