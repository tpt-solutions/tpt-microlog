//! Integrations with the Rust logging ecosystem: a panic hook, the
//! [`mlog!`](crate::mlog) structured-event macro, a `tracing` layer (feature
//! `tracing`) and a `log` facade backend (feature `log`).
//!
//! All of them take an `Arc<MicroLog>`: dropping a `MicroLog` shuts it down,
//! so the integrations share ownership instead of cloning.

use std::sync::Arc;

use serde_json::Value;

use crate::{LogLevel, MicroLog};

#[cfg(feature = "log")]
pub mod log_facade;
#[cfg(feature = "tracing")]
pub mod tracing_layer;

#[doc(hidden)]
pub mod __private {
    pub use serde_json::{json, Value};
}

/// Log a structured event: `mlog!(log, Info, "user logged in", user_id = 123)`.
///
/// `log` is anything with a `log_with` method (a [`MicroLog`], `&MicroLog` or
/// `Arc<MicroLog>`), the level is a [`LogLevel`] variant name, and each
/// `key = value` pair becomes metadata (the value may be any expression
/// `serde_json::json!` accepts).
#[macro_export]
macro_rules! mlog {
    ($log:expr, $lvl:ident, $msg:expr $(,)?) => {
        $log.log_with(
            $crate::LogLevel::$lvl,
            $msg,
            ::std::iter::empty::<(::std::string::String, $crate::integrations::__private::Value)>(),
        )
    };
    ($log:expr, $lvl:ident, $msg:expr, $($key:ident = $val:expr),+ $(,)?) => {
        $log.log_with(
            $crate::LogLevel::$lvl,
            $msg,
            [$((stringify!($key), $crate::integrations::__private::json!($val))),+],
        )
    };
}

/// Targets whose records must never be forwarded: they are produced by the
/// delivery path itself (HTTP/TLS/gRPC stacks), so forwarding them would feed
/// every batch's own network chatter back into the next batch forever.
#[cfg(any(feature = "tracing", feature = "log", test))]
const INTERNAL_TARGET_PREFIXES: &[&str] = &[
    "tpt_log", "hyper", "reqwest", "h2", "rustls", "tonic", "tower", "tokio", "mio", "want",
];

/// Whether a record from `target` (or emitted on the current thread) belongs
/// to the logging pipeline and must be skipped.
#[cfg(any(feature = "tracing", feature = "log", test))]
pub(crate) fn is_internal(target: &str) -> bool {
    // Every tpt-microlog crate (tpt_log_core, tpt_log_transport, ...).
    if target.starts_with("tpt_log_") {
        return true;
    }
    if INTERNAL_TARGET_PREFIXES
        .iter()
        .any(|p| target == *p || target.starts_with(&format!("{p}::")))
    {
        return true;
    }
    std::thread::current()
        .name()
        .is_some_and(|n| n.starts_with("tpt-microlog"))
}

/// Install a process-wide panic hook that reports every panic as a FATAL
/// entry (message, location, thread, backtrace when `RUST_BACKTRACE` is set),
/// flushes (bounded by `shutdown_timeout_ms`) and then runs the previously
/// installed hook, so default panic output and abort behaviour are unchanged.
pub fn install_panic_hook(log: Arc<MicroLog>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // A panic inside the pipeline itself must not re-enter it.
        let on_pipeline_thread = std::thread::current()
            .name()
            .is_some_and(|n| n.starts_with("tpt-microlog"));
        if !on_pipeline_thread {
            let payload = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".into());
            let mut meta: Vec<(&str, Value)> = vec![(
                "thread",
                Value::String(std::thread::current().name().unwrap_or("<unnamed>").into()),
            )];
            if let Some(loc) = info.location() {
                meta.push((
                    "location",
                    Value::String(format!("{}:{}:{}", loc.file(), loc.line(), loc.column())),
                ));
            }
            let backtrace = std::backtrace::Backtrace::capture();
            if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
                meta.push(("backtrace", Value::String(backtrace.to_string())));
            }
            log.log_with(LogLevel::Fatal, format!("panic: {payload}"), meta);
            log.flush();
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    fn spy_log() -> (Arc<MicroLog>, Arc<crate::transport::SpyTransport>) {
        let cfg = Config {
            wal_enabled: false,
            batch_size: 1,
            flush_interval_ms: 20,
            ..Config::default()
        };
        let (log, spy) = MicroLog::with_spy(cfg);
        (Arc::new(log), spy)
    }

    #[test]
    fn mlog_macro_emits_structured_metadata() {
        let (log, spy) = spy_log();
        mlog!(
            log,
            Info,
            "user logged in",
            user_id = 123,
            ip = "1.2.3.4",
            tags = ["a", "b"]
        );
        mlog!(log, Warn, "bare");
        log.flush();
        let entries = spy.entries();
        let e = entries
            .iter()
            .find(|e| e.message == "user logged in")
            .unwrap();
        assert_eq!(e.metadata["user_id"], 123);
        assert_eq!(e.metadata["ip"], "1.2.3.4");
        assert_eq!(e.metadata["tags"][1], "b");
        assert!(entries.iter().any(|e| e.message == "bare"));
    }

    #[test]
    fn internal_targets_are_filtered() {
        assert!(is_internal("hyper::proto::h1"));
        assert!(is_internal("reqwest"));
        assert!(is_internal("tpt_log_core::microlog"));
        assert!(!is_internal("my_app::checkout"));
        assert!(!is_internal("hyperion")); // prefix must end at a path boundary
    }

    // The panic hook is process-global, so exactly one test installs it.
    #[test]
    fn panic_hook_reports_fatal_and_chains() {
        let (log, spy) = spy_log();
        let chained = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = chained.clone();
        std::panic::set_hook(Box::new(move |_| {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        install_panic_hook(log.clone());
        let _ = std::thread::Builder::new()
            .name("boom-thread".into())
            .spawn(|| panic!("kaboom {}", 42))
            .unwrap()
            .join();
        let _ = std::panic::take_hook(); // restore default for other tests
        assert!(
            chained.load(std::sync::atomic::Ordering::SeqCst),
            "previous hook ran"
        );
        let entries = spy.entries();
        let e = entries
            .iter()
            .find(|e| e.message == "panic: kaboom 42")
            .expect("panic entry delivered");
        assert_eq!(e.level, LogLevel::Fatal);
        assert_eq!(e.metadata["thread"], "boom-thread");
        assert!(e.metadata["location"]
            .as_str()
            .unwrap()
            .contains("integrations.rs"));
    }
}
