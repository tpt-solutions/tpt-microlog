// tpt-microlog <-> pino bridge (Node.js).
//
//   import pino from 'pino';
//   import { createStream } from 'tpt-microlog/pino';
//
//   // 1) in-process destination
//   const logger = pino(createStream({ endpoint: 'https://logs.example.com/v1', service: 'api' }));
//
//   // 2) pino worker-thread transport (pino resolves the default export)
//   const logger2 = pino(pino.transport({
//     target: 'tpt-microlog/pino',
//     options: { endpoint: 'https://logs.example.com/v1' },
//   }));
//
// Pino writes one JSON object per line. Each line becomes one tpt-microlog
// entry: `msg` is the message, `level` picks the severity and every other
// field becomes metadata. The entry timestamp is taken when the line reaches
// this stream (pino's own `time` is kept as metadata).

import { readFileSync } from 'node:fs';
import { Writable } from 'node:stream';
import { StringDecoder } from 'node:string_decoder';
import { fileURLToPath } from 'node:url';

import { initSync, MicroLog } from './dist/tpt_log_wasm.js';

let wasmReady = false;
function ensureWasm() {
  if (wasmReady) return;
  const bytes = readFileSync(fileURLToPath(new URL('./dist/tpt_log_wasm_bg.wasm', import.meta.url)));
  initSync({ module: bytes });
  wasmReady = true;
}

/** Pino numeric levels -> tpt-microlog levels (custom levels round down). */
export function mapLevel(level) {
  if (typeof level === 'string') {
    const upper = level.toUpperCase();
    return ['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR', 'FATAL'].includes(upper) ? upper : 'INFO';
  }
  if (typeof level !== 'number' || Number.isNaN(level)) return 'INFO';
  if (level <= 10) return 'TRACE';
  if (level <= 20) return 'DEBUG';
  if (level <= 30) return 'INFO';
  if (level <= 40) return 'WARN';
  if (level <= 50) return 'ERROR';
  return 'FATAL';
}

const RESERVED = new Set(['level', 'msg', 'message', 'v']);

/**
 * Create a Writable that forwards pino's JSON lines to a tpt-microlog logger.
 * `options` are the usual `MicroLog` options (`endpoint`, `backend`, `apiKey`,
 * `redactKeys`, `onError`, ...); pass `log` to reuse an existing instance
 * (it is then flushed, not shut down, when the stream ends).
 */
export function createStream(options = {}) {
  ensureWasm();
  const { log: shared, ...logOptions } = options;
  const log = shared ?? new MicroLog(logOptions);
  const report = typeof logOptions.onError === 'function' ? logOptions.onError : () => {};
  const decoder = new StringDecoder('utf8');
  let rest = '';

  function handleLine(line) {
    if (!line.trim()) return;
    let obj;
    try {
      obj = JSON.parse(line);
    } catch (err) {
      // Not pino JSON (e.g. a stray console.log): keep it rather than lose it.
      log.logWithLevel('INFO', line, { unparsed: true });
      report('pino', `non-JSON line forwarded as plain text: ${err.message}`);
      return;
    }
    if (obj === null || typeof obj !== 'object' || Array.isArray(obj)) {
      log.logWithLevel('INFO', line, { unparsed: true });
      return;
    }
    const metadata = {};
    for (const [key, value] of Object.entries(obj)) {
      if (!RESERVED.has(key)) metadata[key] = value;
    }
    const message = obj.msg ?? obj.message ?? obj.err?.message ?? '';
    log.logWithLevel(mapLevel(obj.level), String(message), metadata);
  }

  function consume(text) {
    rest += text;
    let newline;
    while ((newline = rest.indexOf('\n')) !== -1) {
      const line = rest.slice(0, newline);
      rest = rest.slice(newline + 1);
      handleLine(line);
    }
  }

  return new Writable({
    decodeStrings: true,
    write(chunk, _encoding, callback) {
      try {
        consume(decoder.write(chunk));
        callback();
      } catch (err) {
        callback(err);
      }
    },
    final(callback) {
      try {
        consume(decoder.end());
        if (rest) handleLine(rest);
        rest = '';
      } catch (err) {
        callback(err);
        return;
      }
      const done = shared ? log.flush() : log.shutdown();
      Promise.resolve(done).then(() => callback(), callback);
    },
  });
}

export default createStream;
