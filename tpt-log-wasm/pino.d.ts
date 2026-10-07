import type { Writable } from 'node:stream';
import type { MicroLog, MicroLogOptions } from './index';

/** Options for {@link createStream}: the usual `MicroLog` options, or `log`. */
export interface PinoStreamOptions extends MicroLogOptions {
  /**
   * Reuse an existing logger instead of creating one. A shared logger is
   * flushed, not shut down, when the stream ends.
   */
  log?: MicroLog;
}

/** Map a pino level (numeric or label) to a tpt-microlog level name. */
export function mapLevel(
  level: number | string | undefined,
): 'TRACE' | 'DEBUG' | 'INFO' | 'WARN' | 'ERROR' | 'FATAL';

/**
 * A Writable that turns pino's JSON lines into tpt-microlog entries. Use it
 * as a pino destination, or as the default export of a pino transport
 * (`pino.transport({ target: 'tpt-microlog/pino', options })`).
 */
export function createStream(options?: PinoStreamOptions): Writable;

export default createStream;
