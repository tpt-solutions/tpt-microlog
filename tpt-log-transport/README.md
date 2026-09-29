# tpt-log-transport

Delivery backends for [tpt-microlog](https://github.com/tpt-solutions/tpt-microlog) — the microscopic telemetry & log router.

`tpt-log-transport` implements the `tpt_log_core::Transport` trait for every
supported backend and provides `build_transport`, a config-to-transport factory
that picks the right implementation from a [`tpt_log_core::Config`].

The `Transport` contract is deliberately **blocking and runtime-free**: delivery
runs on the core's background worker thread, and retries, WAL persistence, and
graceful shutdown stay in [`tpt-log-core`](../tpt-log-core). This crate only
formats the wire protocol and performs I/O.

- **API docs:** <https://docs.rs/tpt-log-transport>
- **Repository:** <https://github.com/tpt-solutions/tpt-microlog/tree/master/tpt-log-transport>

## Install

```toml
[dependencies]
tpt-log-transport = "0.1"
```

## Usage

```rust
use std::sync::Arc;
use tpt_log_core::{Config, MicroLog};
use tpt_log_transport::build_transport;

let config = Config::from_toml_str(r#"
    backend = "datadog"
    api_key = "your-api-key"
    service = "checkout-api"
"#)?;

let transport = build_transport(&config)?;
let log = MicroLog::new(config, Arc::from(transport));

log.info("user logged in");
log.metric("cpu_usage", 42.5, None);
log.shutdown();
```

`build_transport` returns `Result<Box<dyn Transport>, TransportError>` and
rejects unknown or incomplete backend selections (for example, `datadog`
without an `api_key`, or a `grpc` backend without a `grpc_endpoint`).

## Supported backends

| `backend` | Type | Endpoint source | Notes |
| --- | --- | --- | --- |
| `none` | `NullTransport` | — | Discards batches; benchmarking and tests |
| `custom` | `HttpTransport` | `endpoint` | Generic JSON `POST` |
| `datadog` | `HttpTransport` + `adapters::datadog` | default `https://http-intake.logs.datadoghq.com/api/v2/logs` | Logs API v2, `DD-API-KEY` header |
| `elasticsearch` | `HttpTransport` + `adapters::elasticsearch` | default `http://localhost:9200` | Bulk API, NDJSON |
| `cloudwatch` | `HttpTransport` + `adapters::cloudwatch` | AWS regional endpoint | `PutLogEvents` signed with SigV4 |
| `prometheus` | `HttpTransport` + `adapters::prometheus` | `prometheus_bind` | Embedded `/metrics` pull endpoint |
| `grpc` | `GrpcTransport` | `grpc_endpoint` (`host:port`) | OTLP logs over gRPC |
| `udp` | `UdpTransport` | `net_endpoint` | Fire-and-forget datagrams |
| `tcp` | `TcpTransport` | `net_endpoint` | Ordered newline-delimited stream |
| `file` | `FileTransport` | `file_path` | JSONL with size/time rotation |

## Writing a custom transport

Implement `Transport` directly instead of using the factory:

```rust
use tpt_log_core::{CompressedBatch, Transport, TransportError};

struct MyTransport;

impl Transport for MyTransport {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        // batch.payload holds the zstd-compressed JSON, batch.entries the raw JSON.
        Ok(())
    }
    fn name(&self) -> &str {
        "my-transport"
    }
}
```

Return `TransportError::Transient` for retryable failures (timeouts, 5xx, 429)
and `TransportError::Permanent` for failures that will never succeed (4xx other
than 429) — the core decides whether to retry and whether to fall back to the
WAL.

## HTTP behaviour

`HttpTransport` wraps a pooled blocking `reqwest` client with `rustls-tls`, so no
OpenSSL system dependency is required. `BodyMode` selects the request body
encoding (plain JSON, zstd-compressed, or the backend's bulk NDJSON form).

Failure classification is centralized in `http::request_once` and drives the
core's `RetryPolicy`.

## Adapters

Each adapter in `src/adapters/` converts a decoded batch into its backend's wire
format:

- `datadog` — Logs API v2 envelope with attributes and tags.
- `elasticsearch` — Bulk API action/ document pairs.
- `cloudwatch` — `LogStreamEvent` batches paired with `sequenceToken` handling.
- `sigv4` — AWS Signature Version 4 request signing (HMAC-SHA256).
- `prometheus` — text exposition of metric entries.

## Development

```sh
cargo build -p tpt-log-transport
cargo test -p tpt-log-transport
```

`build.rs` compiles the vendored OpenTelemetry proto definitions from
`proto/opentelemetry/` into `src/gen.rs` at build time via `tonic-build`, so no
network access is required at build time.

## License

Dual-licensed under [MIT](https://github.com/tpt-solutions/tpt-microlog/blob/master/LICENSE-MIT) or [Apache-2.0](https://github.com/tpt-solutions/tpt-microlog/blob/master/LICENSE-APACHE),
at your option.
