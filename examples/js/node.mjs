/**
 * tpt-microlog Node.js example (Node 18+ has global fetch).
 *
 * Build the WASM module and generate the JS glue first:
 *
 *   cargo build --release -p tpt-log-wasm --target wasm32-unknown-unknown
 *   wasm-bindgen --target web --out-dir tpt-log-wasm/dist \
 *     target/wasm32-unknown-unknown/release/tpt_log_wasm.wasm
 *
 * Run:
 *   node examples/js/node.mjs
 */

import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import init, { MicroLog } from '../../tpt-log-wasm/dist/tpt_log_wasm.js';

const wasmBytes = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), '../../tpt-log-wasm/dist/tpt_log_wasm_bg.wasm'),
);

// A tiny receiver that prints every batch it gets.
const server = createServer((req, res) => {
  if (req.method !== 'POST') {
    res.writeHead(404).end();
    return;
  }
  let body = '';
  req.on('data', (chunk) => (body += chunk));
  req.on('end', () => {
    console.log('received batch:', body.slice(0, 200), '...');
    res.writeHead(200).end('{}');
  });
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const port = server.address().port;

await init(wasmBytes);

const log = new MicroLog({
  endpoint: `http://127.0.0.1:${port}/logs`,
  service: 'node-worker',
  batchSize: 3,
  flushInterval: 5.0,
});

log.info('Job started', { jobId: 42, queue: 'emails' });
log.metric('queue_depth', 7, { queue: 'emails' });
log.warn('Retrying job', { jobId: 42, attempt: 2 });

const pending = await log.flush();
console.log('pending after flush:', pending); // 0 on success

await log.shutdown();
server.close();
