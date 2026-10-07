// Smoke test for tpt-microlog/pino. Needs the web-target glue in ./dist
// (wasm-bindgen --target web --out-dir tpt-log-wasm/dist ...). When the real
// `pino` package is installed it is exercised too; otherwise only the
// line-protocol tests run.
import assert from 'node:assert/strict';
import { once } from 'node:events';

import { createStream, mapLevel } from '../pino.js';

const bodies = [];
globalThis.fetch = async (request) => {
  bodies.push(JSON.parse(await request.text()));
  return new Response(null, { status: 200 });
};
const delivered = () => bodies.flatMap((b) => b.entries);

// --- level mapping -------------------------------------------------------
assert.deepEqual(
  [10, 20, 30, 40, 50, 60, 35, 'warn', 'FATAL', undefined].map(mapLevel),
  ['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR', 'FATAL', 'WARN', 'WARN', 'FATAL', 'INFO'],
);

// --- line protocol: split chunks, multibyte, junk, redaction, final flush -
{
  const stream = createStream({
    endpoint: 'http://localhost:9/logs',
    batchSize: 100,
    flushInterval: 60,
    redactKeys: ['token'],
    minLevel: 'TRACE',
  });
  const line = JSON.stringify({ level: 50, time: 1, pid: 7, msg: 'héllo wörld', token: 'x', user: { id: 3 } }) + '\n';
  const bytes = Buffer.from(line);
  // Split in the middle of a multibyte character and of the line.
  const cut = bytes.indexOf(0xc3) + 1;
  stream.write(bytes.subarray(0, cut));
  stream.write(bytes.subarray(cut, cut + 10));
  stream.write(bytes.subarray(cut + 10));
  stream.write('{"level":30,"msg":"two"}\n{"level":20,"msg":"three"}\nnot json at all\n');
  stream.write('{"level":40,"msg":"no trailing newline"}');
  stream.end();
  await once(stream, 'finish');

  const entries = delivered();
  const byMsg = Object.fromEntries(entries.map((e) => [e.message, e]));
  assert.equal(byMsg['héllo wörld'].level, 'ERROR');
  assert.equal(byMsg['héllo wörld'].metadata.token, '[REDACTED]');
  assert.equal(byMsg['héllo wörld'].metadata.user.id, 3);
  assert.equal(byMsg['héllo wörld'].metadata.pid, 7);
  assert.equal('msg' in byMsg['héllo wörld'].metadata, false, 'reserved keys are not duplicated');
  assert.equal(byMsg.two.level, 'INFO');
  assert.equal(byMsg.three.level, 'DEBUG');
  assert.equal(byMsg['no trailing newline'].level, 'WARN');
  assert.equal(byMsg['not json at all'].metadata.unparsed, true, 'junk is kept, not dropped');
}

// --- real pino, when available -------------------------------------------
let pino;
try {
  pino = (await import('pino')).default;
} catch {
  console.log('pino not installed: skipping the real-pino check');
}
if (pino) {
  bodies.length = 0;
  const stream = createStream({ endpoint: 'http://localhost:9/logs', batchSize: 100, flushInterval: 60 });
  const logger = pino({ level: 'trace' }, stream);
  logger.info({ orderId: 5 }, 'order placed');
  logger.error(new Error('boom'), 'failed');
  logger.fatal('down');
  stream.end();
  await once(stream, 'finish');
  const byMsg = Object.fromEntries(delivered().map((e) => [e.message, e]));
  assert.equal(byMsg['order placed'].metadata.orderId, 5);
  assert.equal(byMsg.failed.level, 'ERROR');
  assert.match(byMsg.failed.metadata.err.message, /boom/);
  assert.equal(byMsg.down.level, 'FATAL');
}

console.log('pino bridge OK');
