# Benchmark methodology

Numbers without a method are marketing. This page says what the repository's
benchmarks measure, how to reproduce them, and how to read them.

## What is measured

Two programs in `tpt-log-core/examples/`:

* **`throughput`** (`cargo run --release -p tpt-log-core --example throughput -- [entries]`)
  logs `N` entries (default one million) of the form
  `benchmark message number {i} with some payload text` against the `none`
  backend, with the WAL off and a 200,000-entry queue. It reports two rates:
  *ingest* (how fast `info()` returns: the hot path your application pays for)
  and *total + drain* (until `shutdown()` has batched, compressed and handed
  every entry to the discarding transport).
* **`ci_bench`** is what CI uses: it logs 200,000 structured entries with
  metadata five times and prints the median. CI runs it on the base and the head
  commit of a pull request on the same machine, interleaved, and fails the
  change if throughput drops by more than 20%.

## What is deliberately not measured

* **Network and backend speed.** The transport is a no-op. End-to-end rates
  depend on your backend, its rate limits and your link; measure them with your
  real configuration (`tpt-microlog send-test`, or `stats()` and `health()`
  latency in production).
* **Disk.** The WAL is off. With `durability = "wal-first"` expect one disk
  write per batch; measure on the disk you will use.
* **Multi-process behaviour**, memory ceilings under a slow backend, and CPU
  share. These are design targets covered by tests, not tracked by a dashboard.

## How to read a result

* Run in `--release`; debug builds are an order of magnitude slower.
* Ignore the first run (cold caches); compare medians of several.
* Compare only runs from the same machine and load. Shared CI runners are noisy,
  which is why the CI gate compares base and head *interleaved on one machine*
  and uses a generous 20% threshold: it catches real regressions, not 3% noise.
* A change in *ingest* rate means the producer-side path changed (filters,
  governors, redaction, the queue). A change in *total + drain* with a flat ingest
  rate means batching, compression or delivery changed.

## Reproducing

```sh
cargo run --release -p tpt-log-core --example throughput -- 1000000
cargo run --release -p tpt-log-core --example ci_bench
```

To measure your own scenario, build a `MicroLog` with the configuration you care
about (a custom transport that records timings is a few lines), log from as many
threads as your application has, and report entries per second together with the
configuration, hardware and `rustc --version`.
