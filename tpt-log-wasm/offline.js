// Offline queue for browser/edge logging.
//
//   import init, { MicroLog } from 'tpt-microlog';
//   import { installOfflineQueue, createIndexedDbStore } from 'tpt-microlog/offline';
//
//   await init();
//   const queue = installOfflineQueue({
//     endpoint: 'https://logs.example.com/v1',
//     store: createIndexedDbStore('tpt-microlog-queue'),
//   });
//   const log = new MicroLog({ endpoint: 'https://logs.example.com/v1' });
//
// The in-memory WASM engine already retries, but a closed tab loses whatever
// it was holding. This wraps `fetch` for requests to `endpoint`: when the
// network is down (or the server answers 5xx/408/425/429) the request body is
// persisted in a store (IndexedDB in browsers) and the engine is told it was
// accepted (`202`) so it does not pile up retries in memory. The stored
// requests are replayed oldest-first when the browser comes back `online`,
// periodically, and on the next successful send.
//
// Trade-off to know about: entries parked here already count as "delivered" in
// the engine's `stats()`; use `queue.size()` for what is still waiting.
// Permanent rejections (other 4xx) are not queued: they can never succeed.

const TRANSIENT_STATUS = new Set([0, 408, 425, 429]);

function isTransient(status) {
  return TRANSIENT_STATUS.has(status) || (status >= 500 && status <= 599);
}

/** A non-persistent store, for tests and non-browser runtimes. */
export function createMemoryStore() {
  let next = 1;
  const items = new Map();
  return {
    async add(item) {
      const id = next++;
      items.set(id, { ...item, id });
      return id;
    },
    async all() {
      return [...items.values()].sort((a, b) => a.id - b.id);
    },
    async remove(id) {
      items.delete(id);
    },
    async count() {
      return items.size;
    },
  };
}

/** IndexedDB-backed store (browsers, service workers). */
export function createIndexedDbStore(dbName = 'tpt-microlog-queue', idb = globalThis.indexedDB) {
  if (!idb) throw new Error('IndexedDB is not available in this environment');
  const STORE = 'requests';
  let dbPromise;
  const open = () =>
    (dbPromise ??= new Promise((resolve, reject) => {
      const request = idb.open(dbName, 1);
      request.onupgradeneeded = () => {
        request.result.createObjectStore(STORE, { keyPath: 'id', autoIncrement: true });
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    }));
  const run = async (mode, fn) => {
    const db = await open();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(STORE, mode);
      const result = fn(tx.objectStore(STORE));
      tx.oncomplete = () => resolve(result.result);
      tx.onerror = () => reject(tx.error);
      tx.onabort = () => reject(tx.error);
    });
  };
  return {
    add: (item) => run('readwrite', (s) => s.add(item)),
    all: () => run('readonly', (s) => s.getAll()),
    remove: (id) => run('readwrite', (s) => s.delete(id)),
    count: () => run('readonly', (s) => s.count()),
  };
}

/**
 * Wrap `fetch` so requests to `endpoint` survive outages.
 *
 * @param {object} options
 * @param {string} options.endpoint requests whose URL starts with this are queued
 * @param {object} [options.store] defaults to an in-memory store
 * @param {number} [options.maxItems=500] oldest are dropped beyond this
 * @param {number} [options.replayIntervalMs=30000] 0 disables the timer
 * @param {typeof fetch} [options.fetchImpl] the real fetch (default: current global)
 * @param {EventTarget} [options.target] where `online` is heard (default globalThis)
 * @param {(item: object, reason: string) => void} [options.onDrop]
 * @returns {{ replay(): Promise<number>, size(): Promise<number>, uninstall(): void }}
 */
export function installOfflineQueue(options) {
  const { endpoint } = options;
  if (!endpoint) throw new Error('installOfflineQueue: `endpoint` is required');
  const store = options.store ?? createMemoryStore();
  const maxItems = options.maxItems ?? 500;
  const realFetch = options.fetchImpl ?? globalThis.fetch.bind(globalThis);
  const target = options.target ?? globalThis;
  const onDrop = options.onDrop ?? (() => {});
  let replaying = null;

  async function trim() {
    const items = await store.all();
    for (const item of items.slice(0, Math.max(0, items.length - maxItems))) {
      await store.remove(item.id);
      onDrop(item, 'queue full: oldest dropped');
    }
  }

  async function replay() {
    if (replaying) return replaying;
    replaying = (async () => {
      let sent = 0;
      for (const item of await store.all()) {
        let response;
        try {
          response = await realFetch(item.url, { method: 'POST', headers: item.headers, body: item.body });
        } catch {
          break; // still offline: keep order, try again later
        }
        if (response.ok || !isTransient(response.status)) {
          if (!response.ok) onDrop(item, `rejected by the server (${response.status})`);
          await store.remove(item.id);
          sent += 1;
        } else {
          break;
        }
      }
      return sent;
    })().finally(() => {
      replaying = null;
    });
    return replaying;
  }

  const wrapped = async function (input, init) {
    const request = input instanceof Request ? input : null;
    const url = request ? request.url : String(input);
    const method = (request?.method ?? init?.method ?? 'GET').toUpperCase();
    if (method !== 'POST' || !url.startsWith(endpoint)) return realFetch(input, init);

    // Read the body up front: it can only be consumed once.
    const body = request ? await request.clone().text() : init?.body;
    const headers = {};
    for (const [k, v] of new Headers(request ? request.headers : init?.headers)) headers[k] = v;
    const bodyText = typeof body === 'string' ? body : body ? new TextDecoder().decode(body) : '';

    let response;
    try {
      response = await realFetch(url, { method: 'POST', headers, body: bodyText });
    } catch {
      response = null;
    }
    if (response && !isTransient(response.status)) {
      if (response.ok) replay(); // network is back: drain older requests
      return response;
    }
    await store.add({ url, headers, body: bodyText, queuedAt: Date.now() });
    await trim();
    return new Response(null, { status: 202, statusText: 'Queued offline' });
  };

  const previousFetch = globalThis.fetch;
  globalThis.fetch = wrapped;
  const onOnline = () => {
    replay();
  };
  target.addEventListener?.('online', onOnline);
  const timer =
    (options.replayIntervalMs ?? 30000) > 0
      ? setInterval(() => replay(), options.replayIntervalMs ?? 30000)
      : null;
  timer?.unref?.();

  return {
    replay,
    size: () => store.count(),
    uninstall() {
      if (globalThis.fetch === wrapped) globalThis.fetch = previousFetch;
      target.removeEventListener?.('online', onOnline);
      if (timer) clearInterval(timer);
    },
  };
}
