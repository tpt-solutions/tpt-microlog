//! Turning whatever clients send into log entries.
//!
//! Accepted shapes (per line, per datagram, or as an HTTP body):
//!
//! * a JSON object: `{"level":"warn","message":"...","user":42}`. The message
//!   comes from `message`/`msg`/`log`/`text`, the level from
//!   `level`/`severity`/`lvl`/`levelname` (names, pino numbers 10-60, or syslog
//!   severities 0-7); everything else becomes metadata. A client timestamp is
//!   kept as `source_timestamp`.
//! * a metric: `{"type":"metric","name":"cpu","value":0.5,"tags":{...}}` or
//!   `{"metric":"cpu","value":0.5}`.
//! * a JSON array of objects, or `{"entries":[...]}` (tpt-microlog's own wire
//!   format, so agents can be chained).
//! * any other text line: an INFO entry with that text as the message.

use serde_json::{Map, Value};
use tpt_log_core::LogLevel;

/// One parsed input record.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Log {
        level: LogLevel,
        message: String,
        metadata: Map<String, Value>,
    },
    Metric {
        name: String,
        value: f64,
        tags: Option<Map<String, Value>>,
    },
}

const MESSAGE_KEYS: [&str; 4] = ["message", "msg", "log", "text"];
const LEVEL_KEYS: [&str; 4] = ["level", "severity", "lvl", "levelname"];
const TIME_KEYS: [&str; 4] = ["timestamp", "time", "ts", "@timestamp"];

/// Map a level name or number to a [`LogLevel`] (unknown values are INFO).
pub fn level_from_value(value: &Value) -> LogLevel {
    match value {
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "trace" | "verbose" => LogLevel::Trace,
            "debug" | "dbg" => LogLevel::Debug,
            "info" | "information" | "notice" => LogLevel::Info,
            "warn" | "warning" => LogLevel::Warn,
            "error" | "err" | "critical" | "crit" => LogLevel::Error,
            "fatal" | "panic" | "emerg" | "emergency" | "alert" => LogLevel::Fatal,
            _ => LogLevel::Info,
        },
        Value::Number(n) => match n.as_u64() {
            // pino-style: 10 trace .. 60 fatal
            Some(v) if v >= 10 => match v {
                0..=10 => LogLevel::Trace,
                11..=20 => LogLevel::Debug,
                21..=30 => LogLevel::Info,
                31..=40 => LogLevel::Warn,
                41..=50 => LogLevel::Error,
                _ => LogLevel::Fatal,
            },
            // syslog severities: 0 emerg .. 7 debug
            Some(0..=2) => LogLevel::Fatal,
            Some(3) => LogLevel::Error,
            Some(4) => LogLevel::Warn,
            Some(5) | Some(6) => LogLevel::Info,
            Some(7) => LogLevel::Debug,
            _ => LogLevel::Info,
        },
        _ => LogLevel::Info,
    }
}

fn from_object(mut obj: Map<String, Value>) -> Parsed {
    // Metric?
    let is_metric = obj.get("type").and_then(Value::as_str) == Some("metric")
        || (obj.get("metric").is_some_and(Value::is_string) && obj.get("value").is_some());
    if is_metric {
        let name = obj
            .get("metric")
            .or_else(|| obj.get("name"))
            .or_else(|| obj.get("message"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let value = obj.get("value").and_then(|v| match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        });
        if let (Some(name), Some(value)) = (name, value) {
            let tags = match obj.remove("tags") {
                Some(Value::Object(t)) => Some(t),
                _ => None,
            };
            return Parsed::Metric { name, value, tags };
        }
    }

    let mut message = String::new();
    for key in MESSAGE_KEYS {
        if let Some(v) = obj.remove(key) {
            message = match v {
                Value::String(s) => s,
                other => other.to_string(),
            };
            break;
        }
    }
    let mut level = LogLevel::Info;
    for key in LEVEL_KEYS {
        if let Some(v) = obj.remove(key) {
            level = level_from_value(&v);
            break;
        }
    }
    for key in TIME_KEYS {
        if let Some(v) = obj.remove(key) {
            obj.entry("source_timestamp").or_insert(v);
            break;
        }
    }
    Parsed::Log {
        level,
        message,
        metadata: obj,
    }
}

fn from_value(value: Value, out: &mut Vec<Parsed>) {
    match value {
        Value::Object(mut obj) => {
            if let Some(Value::Array(entries)) = obj.remove("entries") {
                for entry in entries {
                    from_value(entry, out);
                }
            } else {
                out.push(from_object(obj));
            }
        }
        Value::Array(items) => {
            for item in items {
                from_value(item, out);
            }
        }
        Value::String(s) => out.push(plain(&s)),
        other => out.push(plain(&other.to_string())),
    }
}

fn plain(text: &str) -> Parsed {
    Parsed::Log {
        level: LogLevel::Info,
        message: text.to_string(),
        metadata: Map::new(),
    }
}

/// Parse a single line; `None` for blank lines.
pub fn parse_line(line: &str) -> Option<Parsed> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if line.starts_with('{') {
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(line) {
            let mut out = Vec::new();
            from_value(Value::Object(obj), &mut out);
            // A single record per line; `entries` wrappers are expanded by
            // `parse_payload`.
            return out.into_iter().next();
        }
    }
    Some(plain(line))
}

