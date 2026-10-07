/** A request parked while the network was unavailable. */
export interface QueuedRequest {
  id?: number;
  url: string;
  headers: Record<string, string>;
  body: string;
  queuedAt: number;
}

/** Persistence for queued requests. */
export interface QueueStore {
  add(item: Omit<QueuedRequest, 'id'>): Promise<unknown>;
  /** All items, oldest first. */
  all(): Promise<QueuedRequest[]>;
  remove(id: number): Promise<void>;
  count(): Promise<number>;
}

/** Non-persistent store for tests and non-browser runtimes. */
export function createMemoryStore(): QueueStore;

/** IndexedDB-backed store (browsers and service workers). */
export function createIndexedDbStore(dbName?: string, idb?: IDBFactory): QueueStore;

export interface OfflineQueueOptions {
  /** Requests whose URL starts with this are queued on failure. */
  endpoint: string;
  /** Defaults to an in-memory store; use `createIndexedDbStore()` in browsers. */
  store?: QueueStore;
  /** Oldest requests are dropped beyond this many (default 500). */
  maxItems?: number;
  /** Periodic replay interval in ms; 0 disables it (default 30000). */
  replayIntervalMs?: number;
  /** The real `fetch` (default: the current global one). */
  fetchImpl?: typeof fetch;
  /** Where the `online` event is heard (default `globalThis`). */
  target?: EventTarget;
  /** Called when a queued request is discarded. */
  onDrop?: (item: QueuedRequest, reason: string) => void;
}

export interface OfflineQueue {
  /** Resend queued requests oldest-first; resolves to how many were sent. */
  replay(): Promise<number>;
  /** Requests still waiting. */
  size(): Promise<number>;
  /** Restore the original `fetch` and stop the timer/listeners. */
  uninstall(): void;
}

/**
 * Wrap `fetch` so POSTs to `endpoint` survive outages: failures (network
 * errors, 5xx, 408/425/429) are stored and answered with a synthetic 202;
 * the stored requests are replayed when connectivity returns.
 */
export function installOfflineQueue(options: OfflineQueueOptions): OfflineQueue;
