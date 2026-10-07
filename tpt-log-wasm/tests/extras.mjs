// Tests for browser.js (error capture) and offline.js (offline queue).
// Pure JS: no wasm, no network, no IndexedDB (a memory store stands in).
import assert from "node:assert/strict";

import { describeError, installErrorCapture } from "../browser.js";
import { createMemoryStore, installOfflineQueue } from "../offline.js";

// ---------------------------------------------------------------- browser.js
{
  const handlers = new Map();
  const target = {
    addEventListener: (type, fn) => handlers.set(type, fn),
    removeEventListener: (type) => handlers.delete(type),
  };
  const logged = [];
  const log = {
    error: (m, md) => logged.push(["error", m, md]),
    warn: (m, md) => logged.push(["warn", m, md]),
  };
  let clock = 1000;
  const stop = installErrorCapture(log, {
    target,
    now: () => clock,
    dedupeWindowMs: 5000,
    metadata: { app: "shop" },
    captureResourceErrors: true,
  });

  const err = new Error("kaboom");
  handlers.get("error")({ message: "Uncaught Error: kaboom", error: err, filename: "app.js", lineno: 10, colno: 5 });
  assert.equal(logged[0][0], "error");
  assert.equal(logged[0][1], "kaboom");
  assert.equal(logged[0][2].kind, "uncaught");
  assert.equal(logged[0][2].app, "shop");
  assert.equal(logged[0][2].lineno, 10);
  assert.match(logged[0][2].stack, /kaboom/);

  // An error in a render loop is de-duplicated, then reported with a count.
  for (let i = 0; i < 20; i++) handlers.get("error")({ message: "x", error: err, filename: "app.js" });
  assert.equal(logged.length, 1, "repeats inside the window are suppressed");
  clock += 6000;
  handlers.get("error")({ message: "x", error: err, filename: "app.js" });
  assert.equal(logged.length, 2);
  assert.equal(logged[1][2].repeat_count, 20);

  handlers.get("unhandledrejection")({ reason: new TypeError("nope") });
  assert.equal(logged.at(-1)[1], "unhandled promise rejection: nope");
  assert.equal(logged.at(-1)[2].name, "TypeError");
  handlers.get("unhandledrejection")({ reason: "just a string" });
  assert.equal(logged.at(-1)[1], "unhandled promise rejection: just a string");

  // A failed <img>: an Event on the element, no message/error.
  handlers.get("error")({ target: { tagName: "IMG", src: "https://x/y.png" } });
  assert.equal(logged.at(-1)[0], "warn");
  assert.equal(logged.at(-1)[2].url, "https://x/y.png");

  // A throwing logger must never throw out of the handler.
  const stop2 = installErrorCapture(
    { error() { throw new Error("logger broke"); }, warn() {} },
    { target: { addEventListener() {}, removeEventListener() {} } },
  );
  stop2();

  stop();
  assert.equal(handlers.size, 0, "stop() removes the listeners");
  assert.deepEqual(describeError({ message: "m", name: "Custom" }).name, "Custom");
  assert.equal(describeError(42).message, "42");
}

// ---------------------------------------------------------------- offline.js
{
  const ENDPOINT = "https://logs.example.com/v1";
  const calls = [];
  let mode = "ok"; // ok | down | 503 | 400
  const fakeFetch = async (input, init) => {
    const body = init?.body ?? (await input.text?.());
    calls.push({ url: String(input.url ?? input), body, mode });
    if (mode === "down") throw new TypeError("Failed to fetch");
    if (mode === "503") return new Response("busy", { status: 503 });
    if (mode === "400") return new Response("bad", { status: 400 });
    return new Response("{}", { status: 200 });
  };

  const original = globalThis.fetch;
  const drops = [];
  const queue = installOfflineQueue({
    endpoint: ENDPOINT,
    store: createMemoryStore(),
    fetchImpl: fakeFetch,
    maxItems: 3,
    replayIntervalMs: 0,
    target: { addEventListener() {}, removeEventListener() {} },
    onDrop: (item, reason) => drops.push(reason),
  });
  assert.notEqual(globalThis.fetch, original, "fetch is wrapped");

  const send = (payload) =>
    globalThis.fetch(new Request(ENDPOINT, { method: "POST", body: JSON.stringify(payload), headers: { "content-type": "application/json" } }));

  // Healthy: passes straight through.
  let res = await send({ n: 1 });
  assert.equal(res.status, 200);
  assert.equal(await queue.size(), 0);

  // Offline: queued, and the engine is told 202.
  mode = "down";
  res = await send({ n: 2 });
  assert.equal(res.status, 202);
  mode = "503";
  res = await send({ n: 3 });
  assert.equal(res.status, 202, "server errors are queued too");
  assert.equal(await queue.size(), 2);

  // Permanent rejections are returned untouched and not queued.
  mode = "400";
  res = await send({ n: 4 });
  assert.equal(res.status, 400);
  assert.equal(await queue.size(), 2);

  // Other hosts are never touched.
  mode = "down";
  await assert.rejects(globalThis.fetch("https://elsewhere.example.com/x", { method: "POST", body: "{}" }));
  assert.equal(await queue.size(), 2);

  // Replay while still down keeps everything, in order.
  assert.equal(await queue.replay(), 0);
  assert.equal(await queue.size(), 2);

  // Back online: replay delivers oldest-first and empties the queue.
  mode = "ok";
  calls.length = 0;
  assert.equal(await queue.replay(), 2);
  assert.deepEqual(calls.map((c) => JSON.parse(c.body).n), [2, 3]);
  assert.equal(await queue.size(), 0);

  // A successful live send also drains what was parked meanwhile.
  mode = "down";
  await send({ n: 5 });
  mode = "ok";
  await send({ n: 6 });
  await new Promise((r) => setTimeout(r, 20));
  assert.equal(await queue.size(), 0);

  // Bounded: beyond maxItems the oldest are dropped and reported.
  mode = "down";
  for (let n = 10; n < 16; n++) await send({ n });
  assert.equal(await queue.size(), 3);
  assert.equal(drops.length, 3);
  assert.match(drops[0], /oldest dropped/);
  mode = "ok";
  calls.length = 0;
  await queue.replay();
  assert.deepEqual(calls.map((c) => JSON.parse(c.body).n), [13, 14, 15], "newest survive");

  queue.uninstall();
  assert.equal(globalThis.fetch, original, "uninstall restores fetch");
}

console.log("extras OK");
