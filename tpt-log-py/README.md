# tpt-log-py

Python binding for [tpt-microlog](https://github.com/tpt-solutions/tpt-microlog) — the microscopic telemetry & log router.

This crate is the [PyO3](https://pyo3.rs) extension module published to PyPI as
`tpt-microlog`. It wraps the native [`tpt-log-core`](../tpt-log-core) engine and
[`tpt-log-transport`](../tpt-log-transport) backends, exposing an idiomatic
Python API on top of them.

- **API docs:** <https://docs.rs/tpt-log-py>
- **Repository:** <https://github.com/tpt-solutions/tpt-microlog/tree/master/tpt-log-py>
- **Published package:** <https://pypi.org/project/tpt-microlog/>

## Install

```sh
pip install tpt-microlog
```

Wheels are built with [maturin](https://www.maturin.rs) using the stable ABI
(`abi3-py39`), so a single wheel covers Python 3.9 through 3.13. A source build
requires a Rust toolchain.

## Usage

```python
from tpt_microlog import MicroLog

log = MicroLog(backend="datadog", api_key="your-api-key", service="checkout-api")

log.info("User logged in", user_id=123, ip="1.2.3.4")
log.error("Database connection failed", error="timeout", retry_count=3)
log.metric("cpu_usage", 42.5, tags={"host": "server-01"})
log.span("checkout", trace_id="...", span_id="...", duration_us=1520)

log.shutdown()  # flush + drain before exit
```

Metadata is passed as keyword arguments, so entries stay structured without
building dictionaries at every call site.

To load an entire configuration from a `tpt-log.toml` file:

```python
from tpt_microlog import load_config

log = load_config("tpt-log.toml")
log.info("started", pid=1234)
log.shutdown()
```

## `logging` integration

```python
import logging
from tpt_microlog.handler import MicroLogHandler

logging.getLogger().addHandler(MicroLogHandler(backend="datadog", api_key="..."))
logging.getLogger("checkout").warning("slow query", extra={"ms": 840})
```

Pass `log=` to share an existing `MicroLog` instead of creating one (a shared
logger is flushed, not shut down, when the handler closes). Each record carries
`logger`, `module`, `function`, `line` and `thread` metadata, every `extra=`
field, and the traceback under `exception`. `CRITICAL` maps to FATAL and levels
below DEBUG map to TRACE.

## API

| Member | Description |
| --- | --- |
| `MicroLog(**kwargs)` | Creates a logger from keyword options |
| `trace` / `debug` / `info` / `warn` / `error` / `fatal` | Level logging with `**metadata` |
| `metric(name, value, tags=None)` | Numeric metric with optional tags |
| `span(name, trace_id, span_id, parent_span_id=None, duration_us=0)` | Distributed-trace span |
| `queued()` | Entries still buffered in the ingestion queue |
| `backend` | Property returning the configured backend name |
| `shutdown()` | Flush and drain (idempotent) |
| `load_config(path)` | Builds a `MicroLog` from a `tpt-log.toml` file |
| `tpt_microlog.__version__` | Installed package version |

### Constructor options

All options are optional and layered on top of `tpt-log.toml` and `TPT_LOG_*`
environment variables.

| Option | Default | Description |
| --- | --- | --- |
| `backend` | `none` | `none`, `custom`/`http`, `datadog`, `elasticsearch`, `cloudwatch`, `prometheus`, `grpc`, `udp`, `tcp`, `file` |
| `endpoint` | backend default | Base URL for HTTP-style backends |
| `api_key` | none | Secret token |
| `service` | none | Service name attached to every entry |
| `batch_size` | `100` | Entries per batch |
| `flush_interval` | `1.0` | Seconds between flushes |
| `memory_limit_mb` | `10` | Flush once buffered bytes cross this |
| `compression_level` | `3` | zstd level (1–21) |
| `fast_mode` | `False` | Disable compression for extreme throughput |
| `max_retries` | `10` | Delivery attempts with exponential backoff |
| `wal_enabled` / `wal_dir` / `wal_max_size_mb` | platform default | Disk persistence on failure |
| `min_level` | `TRACE` | Drop entries below this level |
| `sample_rate` | `1.0` | Fraction of entries delivered (0.0–1.0) |
| `enrichment` | `False` | Attach hostname / PID / service |
| `extra_headers` | none | List of `"Name=Value"` HTTP headers |

Type stubs are shipped in `tpt_microlog/_native.pyi` and the package is marked
`py.typed`, so type checkers see the full API.

## Building from source

```sh
# from the repository root
cargo build --release -p tpt-log-py
pip install maturin
maturin develop --manifest-path tpt-log-py/Cargo.toml
```

The native module is registered as `tpt_microlog._native` and re-exported from
the pure-Python `tpt_microlog` package.

Run the unit tests with:

```sh
cargo test -p tpt-log-py
```

The `pyo3` feature is enabled by default and is required to build the extension
module; it can be disabled to build the crate as a plain Rust library.

## License

Dual-licensed under [MIT](https://github.com/tpt-solutions/tpt-microlog/blob/master/LICENSE-MIT) or [Apache-2.0](https://github.com/tpt-solutions/tpt-microlog/blob/master/LICENSE-APACHE),
at your option.
