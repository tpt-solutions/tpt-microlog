//! W3C trace-context correlation.
//!
//! Parse an incoming `traceparent` header, open a span, and every log entry
//! emitted on that thread while the span is open automatically carries
//! `trace_id` and `span_id`; dropping the span records it (with its duration)
//! as a trace-span entry.
//!
//! ```
//! use std::sync::Arc;
//! use tpt_log_core::{Config, MicroLog};
//! use tpt_log_core::trace::{start_span, TraceContext};
//!
//! let log = Arc::new(MicroLog::with_default_transport(Config::default()));
//! let incoming = TraceContext::parse_traceparent(
//!     "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
//! );
//! {
//!     let span = start_span(&log, "handle_request", incoming.as_ref());
//!     log.info("inside the request"); // carries trace_id / span_id
//!     let _header_for_downstream = span.context().to_traceparent();
//! } // span recorded here
//! ```
//!
//! The current context is thread-local, so it follows ordinary threaded code.
//! Async runtimes that move tasks between threads should pass the context
//! explicitly (see [`TraceContext::child`]) or use the `tracing` layer, which
//! reads the span the task is actually in.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Instant;

use rand::Rng;

use crate::MicroLog;

/// A W3C trace-context identity: 128-bit trace id, 64-bit span id, sampled flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceContext {
    /// 32 lower-case hex characters, never all zero.
    pub trace_id: String,
    /// 16 lower-case hex characters, never all zero.
    pub span_id: String,
    /// The `sampled` bit of the trace flags.
    pub sampled: bool,
}

fn random_hex(bytes: usize) -> String {
    let mut rng = rand::thread_rng();
    loop {
        let raw: Vec<u8> = (0..bytes).map(|_| rng.gen()).collect();
        if raw.iter().any(|b| *b != 0) {
            return raw.iter().map(|b| format!("{b:02x}")).collect();
        }
    }
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && s.bytes().any(|b| b != b'0')
}

impl TraceContext {
    /// A fresh trace (new trace id and span id, sampled).
    pub fn new_root() -> Self {
        Self {
            trace_id: random_hex(16),
            span_id: random_hex(8),
            sampled: true,
        }
    }

    /// A child span in the same trace.
    pub fn child(&self) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            span_id: random_hex(8),
            sampled: self.sampled,
        }
    }

    /// Parse a `traceparent` header (`00-<trace-id>-<parent-id>-<flags>`).
    /// Returns `None` for anything malformed, including all-zero ids and the
    /// reserved version `ff`; unknown *higher* versions are accepted as long as
    /// the first four fields parse, as the spec requires.
    pub fn parse_traceparent(header: &str) -> Option<Self> {
        let mut parts = header.trim().split('-');
        let (version, trace_id, parent_id, flags) =
            (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        let version_ok = version.len() == 2
            && version
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && version != "ff";
        // Version 00 has exactly four fields; later versions may append more.
        if !version_ok || (version == "00" && parts.next().is_some()) {
            return None;
        }
        if !is_hex(trace_id, 32) || !is_hex(parent_id, 16) || flags.len() != 2 {
            return None;
        }
        let flags = u8::from_str_radix(flags, 16).ok()?;
        Some(Self {
            trace_id: trace_id.to_string(),
            span_id: parent_id.to_string(),
            sampled: flags & 1 == 1,
        })
    }

    /// The header to send downstream so they continue this trace.
    pub fn to_traceparent(&self) -> String {
        format!(
            "00-{}-{}-{:02x}",
            self.trace_id,
            self.span_id,
            u8::from(self.sampled)
        )
    }
}

thread_local! {
    static CURRENT: RefCell<Option<TraceContext>> = const { RefCell::new(None) };
}

/// The trace context active on this thread, if any.
pub fn current() -> Option<TraceContext> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Restores the previous thread-local context when dropped.
pub struct ContextGuard {
    previous: Option<TraceContext>,
}

/// Make `ctx` the current context for this thread until the guard is dropped.
pub fn enter(ctx: TraceContext) -> ContextGuard {
    let previous = CURRENT.with(|c| c.borrow_mut().replace(ctx));
    ContextGuard { previous }
}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT.with(|c| *c.borrow_mut() = previous);
    }
}

/// An open span. Entries logged on this thread while it lives are correlated;
/// dropping it records the span and its duration.
pub struct SpanGuard {
    log: Arc<MicroLog>,
    name: String,
    ctx: TraceContext,
    parent_span_id: Option<String>,
    started: Instant,
    _guard: ContextGuard,
}

impl SpanGuard {
    /// This span's identity (use [`TraceContext::to_traceparent`] downstream).
    pub fn context(&self) -> &TraceContext {
        &self.ctx
    }
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        self.log.span(
            std::mem::take(&mut self.name),
            self.ctx.trace_id.clone(),
            self.ctx.span_id.clone(),
            self.parent_span_id.take(),
            self.started.elapsed().as_micros() as u64,
        );
    }
}

