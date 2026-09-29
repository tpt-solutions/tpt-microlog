/**
 * tpt-microlog browser example — page telemetry to your own endpoint.
 *
 * Bundle with any bundler that understands wasm-bindgen `--target web` glue
 * (Vite, webpack 5, esbuild), or serve the generated `dist/` directly:
 *
 *   wasm-bindgen --target web --out-dir dist \
 *     target/wasm32-unknown-unknown/release/tpt_log_wasm.wasm
 *
 *   <script type="module">
 *     import init, { MicroLog } from './dist/tpt_log_wasm.js';
 *     await init();
 *     // ... use MicroLog as below
 *   </script>
 */

import { MicroLog, version } from 'tpt-microlog';

const log = new MicroLog({
  backend: 'custom',
  endpoint: 'https://my-api.com/logs',
  service: 'web-app',
  batchSize: 50,
  flushInterval: 2.0,
  minLevel: 'DEBUG',
});

console.log('tpt-microlog', version());

// Page-view tracking with structured metadata.
log.info('Page viewed', { page: '/home', userId: 123 });

// Performance metrics with tags.
log.metric('load_time', performance.now() / 1000, { page: '/home' });

// Errors with context.
try {
  JSON.parse('{oops}');
} catch (err) {
  log.error('Failed to parse cart', { error: String(err), page: '/cart' });
}

// On pagehide the wrapper flushes what is left with navigator.sendBeacon().
window.addEventListener('beforeunload', () => {
  // shutdown() is async; sendBeacon covers whatever does not make it.
  log.dispose();
});
