# Delivery guarantees explained

tpt-microlog aims for **at-least-once delivery of every entry it accepted**, and
is explicit about the places where that stops being true. This page follows one
entry from your call to the backend.

## The life of an entry

```text
log.info("...")
  1. filters           min_level, filter_regex, filter_tags            -> dropped (counted nowhere: you asked for it)
  2. governors         flight recorder, sampling, rate limit, dedup    -> dropped (counted: sampled/rate_limited/deduplicated)
  3. enrich + redact   host/pid/service, redact_keys/patterns          (so secrets never reach 4-8)
  4. queue             bounded; queue_full_policy decides when full
  5. batcher           flush on batch_size, memory_limit_bytes, flush_interval_ms, flush(), shutdown()
  6. compress          zstd (fast_mode turns it off)
  7. deliver           transport.send with retries and backoff
  8a. success          counted in delivered_entries
  8b. failure          written to the WAL (wal_writes); if there is no WAL: dropped_batches
  9. replay            a background pass re-sends WAL batches oldest-first
 10. dead letter       a batch that keeps failing moves to wal/dead/ after N replays
```

Steps 1-2 are *intentional* loss: they are the volume controls you configured.
Everything from step 4 on is the delivery guarantee.

## Retries

A failed send is retried up to `max_retries` times with exponential backoff
(`retry_base_ms` doubling up to `retry_max_backoff_ms`, jittered). A server's
`Retry-After` is honored, capped by `retry_after_max_ms`. Errors that cannot
succeed (a configuration error, a 4xx other than 408/425/429) are **not**
retried in-process: the batch is parked in the WAL straight away so it cannot
block the batches behind it, and if replay keeps failing it is dead-lettered
(below).

## The write-ahead log

When retries are exhausted the batch is written to the WAL directory (see
`wal_dir`), namespaced by service + backend + endpoint so two programs never
replay each other's data, and guarded by an OS file lock. Every
`wal_retry_interval_ms` a replay pass re-sends batches oldest-first and **stops
at the first failure**, so a dead backend cannot make the logger spin through
the whole directory. Each failed pass increments an attempt counter in the file
name; after `wal_dead_letter_replays` failures the file moves to `wal/dead/` and
is reported through `on_error`. Dead-lettered batches are kept for inspection
(`tpt-microlog wal query --dead`), never silently deleted.

The WAL is bounded by `wal_max_size_bytes`: beyond it the **oldest** batches are
discarded (and reported). Size it for the outage you want to survive.

## Durability modes

| `durability` | What survives |
| --- | --- |
| `best-effort` (default) | Everything on a graceful shutdown. A hard kill (SIGKILL, power loss) can lose entries still in memory. |
| `wal-first` | Each batch is written to the WAL *before* the first send and removed after success, so an abrupt kill loses at most the entries not yet batched. Costs one disk write per batch. |

## Shutdown

`shutdown()` (also run when the last logger is dropped, and at interpreter exit
in Python) stops the batcher, flushes the last batch, drains the delivery
queue and makes one more replay attempt. It is bounded by
`shutdown_timeout_ms`: if the backend is slow, what is left is **spilled to the
WAL** and the call returns, so the host program is never held hostage by its
logger. `flush()` waits (with the same bound) until everything is delivered or
persisted, without stopping the logger.

After `shutdown()` the WAL directory is released, so a logger created
afterwards in the same process can open it.

## What "delivered" means per backend

`delivered_entries` counts entries the transport reported as sent without error:

* HTTP-style backends (Datadog, Elasticsearch, Loki, Splunk, OTLP, generic
  HTTP, CloudWatch): the server answered with success. Elasticsearch's bulk
  response is inspected per item.
* TCP: written to the stream (set `tcp_ack = true` for a per-frame
  acknowledgement). UDP: handed to the network stack; **UDP is fire-and-forget**.
* File: written to disk. Console: printed. Prometheus: this is a pull backend,
  so values are exposed for scraping instead of delivered.

## Caveats, stated plainly

* **Duplicates are possible.** A batch the backend accepted whose
  acknowledgement was lost is sent again. Prefer backends or queries that
  tolerate repeats.
* **Order is not guaranteed** across retries and WAL replay; entries carry
  their own timestamps.
* **Intentional drops** (filters, sampling, rate limit, dedup, the `drop_*`
  queue policies) are not delivery failures; they are counted separately in
  `stats()`.
* **`queue_full_policy = "block"`** never drops but can stall the calling
  thread if the backend is down *and* the queue fills; choose `drop_newest` or
  `drop_oldest` when the application must never wait on logging.
* A **forked child** process (Python) detaches rather than share the parent's
  worker threads.
* The **WebAssembly** build keeps retries in memory only; use the
  [offline queue](../quickstart/javascript.md) for persistence in browsers.

When data goes missing, [Troubleshooting](troubleshooting.md) maps each counter
to the stage where it was lost.
