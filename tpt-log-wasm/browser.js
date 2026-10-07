// Browser error auto-capture for tpt-microlog.
//
//   import init, { MicroLog } from 'tpt-microlog';
//   import { installErrorCapture } from 'tpt-microlog/browser';
//
//   await init();
//   const log = new MicroLog({ endpoint: '/logs', service: 'web' });
//   const stop = installErrorCapture(log);   // stop() removes the listeners
//
// Uncaught exceptions (`error`) and unhandled promise rejections
// (`unhandledrejection`) are logged at ERROR with the message, stack and
// location. Resource-load failures (`<img>`/`<script>` errors, which do not
// carry an `error` object) are logged at WARN when `captureResourceErrors` is
// on. Identical errors are de-duplicated within `dedupeWindowMs` so an error
// in a render loop cannot flood the logger.

/** Describe an arbitrary thrown value. */
export function describeError(value) {
  if (value instanceof Error) {
    return { message: value.message || String(value), name: value.name, stack: value.stack };
  }
  if (value && typeof value === 'object') {
    let message;
    try {
      message = value.message ?? JSON.stringify(value);
    } catch {
      message = String(value);
    }
    return { message: String(message), name: value.name ?? 'Object', stack: value.stack };
  }
  return { message: String(value), name: typeof value };
}

/**
 * Install `error` and `unhandledrejection` listeners on `target` (default
 * `globalThis`). Returns a function that removes them.
 *
 * @param {{ error: Function, warn: Function }} log a tpt-microlog logger
 * @param {object} [options]
 * @param {EventTarget} [options.target]
 * @param {boolean} [options.captureResourceErrors=false]
 * @param {number} [options.dedupeWindowMs=5000]
 * @param {() => number} [options.now]
 * @param {Record<string, unknown>} [options.metadata] added to every entry
 */
export function installErrorCapture(log, options = {}) {
  const target = options.target ?? globalThis;
  const windowMs = options.dedupeWindowMs ?? 5000;
  const now = options.now ?? (() => Date.now());
  const extra = options.metadata ?? {};
  const seen = new Map(); // dedupe key -> { at, suppressed }

  function shouldLog(key) {
    const t = now();
    const entry = seen.get(key);
    if (entry && t - entry.at < windowMs) {
      entry.suppressed += 1;
      return { log: false };
    }
    const repeats = entry ? entry.suppressed : 0;
    seen.set(key, { at: t, suppressed: 0 });
    // Bound memory: forget keys that have been quiet for a long time.
    if (seen.size > 200) {
      for (const [k, v] of seen) if (t - v.at > windowMs * 10) seen.delete(k);
    }
    return { log: true, repeats };
  }

  function report(level, message, details, kind) {
    const dedupe = shouldLog(`${kind}|${message}|${details.stack ?? details.filename ?? ''}`);
    if (!dedupe.log) return;
    const metadata = { ...extra, ...details, kind };
    if (dedupe.repeats) metadata.repeat_count = dedupe.repeats;
    try {
      log[level](message, metadata);
    } catch {
      // Logging must never throw out of an error handler.
    }
  }

  const onError = (event) => {
    // Resource failures bubble (in the capture phase) as plain Events whose
    // target is the element; script errors are ErrorEvents with a message.
    const isResource = event.target && event.target !== target && !event.message && !event.error;
    if (isResource) {
      if (!options.captureResourceErrors) return;
      const el = event.target;
      report('warn', `resource failed to load: ${el.src || el.href || el.tagName}`, {
        tag: el.tagName,
        url: el.src || el.href,
      }, 'resource');
      return;
    }
    const described = describeError(event.error ?? event.message);
    report('error', described.message, {
      name: described.name,
      stack: described.stack,
      filename: event.filename,
      lineno: event.lineno,
      colno: event.colno,
    }, 'uncaught');
  };

  const onRejection = (event) => {
    const described = describeError(event.reason);
    report('error', `unhandled promise rejection: ${described.message}`, {
      name: described.name,
      stack: described.stack,
    }, 'unhandledrejection');
  };

  target.addEventListener('error', onError, true);
  target.addEventListener('unhandledrejection', onRejection);
  return () => {
    target.removeEventListener('error', onError, true);
    target.removeEventListener('unhandledrejection', onRejection);
  };
}