/// Open a span named `name`. With `parent` (an incoming `traceparent`, or an
/// explicit context) the span continues that trace; otherwise it continues the
/// thread's current span, or starts a new trace.
pub fn start_span(log: &Arc<MicroLog>, name: &str, parent: Option<&TraceContext>) -> SpanGuard {
    let parent_ctx = parent.cloned().or_else(current);
    let (ctx, parent_span_id) = match &parent_ctx {
        Some(p) => (p.child(), Some(p.span_id.clone())),
        None => (TraceContext::new_root(), None),
    };
    SpanGuard {
        log: log.clone(),
        name: name.to_string(),
        ctx: ctx.clone(),
        parent_span_id,
        started: Instant::now(),
        _guard: enter(ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, LogType};

    const SAMPLE: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn traceparent_round_trips() {
        let ctx = TraceContext::parse_traceparent(SAMPLE).unwrap();
        assert_eq!(ctx.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(ctx.span_id, "00f067aa0ba902b7");
        assert!(ctx.sampled);
        assert_eq!(ctx.to_traceparent(), SAMPLE);
        let unsampled = TraceContext::parse_traceparent(&SAMPLE.replace("-01", "-00")).unwrap();
        assert!(!unsampled.sampled);
    }

    #[test]
    fn malformed_traceparents_are_rejected() {
        for bad in [
            "",
            "garbage",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7", // no flags
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01", // upper case
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01", // zero trace
            "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01", // zero span
            "00-4bf92f3577b34da6a3ce929d0e0e47-00f067aa0ba902b7-01", // short trace id
            "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01", // reserved version
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra", // v00 is 4 fields
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-zz",
        ] {
            assert!(
                TraceContext::parse_traceparent(bad).is_none(),
                "accepted {bad:?}"
            );
        }
        // A future version may carry extra fields.
        assert!(TraceContext::parse_traceparent(
            &SAMPLE.replacen("00-", "01-", 1).replace("-01", "-01-x")
        )
        .is_some());
    }

    #[test]
    fn generated_ids_are_valid_and_children_share_the_trace() {
        let root = TraceContext::new_root();
        assert!(is_hex(&root.trace_id, 32) && is_hex(&root.span_id, 16));
        let child = root.child();
        assert_eq!(child.trace_id, root.trace_id);
        assert_ne!(child.span_id, root.span_id);
        assert!(TraceContext::parse_traceparent(&child.to_traceparent()).is_some());
    }

    #[test]
    fn spans_correlate_entries_nest_and_restore() {
        let cfg = Config {
            wal_enabled: false,
            batch_size: 1,
            flush_interval_ms: 20,
            ..Config::default()
        };
        let (log, spy) = MicroLog::with_spy(cfg);
        let log = Arc::new(log);
        let incoming = TraceContext::parse_traceparent(SAMPLE).unwrap();

        log.info("before any span");
        assert!(current().is_none());
        let (outer_id, inner_id);
        {
            let outer = start_span(&log, "outer", Some(&incoming));
            outer_id = outer.context().span_id.clone();
            log.info("in outer");
            {
                let inner = start_span(&log, "inner", None);
                inner_id = inner.context().span_id.clone();
                assert_eq!(inner.context().trace_id, incoming.trace_id);
                log.warn("in inner");
            }
            log.info("back in outer");
            assert_eq!(current().unwrap().span_id, outer_id, "restored after inner");
        }
        assert!(current().is_none(), "restored after outer");
        log.info("after spans");
        assert!(log.flush());

        let entries = spy.entries();
        let by_msg = |m: &str| entries.iter().find(|e| e.message == m).unwrap();
        assert!(by_msg("before any span").metadata.get("trace_id").is_none());
        assert!(by_msg("after spans").metadata.get("trace_id").is_none());
        assert_eq!(by_msg("in outer").metadata["trace_id"], incoming.trace_id);
        assert_eq!(by_msg("in outer").metadata["span_id"], outer_id);
        assert_eq!(by_msg("in inner").metadata["span_id"], inner_id);
        assert_eq!(by_msg("back in outer").metadata["span_id"], outer_id);

        let inner_span = entries
            .iter()
            .find(|e| e.log_type == LogType::Trace && e.message == "inner")
            .unwrap();
        assert_eq!(inner_span.metadata["span"]["parent_span_id"], outer_id);
        assert_eq!(inner_span.metadata["span"]["trace_id"], incoming.trace_id);
        let outer_span = entries
            .iter()
            .find(|e| e.log_type == LogType::Trace && e.message == "outer")
            .unwrap();
        assert_eq!(
            outer_span.metadata["span"]["parent_span_id"],
            incoming.span_id
        );
    }

    #[test]
    fn explicit_metadata_wins_over_the_ambient_context() {
        let (log, spy) = MicroLog::with_spy(Config {
            wal_enabled: false,
            batch_size: 1,
            ..Config::default()
        });
        let log = Arc::new(log);
        let _span = start_span(&log, "s", None);
        log.log_with(
            crate::LogLevel::Info,
            "custom ids",
            [("trace_id", serde_json::json!("mine"))],
        );
        assert!(log.flush());
        let e = spy
            .entries()
            .into_iter()
            .find(|e| e.message == "custom ids")
            .unwrap();
        assert_eq!(e.metadata["trace_id"], "mine");
    }
}
