# tpt-microlog

**The microscopic telemetry and log router. Reliable delivery, zero infrastructure.**

tpt-microlog is an embeddable logging and metrics engine written in Rust, with
bindings for Python, JavaScript/WebAssembly, Go, C/C++ and Rust. It lives inside
your process: it takes entries on a lock-free queue, batches and compresses
them, and keeps retrying until the backend accepts them, parking what it cannot
deliver in an on-disk write-ahead log (WAL). There is no collector to run.

```text
your code -> filters / sampling / redaction -> queue -> batch + zstd
          -> send with retries -> (failure) WAL -> replay -> backend
```

## What it is good at

* **Not losing logs.** Failed batches are persisted and replayed; shutdown is
  bounded so it can never hang your program. See
  [Delivery guarantees explained](guides/delivery.md) for exactly what is and is
  not promised.
* **Many destinations.** Datadog, Elasticsearch, CloudWatch, Grafana Loki,
  Splunk HEC, OTLP (gRPC and HTTP), Prometheus, generic HTTP, UDP, TCP, files and
  the console, with fan-out and failover. See the
  [backend cookbook](guides/backends.md).
* **Keeping the bill down.** De-duplication, rate limits, adaptive sampling, a
  daily budget and a flight recorder that ships DEBUG context only when an
  error happens. See [Controlling volume](guides/volume.md).
* **Keeping secrets out.** Redaction runs before anything reaches the WAL or
  the network. See [Privacy and redaction](guides/privacy.md).
* **One engine, every language.** The same configuration file and the same
  behaviour from Rust, Python, JavaScript, Go and C, plus an
  [agent](quickstart/agent.md) for everything else.

## Status

The project is pre-1.0 and its packages are **not published yet**: install from
a checkout (each quickstart shows how). Where this book describes something
that has been designed and tested here but not yet exercised in a real
environment (some container recipes, the release workflow), it says so.

## Where to start

Pick your language under *Quickstarts*, then read
[Delivery guarantees explained](guides/delivery.md) and the
[`tpt-log.toml` gallery](guides/toml-gallery.md). If something is not working,
go to [Troubleshooting](guides/troubleshooting.md).

Contributions are accepted as issues only; see
[CONTRIBUTING.md](https://github.com/tpt-solutions/tpt-microlog/blob/master/CONTRIBUTING.md).
