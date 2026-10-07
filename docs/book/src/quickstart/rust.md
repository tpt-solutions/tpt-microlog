# Rust

The crates are not on crates.io yet; depend on the repository:

```toml
[dependencies]
tpt-log-core = { git = "https://github.com/tpt-solutions/tpt-microlog", features = ["tracing", "log"] }
tpt-log-transport = { git = "https://github.com/tpt-solutions/tpt-microlog" }
```

`tpt-log-core` is the engine; `tpt-log-transport` adds the backends and the
`Builder`. Rust 1.89 or newer is required.

## A first logger

```rust
use tpt_log_core::mlog;
use tpt_log_transport::Builder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Starts from tpt-log.toml and TPT_LOG_* variables; setters override them.
    let log = Builder::new()
        .backend("file")
        .configure(|c| c.file_path = Some("app.jsonl".into()))
        .service("checkout")
        .redact_keys(["password", "token"])
        .build_arc()?;

    log.info("service started");
    mlog!(log, Info, "user logged in", user_id = 123, ip = "1.2.3.4");
    log.metric("queue_depth", 7.0, None);

    log.shutdown(); // flush and drain; bounded by shutdown_timeout_ms
    Ok(())
}
```

Always call `shutdown()` (or let the last `MicroLog` drop, which does the same)
before the process exits; that is what gives the delivery guarantee.

## Integrations

```rust
use tpt_log_core::install_panic_hook;
use tpt_log_core::integrations::{log_facade, tracing_layer::MicroLogLayer};
use tracing_subscriber::prelude::*;

install_panic_hook(log.clone());                       // panics become FATAL entries
log_facade::init(log.clone(), ::log::LevelFilter::Info)?; // `log` crate macros
tracing_subscriber::registry()
    .with(MicroLogLayer::new(log.clone()).with_spans(true)) // `tracing` events and spans
    .init();
```

Entries logged inside a span carry `trace_id` and `span_id`
([details](../guides/observability.md)). Records emitted by the HTTP/TLS stack
that delivers your logs are never forwarded, so the logger cannot feed on its
own network chatter.

## Typed events

```rust
use serde::Serialize;
use tpt_log_core::events::Event;

#[derive(Serialize)]
struct OrderPlaced { order_id: u64, total_cents: u64 }

impl Event for OrderPlaced {
    const NAME: &'static str = "order_placed";
    const VERSION: u32 = 2; // bump when a field changes meaning
}

log.event(&OrderPlaced { order_id: 42, total_cents: 1999 });
```

The entry carries the fields plus `event` and `event_version`, so a backend can
index them and tell schema generations apart.
