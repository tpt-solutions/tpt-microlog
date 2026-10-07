// Node smoke test for the wasm build. Usage (after `wasm-bindgen --target nodejs`):
//   node tests/smoke.mjs <path-to-generated-module.js>
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { resolve } from "node:path";

const require = createRequire(import.meta.url);
const { MicroLog, version } = require(resolve(process.argv[2]));

const bodies = [];
let status = 200;
globalThis.fetch = async (request) => {
  bodies.push({ url: request.url, text: await request.text() });
  return new Response(null, { status });
};

assert.ok(version().length > 0, "version() returns a string");

// Delivery + redaction.
const errors = [];
const log = new MicroLog({
  endpoint: "http://localhost:9/logs",
  batchSize: 100,
  flushInterval: 60,
  redactKeys: ["token"],
  redactBuiltin: true,
  onError: (kind, message) => errors.push([kind, message]),
});
log.info("mail alice@example.com", { token: "s3cret", n: 1 });
log.metric("load", 1.5, { page: "/" });
await log.flush();
assert.equal(bodies.length, 1, "one batch delivered");
const payload = JSON.parse(bodies[0].text);
const entry = payload.entries.find((e) => e.type === "log");
assert.equal(entry.message, "mail [REDACTED]");
assert.equal(entry.metadata.token, "[REDACTED]");
assert.equal(entry.metadata.n, 1);
assert.equal(log.stats().delivered_entries, 2);

// Permanent rejection is dropped and reported (no head-of-line blocking).
status = 400;
log.info("rejected");
await log.flush();
assert.equal(log.queued(), 0, "permanently rejected batch is not re-queued");
assert.ok(errors.some(([kind]) => kind === "delivery"), "onError got a delivery report");

// Transient failure keeps the batch buffered for retry.
status = 503;
log.info("retry me");
await log.flush();
assert.equal(log.queued(), 1, "transient failure re-queues");

log.dispose();
console.log("wasm smoke OK");
