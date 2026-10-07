//! `tracing` integration (feature `tracing`).
//!
//! ```no_run
//! use std::sync::Arc;
//! use tracing_subscriber::prelude::*;
//! use tpt_log_core::{Config, MicroLog};
//! use tpt_log_core::integrations::tracing_layer::MicroLogLayer;
//!
//! let log = Arc::new(MicroLog::with_default_transport(Config::default()));
//! tracing_subscriber::registry()
//!     .with(MicroLogLayer::new(log).with_spans(true))
//!     .init();
//! tracing::info!(user_id = 123, "user logged in");
//! ```
//!
//! Events become entries: the `message` field is the entry message, every
//! other field is metadata, and `target`, `module`, `file`, `line` plus the
//! enclosing spans (`spans: [{name, ...fields}]`) are added. With
//! [`MicroLogLayer::with_spans`], each closed span is also recorded as a
//! trace span (`trace_id` is the root span, `duration_us` the busy+idle
//! lifetime).

use std::fmt::Debug;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use super::is_internal;
use crate::{LogLevel, MicroLog};

/// A `tracing_subscriber::Layer` that forwards events (and optionally spans)
/// to a [`MicroLog`].
pub struct MicroLogLayer {
    log: Arc<MicroLog>,
    spans: bool,
}

impl MicroLogLayer {
    /// Forward events to `log`.
    pub fn new(log: Arc<MicroLog>) -> Self {
        Self { log, spans: false }
    }

    /// Also record every closed span as a trace span entry.
    pub fn with_spans(mut self, enabled: bool) -> Self {
        self.spans = enabled;
        self
    }
}

/// Collects event/span fields into a JSON map; `message` is kept apart.
#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    fields: Map<String, Value>,
}

impl FieldVisitor {
    fn put(&mut self, field: &Field, value: Value) {
        if field.name() == "message" {
            self.message = Some(match value {
                Value::String(s) => s,
                other => other.to_string(),
            });
        } else {
            self.fields.insert(field.name().to_string(), value);
        }
    }
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, Value::String(value.to_string()));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, value.into());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, value.into());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        // NaN/inf have no JSON form.
        self.put(
            field,
            serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number),
        );
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, Value::String(value.to_string()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.put(field, Value::String(format!("{value:?}")));
    }
}

/// Per-span data stored in the span's extensions.
struct SpanData {
    fields: Map<String, Value>,
    start: Instant,
}

fn map_level(level: &Level) -> LogLevel {
    match *level {
        Level::ERROR => LogLevel::Error,
        Level::WARN => LogLevel::Warn,
        Level::INFO => LogLevel::Info,
        Level::DEBUG => LogLevel::Debug,
        Level::TRACE => LogLevel::Trace,
    }
}

impl<S> Layer<S> for MicroLogLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);
        span.extensions_mut().insert(SpanData {
            fields: visitor.fields,
            start: Instant::now(),
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = FieldVisitor::default();
        values.record(&mut visitor);
        let mut ext = span.extensions_mut();
        if let Some(data) = ext.get_mut::<SpanData>() {
            data.fields.extend(visitor.fields);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        if is_internal(meta.target()) {
            return;
        }
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        let mut md = visitor.fields;
        md.insert("target".into(), meta.target().into());
        if let Some(m) = meta.module_path() {
            md.insert("module".into(), m.into());
        }
        if let Some(f) = meta.file() {
            md.insert("file".into(), f.into());
        }
        if let Some(l) = meta.line() {
            md.insert("line".into(), l.into());
        }
        if let Some(scope) = ctx.event_scope(event) {
            let spans: Vec<Value> = scope
                .from_root()
                .map(|span| {
                    let mut obj = Map::new();
                    obj.insert("name".into(), span.name().into());
                    if let Some(data) = span.extensions().get::<SpanData>() {
                        for (k, v) in &data.fields {
                            obj.insert(k.clone(), v.clone());
                        }
                    }
                    Value::Object(obj)
                })
                .collect();
            if !spans.is_empty() {
                md.insert("spans".into(), Value::Array(spans));
            }
        }
        self.log.log_with(
            map_level(meta.level()),
            visitor.message.unwrap_or_default(),
            md,
        );
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        if !self.spans {
            return;
        }
        let Some(span) = ctx.span(&id) else { return };
        if is_internal(span.metadata().target()) {
            return;
        }
        let Some(duration_us) = span
            .extensions()
            .get::<SpanData>()
            .map(|d| d.start.elapsed().as_micros() as u64)
        else {
            return;
        };
        let trace_id = span
            .scope()
            .last()
            .map_or_else(|| id.into_u64(), |root| root.id().into_u64());
        let parent = span.parent().map(|p| format!("{:016x}", p.id().into_u64()));
        self.log.span(
            span.name(),
            format!("{trace_id:016x}"),
            format!("{:016x}", id.into_u64()),
            parent,
            duration_us,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, LogType};
    use tracing_subscriber::prelude::*;

    #[test]
    fn events_spans_and_fields_are_forwarded() {
        let cfg = Config {
            wal_enabled: false,
            batch_size: 1,
            flush_interval_ms: 20,
            ..Config::default()
        };
        let (log, spy) = MicroLog::with_spy(cfg);
        let log = Arc::new(log);
        let subscriber =
            tracing_subscriber::registry().with(MicroLogLayer::new(log.clone()).with_spans(true));

        tracing::subscriber::with_default(subscriber, || {
            let outer = tracing::info_span!(target: "app", "request", req_id = "r1");
            let _o = outer.enter();
            let inner = tracing::debug_span!(target: "app", "db", table = "users");
            {
                let _i = inner.enter();
                tracing::warn!(target: "app", rows = 3u64, ratio = 0.5, ok = true, "slow query");
                tracing::error!(target: "app", error = %"conn reset", "failed");
            }
            drop(inner);
            tracing::info!(target: "hyper::client", "internal chatter");
        });
        log.flush();

        let entries = spy.entries();
        let slow = entries
            .iter()
            .find(|e| e.message == "slow query")
            .expect("event");
        assert_eq!(slow.level, LogLevel::Warn);
        assert_eq!(slow.metadata["rows"], 3);
        assert_eq!(slow.metadata["ratio"], 0.5);
        assert_eq!(slow.metadata["ok"], true);
        let spans = slow.metadata["spans"].as_array().unwrap();
        assert_eq!(spans[0]["name"], "request");
        assert_eq!(spans[0]["req_id"], "r1");
        assert_eq!(spans[1]["name"], "db");
        assert_eq!(spans[1]["table"], "users");
        assert_eq!(
            entries
                .iter()
                .find(|e| e.message == "failed")
                .unwrap()
                .level,
            LogLevel::Error
        );
        assert!(!entries.iter().any(|e| e.message == "internal chatter"));

        let db_span = entries
            .iter()
            .find(|e| e.log_type == LogType::Trace && e.message == "db")
            .expect("closed span recorded");
        let s = &db_span.metadata["span"];
        assert!(s["parent_span_id"].is_string());
        assert_eq!(s["trace_id"].as_str().unwrap().len(), 16);
    }
}
