//! Typed, schema-versioned events.
//!
//! Instead of free-form strings, define events as types: the compiler checks
//! field names and types, the name and schema version are constants, and the
//! backend receives a stable `event` / `event_version` pair it can index.
//!
//! ```
//! use serde::Serialize;
//! use tpt_log_core::events::Event;
//! use tpt_log_core::{Config, LogLevel, MicroLog};
//!
//! #[derive(Serialize)]
//! struct OrderPlaced {
//!     order_id: u64,
//!     total_cents: u64,
//! }
//!
//! impl Event for OrderPlaced {
//!     const NAME: &'static str = "order_placed";
//!     const VERSION: u32 = 2;
//! }
//!
//! let log = MicroLog::with_default_transport(Config::default());
//! log.event(&OrderPlaced { order_id: 42, total_cents: 1999 });
//! ```

use serde::Serialize;
use serde_json::{Map, Value};

use crate::{LogLevel, MicroLog};

/// A typed log event. Implement it on any `Serialize` type.
pub trait Event: Serialize {
    /// Stable event name: the entry message and the `event` metadata field.
    const NAME: &'static str;
    /// Schema version, recorded as `event_version`. Bump it when a field's
    /// meaning or type changes so consumers can tell generations apart.
    const VERSION: u32 = 1;
    /// Severity of the entry.
    const LEVEL: LogLevel = LogLevel::Info;
}

/// Flatten an event into entry metadata: its fields, plus `event` and
/// `event_version`. A struct becomes a flat object; any other shape (tuple,
/// scalar, sequence) is kept under a `value` key rather than lost.
pub fn event_metadata<E: Event>(event: &E) -> Map<String, Value> {
    let mut map = match serde_json::to_value(event) {
        Ok(Value::Object(fields)) => fields,
        Ok(other) => {
            let mut wrapped = Map::new();
            wrapped.insert("value".into(), other);
            wrapped
        }
        Err(e) => {
            // A broken Serialize impl must not drop the event itself.
            let mut failed = Map::new();
            failed.insert("serialize_error".into(), Value::String(e.to_string()));
            failed
        }
    };
    map.insert("event".into(), Value::String(E::NAME.into()));
    map.insert("event_version".into(), Value::from(E::VERSION));
    map
}

impl MicroLog {
    /// Log a typed [`Event`].
    pub fn event<E: Event>(&self, event: &E) {
        self.log_with(E::LEVEL, E::NAME, event_metadata(event));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[derive(Serialize)]
    struct OrderPlaced {
        order_id: u64,
        items: Vec<&'static str>,
    }
    impl Event for OrderPlaced {
        const NAME: &'static str = "order_placed";
        const VERSION: u32 = 3;
    }

    #[derive(Serialize)]
    struct PaymentFailed {
        reason: String,
    }
    impl Event for PaymentFailed {
        const NAME: &'static str = "payment_failed";
        const LEVEL: LogLevel = LogLevel::Error;
    }

    #[derive(Serialize)]
    struct Heartbeat(u32);
    impl Event for Heartbeat {
        const NAME: &'static str = "heartbeat";
    }

    struct Unserializable;
    impl Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("nope"))
        }
    }
    impl Event for Unserializable {
        const NAME: &'static str = "broken";
    }

    #[test]
    fn events_become_structured_entries() {
        let (log, spy) = MicroLog::with_spy(Config {
            wal_enabled: false,
            batch_size: 1,
            flush_interval_ms: 20,
            ..Config::default()
        });
        log.event(&OrderPlaced {
            order_id: 42,
            items: vec!["a", "b"],
        });
        log.event(&PaymentFailed {
            reason: "declined".into(),
        });
        log.event(&Heartbeat(7));
        log.event(&Unserializable);
        assert!(log.flush());

        let entries = spy.entries();
        let get = |name: &str| entries.iter().find(|e| e.message == name).unwrap();
        let order = get("order_placed");
        assert_eq!(order.level, LogLevel::Info);
        assert_eq!(order.metadata["order_id"], 42);
        assert_eq!(order.metadata["items"][1], "b");
        assert_eq!(order.metadata["event"], "order_placed");
        assert_eq!(order.metadata["event_version"], 3);
        assert_eq!(get("payment_failed").level, LogLevel::Error);
        assert_eq!(get("payment_failed").metadata["event_version"], 1);
        assert_eq!(
            get("heartbeat").metadata["value"],
            7,
            "non-struct kept under `value`"
        );
        assert_eq!(
            get("broken").metadata["serialize_error"],
            "nope",
            "event is never dropped"
        );
    }
}
