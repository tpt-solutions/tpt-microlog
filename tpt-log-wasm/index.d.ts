/**
 * tpt-microlog — the microscopic telemetry & log router (WebAssembly build).
 *
 * Delivery uses the browser `fetch` API (or global `fetch` under Node 18+).
 * Failed batches stay buffered and retry on the next tick; there is no disk
 * WAL in WASM builds. On `pagehide`, pending entries are flushed via
 * `navigator.sendBeacon()`.
 *
 * @packageDocumentation
 */

/** Structured metadata attached to an entry (any JSON-serializable object). */
export type Metadata = Record<string, unknown> | undefined | null;

/** Configuration for {@link MicroLog}. */
export interface MicroLogOptions {
  /** Delivery URL. Omit for a no-op logger. */
  endpoint?: string;
  /**
   * Backend label. `"datadog"` sends `apiKey` as the `DD-API-KEY` header;
   * any other value is informational only.
   */
  backend?: string;
  /** API key (used with `backend: "datadog"`). */
  apiKey?: string;
  /** Extra request headers, e.g. `{ Authorization: "Bearer ..." }`. */
  headers?: Record<string, string>;
  /** Flush when this many entries are buffered (default 50). */
  batchSize?: number;
  /** Flush at least this often, in seconds (default 2). */
  flushInterval?: number;
  /** Service name attached to every entry's metadata. */
  service?: string;
  /** Drop entries below this level (default `"DEBUG"`). */
  minLevel?: 'TRACE' | 'DEBUG' | 'INFO' | 'WARN' | 'ERROR' | 'FATAL';
  /** Fraction of entries kept, 0.0–1.0 (default 1.0). */
  sampleRate?: number;
  /** Cap on buffered entries during outages; oldest are dropped (default 10 000). */
  maxBuffer?: number;
}

/**
 * Ultra-lightweight browser telemetry & log router.
 *
 * @example
 * ```ts
 * import { MicroLog } from 'tpt-microlog';
 *
 * const log = new MicroLog({
 *   backend: 'datadog',
 *   apiKey: 'your-api-key',
 *   batchSize: 50,
 *   flushInterval: 2.0,
 * });
 * log.info('Page viewed', { page: '/home', userId: 123 });
 * log.metric('load_time', 1.23, { page: '/home' });
 * await log.shutdown();
 * ```
 */
export class MicroLog {
  constructor(options?: MicroLogOptions);

  /** The delivery endpoint, when configured. */
  get endpoint(): string | undefined;

  /** Entries currently buffered. */
  queued(): number;

  /** Log a message at an explicit level (`"TRACE"`..`"FATAL"`). */
  logWithLevel(level: string, message: string, metadata?: Metadata): void;
  trace(message: string, metadata?: Metadata): void;
  debug(message: string, metadata?: Metadata): void;
  info(message: string, metadata?: Metadata): void;
  warn(message: string, metadata?: Metadata): void;
  error(message: string, metadata?: Metadata): void;
  fatal(message: string, metadata?: Metadata): void;

  /** Record a numeric metric with optional tags. */
  metric(name: string, value: number, tags?: Metadata): void;

  /** Record a distributed-trace span. */
  span(
    name: string,
    traceId: string,
    spanId: string,
    parentSpanId?: string | null,
    durationUs?: bigint | number,
  ): void;

  /**
   * Deliver buffered entries immediately. Resolves with the number of
   * entries still queued (nonzero means delivery failed and will retry).
   */
  flush(): Promise<number>;

  /** Stop the timer, flush, and stop retrying. Idempotent. */
  shutdown(): Promise<number>;

  /** Release the background timer and unload hooks. */
  dispose(): void;

  /** Free the underlying WASM object (called by `Symbol.dispose`). */
  free(): void;
}

/** Library version. */
export function version(): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;
export interface InitOutput {
  readonly memory: WebAssembly.Memory;
  [key: string]: unknown;
}
export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the WASM module synchronously (when the bytes are local).
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * Initializes the WASM module. Bundlers usually handle this automatically
 * via the default export of the generated glue.
 */
export default function __wbg_init(
  module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>,
): Promise<InitOutput>;
