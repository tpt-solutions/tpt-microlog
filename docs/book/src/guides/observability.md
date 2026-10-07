# Observability of the logger itself

A logger that fails silently is worse than none. tpt-microlog gives you four
ways to see what it is doing.

## 1. `on_error`: problems as they happen

Internal failures (a batch given up on, WAL trouble, a worker panic, a config
warning) go to a callback instead of vanishing:

```python
tpt_microlog.set_on_error(lambda kind, message: alerting.send(kind, message))
```

`kind` is `delivery`, `wal`, `worker`, `config` or `health`. Without a callback
they are written to stderr. Identical messages are rate limited to one per 30
seconds so a dead backend does not flood you. The callback runs on background
threads and must be quick; exceptions it raises are swallowed.

## 2. `stats()`: counters

| Counter | Meaning |
| --- | --- |
| `delivered_entries` / `delivered_batches` | accepted by the backend |
| `wal_writes` | batches parked in the WAL after a failed delivery |
| `retried_entries` | entries delivered by a WAL replay |
| `dropped_entries` / `dropped_batches` | lost: queue overflow policy, or no WAL available |
| `sampled_entries`, `rate_limited_entries`, `deduplicated_entries`, `recorded_entries` | thinned on purpose by a [volume control](volume.md) |
| `delivery_failures` | batches given up on after retries |
| `last_latency_ms` | duration of the last successful send |

## 3. `health()` and `/healthz`

```python
log.health()
# {"status": "degraded", "queue_depth": 12, "queue_capacity": 10000, "wal_files": 3,
#  "wal_bytes": 48211, "last_success_ms_ago": 90500, "last_failure_ms_ago": 400, ...}
```

`status` is `ok`, `degraded` (WAL backlog, queue over 80% full, or the last send
failed), `down` (failing right now with undelivered data piling up), or
`stopped` (after `shutdown()`). Set `health_bind = "127.0.0.1:9477"` to serve it
over HTTP: `GET /healthz` returns 200 for `ok`/`degraded` and 503 for
`down`/`stopped` (a degraded logger is still serving traffic and catching up),
and `GET /stats` adds the counters. It binds only where you tell it to; there is
no authentication, so keep it on loopback or a private network.

## 4. Self-metrics

```toml
self_metrics_interval_ms = 30000
```

emits `microlog.queue_depth`, `microlog.inflight_batches`, `microlog.wal_bytes`,
`microlog.wal_files`, `microlog.delivered_entries`, `microlog.dropped_entries`,
`microlog.delivery_failures` and `microlog.delivery_latency_ms` through the
normal pipeline (tagged with the status), bypassing sampling and filters, so
they show up in the same dashboards as your own metrics.

## Correlating entries with traces

Parse an incoming W3C `traceparent` header and open a span; every entry logged
inside it carries `trace_id` and `span_id`, and the closed span is recorded with
its duration. Send `ctx.traceparent` to downstream services to continue the
trace.

```python
with log.trace_span("handle_request", traceparent=headers.get("traceparent")) as ctx:
    log.info("charging card")        # trace_id / span_id attached automatically
```

In Rust use `tpt_log_core::trace::start_span`, or the `tracing` layer, which
attaches the ids of the span an event is actually in (the right choice for
async code). Python's `trace_span` follows asyncio tasks as well as threads.

## Typed events

Define events as types (Rust `Event` trait, Python dataclasses, TypeScript
`EventMap`) and each entry carries `event` and `event_version`, so a backend can
index them and consumers can tell schema generations apart.
