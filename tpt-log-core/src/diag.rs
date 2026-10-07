//! Internal diagnostics channel.
//!
//! Failures inside the logging pipeline (permanent delivery errors, WAL
//! problems, worker panics, bad config) must never be silent, yet a logger
//! cannot log through itself. They flow here instead: by default each message
//! goes to stderr; [`set_on_error`] installs a callback that receives them
//! all (e.g. to feed a host app's own alerting).

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

/// Callback signature: `(kind, message)`. `kind` is a short stable tag such
/// as `"delivery"`, `"wal"`, `"worker"`, `"config"`.
pub type OnError = Arc<dyn Fn(&str, &str) + Send + Sync>;

static HANDLER: RwLock<Option<OnError>> = RwLock::new(None);
static LAST: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// Identical messages are re-reported at most this often, so a dead backend
/// retried every few seconds does not flood stderr or the callback.
const REPEAT_WINDOW: Duration = Duration::from_secs(30);

/// Install (or clear, with `None`) the process-wide error callback. While a
/// callback is installed, nothing is written to stderr.
pub fn set_on_error(handler: Option<OnError>) {
    *HANDLER.write() = handler;
}

/// Report an internal failure. Never panics, never blocks on the callback's
/// panic (it is caught), and de-duplicates rapid repeats.
pub fn report(kind: &str, message: &str) {
    {
        let mut last = LAST.lock();
        let key = format!("{kind}:{message}");
        match &*last {
            Some((k, at)) if *k == key && at.elapsed() < REPEAT_WINDOW => return,
            _ => *last = Some((key, Instant::now())),
        }
    }
    let handler = HANDLER.read().clone();
    match handler {
        Some(cb) => {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb(kind, message)));
        }
        None => eprintln!("tpt-microlog: [{kind}] {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn callback_receives_deduplicated_reports_and_survives_panics() {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        set_on_error(Some(Arc::new(move |kind, msg| {
            assert_eq!(kind, "test-kind");
            assert_eq!(msg, "boom");
            n2.fetch_add(1, Ordering::SeqCst);
        })));
        report("test-kind", "boom");
        report("test-kind", "boom"); // deduplicated
        assert_eq!(n.load(Ordering::SeqCst), 1);

        set_on_error(Some(Arc::new(|_, _| panic!("bad callback"))));
        report("test-kind", "another"); // must not propagate the panic
        set_on_error(None);
    }
}
