//! `log` facade backend (feature `log`).
//!
//! ```no_run
//! use std::sync::Arc;
//! use tpt_log_core::{Config, MicroLog};
//! use tpt_log_core::integrations::log_facade;
//!
//! let log = Arc::new(MicroLog::with_default_transport(Config::default()));
//! log_facade::init(log, ::log::LevelFilter::Info).unwrap();
//! ::log::info!("hello from the log crate");
//! ```

use std::sync::Arc;

use serde_json::Value;

use super::is_internal;
use crate::{LogLevel, MicroLog};

/// A `log::Log` implementation that forwards records to a [`MicroLog`].
pub struct MicroLogLogger {
    log: Arc<MicroLog>,
    max: ::log::LevelFilter,
}

impl MicroLogLogger {
    /// Wrap `log`; records above `max` are discarded.
    pub fn new(log: Arc<MicroLog>, max: ::log::LevelFilter) -> Self {
        Self { log, max }
    }
}

fn map_level(level: ::log::Level) -> LogLevel {
    match level {
        ::log::Level::Error => LogLevel::Error,
        ::log::Level::Warn => LogLevel::Warn,
        ::log::Level::Info => LogLevel::Info,
        ::log::Level::Debug => LogLevel::Debug,
        ::log::Level::Trace => LogLevel::Trace,
    }
}

impl ::log::Log for MicroLogLogger {
    fn enabled(&self, metadata: &::log::Metadata) -> bool {
        metadata.level() <= self.max && !is_internal(metadata.target())
    }

    fn log(&self, record: &::log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let mut meta: Vec<(&str, Value)> = vec![("target", record.target().into())];
        if let Some(m) = record.module_path() {
            meta.push(("module", m.into()));
        }
        if let Some(f) = record.file() {
            meta.push(("file", f.into()));
        }
        if let Some(l) = record.line() {
            meta.push(("line", l.into()));
        }
        self.log
            .log_with(map_level(record.level()), record.args().to_string(), meta);
    }

    fn flush(&self) {
        self.log.flush();
    }
}

/// Install a [`MicroLogLogger`] as the global `log` backend and set the
/// global max level. Fails if another logger is already installed.
pub fn init(log: Arc<MicroLog>, max: ::log::LevelFilter) -> Result<(), ::log::SetLoggerError> {
    ::log::set_boxed_logger(Box::new(MicroLogLogger::new(log, max)))?;
    ::log::set_max_level(max);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use ::log::Log;

    #[test]
    fn records_are_mapped_and_internal_targets_dropped() {
        let cfg = Config {
            wal_enabled: false,
            batch_size: 1,
            flush_interval_ms: 20,
            ..Config::default()
        };
        let (log, spy) = MicroLog::with_spy(cfg);
        let logger = MicroLogLogger::new(Arc::new(log), ::log::LevelFilter::Debug);

        let rec = |target: &str, level, msg: &str| {
            logger.log(
                &::log::Record::builder()
                    .target(target)
                    .level(level)
                    .module_path(Some("app::mod"))
                    .file(Some("app.rs"))
                    .line(Some(9))
                    .args(format_args!("{msg}"))
                    .build(),
            )
        };
        rec("app", ::log::Level::Warn, "careful");
        rec("app", ::log::Level::Trace, "too verbose"); // above max
        rec("hyper::client", ::log::Level::Info, "internal chatter");
        logger.flush();

        let entries = spy.entries();
        let messages: Vec<&str> = entries.iter().map(|e| e.message.as_str()).collect();
        assert!(messages.contains(&"careful"), "{messages:?}");
        assert!(!messages.contains(&"too verbose"));
        assert!(!messages.contains(&"internal chatter"));
        let e = entries.iter().find(|e| e.message == "careful").unwrap();
        assert_eq!(e.level, LogLevel::Warn);
        assert_eq!(e.metadata["line"], 9);
        assert_eq!(e.metadata["module"], "app::mod");
    }
}