/// Parse a whole payload (HTTP body or datagram): one JSON document, or lines.
pub fn parse_payload(body: &str) -> Vec<Parsed> {
    let trimmed = body.trim();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            let mut out = Vec::new();
            from_value(value, &mut out);
            return out;
        }
    }
    trimmed.lines().filter_map(parse_line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn log(p: Parsed) -> (LogLevel, String, Map<String, Value>) {
        match p {
            Parsed::Log {
                level,
                message,
                metadata,
            } => (level, message, metadata),
            other => panic!("expected a log, got {other:?}"),
        }
    }

    #[test]
    fn json_object_maps_message_level_and_metadata() {
        let (level, message, meta) = log(parse_line(
            r#"{"level":"warn","msg":"slow","user":42,"ts":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap());
        assert_eq!(level, LogLevel::Warn);
        assert_eq!(message, "slow");
        assert_eq!(meta["user"], 42);
        assert_eq!(meta["source_timestamp"], "2026-01-01T00:00:00Z");
        assert!(!meta.contains_key("msg") && !meta.contains_key("level"));
    }

    #[test]
    fn level_numbers_and_names() {
        let lvl = |v: Value| level_from_value(&v);
        assert_eq!(lvl(json!(30)), LogLevel::Info); // pino
        assert_eq!(lvl(json!(50)), LogLevel::Error);
        assert_eq!(lvl(json!(60)), LogLevel::Fatal);
        assert_eq!(lvl(json!(3)), LogLevel::Error); // syslog
        assert_eq!(lvl(json!(7)), LogLevel::Debug);
        assert_eq!(lvl(json!("WARNING")), LogLevel::Warn);
        assert_eq!(lvl(json!("emerg")), LogLevel::Fatal);
        assert_eq!(lvl(json!("???")), LogLevel::Info);
    }

    #[test]
    fn metrics_in_both_shapes() {
        assert_eq!(
            parse_line(r#"{"type":"metric","name":"cpu","value":0.5,"tags":{"host":"a"}}"#)
                .unwrap(),
            Parsed::Metric {
                name: "cpu".into(),
                value: 0.5,
                tags: Some(json!({"host":"a"}).as_object().unwrap().clone()),
            }
        );
        assert!(matches!(
            parse_line(r#"{"metric":"queue","value":"7"}"#).unwrap(),
            Parsed::Metric { value, .. } if value == 7.0
        ));
    }

    #[test]
    fn plain_text_and_blank_lines() {
        assert!(parse_line("   ").is_none());
        let (level, message, meta) = log(parse_line("just some text").unwrap());
        assert_eq!(
            (level, message.as_str()),
            (LogLevel::Info, "just some text")
        );
        assert!(meta.is_empty());
        // Malformed JSON is kept as text, not dropped.
        assert_eq!(log(parse_line("{not json").unwrap()).1, "{not json");
    }

    #[test]
    fn payload_shapes() {
        assert_eq!(parse_payload(r#"[{"msg":"a"},{"msg":"b"}]"#).len(), 2);
        assert_eq!(
            parse_payload(r#"{"entries":[{"message":"a"},{"message":"b"},{"message":"c"}]}"#).len(),
            3
        );
        assert_eq!(parse_payload("one\n\ntwo\n{\"msg\":\"three\"}\n").len(), 3);
        assert_eq!(parse_payload(r#"{"msg":"single"}"#).len(), 1);
        assert!(parse_payload("").is_empty());
    }
}
