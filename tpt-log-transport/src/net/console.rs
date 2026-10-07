//! Console/stdout transport — the zero-config default.
//!
//! - On a TTY: one colored, human-readable line per entry.
//! - When piped/redirected: strict JSON lines (one entry per line, same wire
//!   format as the `file` backend) so downstream tools can parse the output.
//!
//! Creating the transport prints a one-time startup banner (TTY only) so
//! developers always know where their logs are going.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use tpt_log_core::{
    CompressedBatch, Config, LogEntry, LogLevel, LogType, Transport, TransportError,
};

/// ANSI color sequences per level (TTY output only).
fn color_for(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Trace => "\x1b[90m",   // bright black
        LogLevel::Debug => "\x1b[36m",   // cyan
        LogLevel::Info => "\x1b[32m",    // green
        LogLevel::Warn => "\x1b[33m",    // yellow
        LogLevel::Error => "\x1b[31m",   // red
        LogLevel::Fatal => "\x1b[1;31m", // bold red
    }
}

const RESET: &str = "\x1b[0m";

/// Render one entry for human eyes: `12:34:56 INFO  message key=value`.
fn pretty(entry: &LogEntry) -> String {
    let secs = entry.timestamp / 1_000;
    let millis = entry.timestamp % 1_000;
    let (h, m, s) = ((secs / 3_600) % 24, (secs / 60) % 60, secs % 60);
    let mut line = format!(
        "{h:02}:{m:02}:{s:02}.{millis:03} {:5} {}",
        entry.level.to_string().to_ascii_uppercase(),
        entry.message
    );
    let mut pairs: Vec<String> = entry
        .metadata
        .iter()
        .map(|(k, v)| {
            let value = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("{k}={value}")
        })
        .collect();
    pairs.sort();
    if !pairs.is_empty() {
        line.push(' ');
        line.push_str(&pairs.join(" "));
    }
    line
}

/// Render one entry as a strict JSON line (same shape as the file backend).
fn json_line(entry: &LogEntry) -> String {
    serde_json::to_string(entry).unwrap_or_default()
}

/// Console/stdout transport.
pub struct ConsoleTransport {
    pretty: bool,
    banner: AtomicBool,
}

impl std::fmt::Debug for ConsoleTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsoleTransport")
            .field("pretty", &self.pretty)
            .finish()
    }
}

impl ConsoleTransport {
    /// Create a console transport. Output style is chosen automatically:
    /// pretty when stdout is a TTY, JSON lines otherwise.
    pub fn new() -> Self {
        Self {
            pretty: std::io::stdout().is_terminal(),
            banner: AtomicBool::new(true),
        }
    }

    /// Force a specific output style (used by tests and the `dev` preset).
    pub fn with_style(pretty: bool) -> Self {
        Self {
            pretty,
            banner: AtomicBool::new(false),
        }
    }

    /// Build from core config.
    pub fn from_config(_config: &Config) -> Self {
        Self::new()
    }
}

impl Default for ConsoleTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for ConsoleTransport {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        if self.banner.swap(false, Ordering::Relaxed) && self.pretty {
            eprintln!(
                "tpt-microlog: console backend active — logs are printed to stdout. \
                 Configure a backend in tpt-log.toml (or TPT_LOG_BACKEND) to ship them elsewhere."
            );
        }
        let entries = batch
            .decompress_entries()
            .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for entry in &entries {
            let line = if entry.log_type == LogType::Metric {
                // Metrics stay single-line in both modes.
                pretty(entry)
            } else if self.pretty {
                let level_color = color_for(entry.level);
                format!("{level_color}{}{RESET}", pretty(entry))
            } else {
                json_line(entry)
            };
            let _ = writeln!(out, "{line}");
        }
        let _ = out.flush();
        Ok(())
    }

    fn name(&self) -> &str {
        "console"
    }
}

/// Drain a length-prefixed frame from a reader. Kept here so the console
/// module mirrors the file transport's test helpers.
#[cfg(test)]
pub(crate) fn read_line(reader: &mut impl std::io::BufRead) -> std::io::Result<String> {
    let mut line = String::new();
    std::io::BufRead::read_line(reader, &mut line)?;
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::value_as_number;

    #[test]
    fn pretty_renders_level_message_and_sorted_metadata() {
        let mut e = LogEntry::new(LogLevel::Warn, "disk nearly full");
        e.timestamp = 3_600_000 + 61_000; // 01:01:01.000
        e.metadata.insert("zebra".into(), serde_json::json!(1));
        e.metadata.insert("alpha".into(), serde_json::json!("x"));
        let line = pretty(&e);
        assert!(
            line.starts_with("01:01:01.000 WARN  disk nearly full"),
            "{line}"
        );
        assert!(line.contains("alpha=x zebra=1"), "{line}");
    }

    #[test]
    fn json_line_round_trips_as_log_entry() {
        let e = LogEntry::new(LogLevel::Info, "hello").with("n", 7);
        let line = json_line(&e);
        let back: LogEntry = serde_json::from_str(&line).unwrap();
        assert_eq!(back.message, "hello");
        assert_eq!(back.metadata["n"], serde_json::json!(7));
    }

    #[test]
    fn value_helper_still_used_by_metrics() {
        let e = LogEntry::metric("cpu", 0.5, None);
        assert_eq!(e.metadata.get("value").and_then(value_as_number), Some(0.5));
    }

    #[test]
    fn read_line_helper_parses() {
        let mut reader = std::io::BufReader::new(&b"first\nsecond\n"[..]);
        assert_eq!(read_line(&mut reader).unwrap(), "first\n");
    }
}
