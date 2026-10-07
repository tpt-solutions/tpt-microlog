import type { MicroLog } from './index';

export interface ErrorCaptureOptions {
  /** Where to listen (default `globalThis`). */
  target?: EventTarget;
  /** Also log failed `<img>`/`<script>`/`<link>` loads at WARN (default false). */
  captureResourceErrors?: boolean;
  /** Identical errors inside this window are folded (default 5000 ms). */
  dedupeWindowMs?: number;
  /** Clock override, for tests. */
  now?: () => number;
  /** Extra metadata added to every captured entry. */
  metadata?: Record<string, unknown>;
}

/** Describe an arbitrary thrown value as `{ message, name, stack? }`. */
export function describeError(value: unknown): { message: string; name: string; stack?: string };

/**
 * Log uncaught exceptions and unhandled promise rejections. Returns a
 * function that removes the listeners.
 */
export function installErrorCapture(
  log: Pick<MicroLog, 'error' | 'warn'>,
  options?: ErrorCaptureOptions,
): () => void;
