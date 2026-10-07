# Python

The package is not on PyPI yet. Build it from a checkout (it needs a Rust
toolchain):

```sh
pip install maturin
cd tpt-log-py && maturin develop --release
```

## A first logger

```python
from tpt_microlog import MicroLog

log = MicroLog(backend="file", file_path="app.jsonl", service="checkout")
log.info("User logged in", user_id=123, ip="1.2.3.4")
log.metric("cpu_usage", 42.5, tags={"host": "server-01"})
log.shutdown()   # flush and drain (also runs at interpreter exit)
```

Every option from [`tpt-log.toml`](../guides/toml-gallery.md) is a keyword
argument; anything you omit comes from the config file and `TPT_LOG_*`
variables. `with MicroLog(...) as log:` shuts down on exit, and a forked child
process detaches automatically so it never waits on worker threads that do not
exist.

## The standard `logging` module

```python
import logging
from tpt_microlog.handler import MicroLogHandler

logging.getLogger().addHandler(MicroLogHandler(backend="datadog", api_key="..."))
logging.getLogger("checkout").warning("slow query", extra={"ms": 840})
```

`extra=` fields become metadata; tracebacks go under `exception`. Pass `log=` to
share an existing logger. See the
[`migrate_from_logging.py` recipe](https://github.com/tpt-solutions/tpt-microlog/blob/master/examples/recipes/migrate_from_logging.py).

## Trace correlation and typed events

```python
with log.trace_span("handle_request", traceparent=request.headers.get("traceparent")) as ctx:
    log.info("working")            # carries trace_id / span_id, also across asyncio tasks
    downstream_headers = {"traceparent": ctx.traceparent}

@dataclass
class PaymentFailed:
    __event_level__ = "error"
    reason: str

log.event(PaymentFailed("declined"))   # message "PaymentFailed", plus event / event_version
```

## Operating it

```python
import tpt_microlog
tpt_microlog.set_on_error(lambda kind, msg: print("logger problem:", kind, msg))

log.stats()    # delivered_entries, dropped_entries, wal_writes, sampled_entries, ...
log.health()   # {"status": "ok" | "degraded" | "down" | "stopped", queue_depth, wal_bytes, ...}
```

The [recipes](https://github.com/tpt-solutions/tpt-microlog/tree/master/examples/recipes)
show request-id correlation, redaction, sampling and an offline-first WAL demo.
