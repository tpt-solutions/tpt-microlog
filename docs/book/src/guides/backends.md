# Backend cookbook

Pick a backend with `backend = "..."`. Each section lists the keys it needs; the
[gallery](toml-gallery.md) has complete, validated files, and
`tpt-microlog doctor` checks DNS/TCP reachability and credentials presence.

| `backend` | Destination |
| --- | --- |
| `none` | discard (in-memory only; for tests) |
| `console` / `stdout` | terminal: pretty on a TTY, JSON lines when piped |
| `custom` / `http` | any HTTP endpoint accepting JSON |
| `datadog` | Datadog Logs API v2 |
| `elasticsearch` / `es` | Elasticsearch / Elastic Cloud bulk API |
| `cloudwatch` | AWS CloudWatch Logs |
| `loki` | Grafana Loki push API |
| `splunk` | Splunk HTTP Event Collector |
| `otlp-http` | OpenTelemetry Collector, OTLP over HTTP (JSON) |
| `grpc` / `otlp` | OpenTelemetry Collector, OTLP over gRPC |
| `prometheus` | embedded `/metrics` endpoint (pull) |
| `tcp` / `udp` | raw sockets to `net_endpoint` |
| `file` | local JSONL with rotation |
| `fanout` | several of the above at once, or in failover order |

## Generic HTTP

```toml
backend = "http"
endpoint = "https://logs.example.com/v1"
extra_headers = ["Authorization=Bearer <token>"]
```

The body is plain JSON, `{"entries": [{timestamp, level, type, message, metadata}]}`,
by default. `body_mode = "zstd"` sends the compressed batch with
`Content-Encoding: zstd` instead, if your receiver understands it.

## Datadog

`api_key` is required. `datadog_site = "datadoghq.eu"` (or the `datadog-eu`
preset) selects another region. Metadata becomes top-level attributes;
`datadog_metrics = true` also sends `metric()` values to the Metrics API.

## Elasticsearch / Elastic Cloud

`endpoint` plus either `api_key` or `es_basic_auth = "user:password"`.
`es_index` names the index (default `tpt-microlog`). The adapter uses the Bulk
API and reads the per-item results: a mapping conflict on one document is
reported rather than silently losing it, and HTTP 429 is retried.

## CloudWatch Logs

`aws_region`, `aws_log_group` and `aws_log_stream`, with credentials from
`aws_access_key` + `aws_secret_key` (+ `aws_session_token`) or the standard
`AWS_*` environment variables. `aws_auto_create_stream = true` creates the group
and stream on first use. Requests are signed with SigV4.

## Grafana Loki

```toml
preset = "grafana-loki"      # http://localhost:3100
```

Entries are grouped into streams by `(service, level)`; the message is the line
and the rest of the metadata travels as Loki structured metadata (Loki 3.x).
For Grafana Cloud or an authenticated Loki, add
`extra_headers = ["Authorization=Basic <base64 user:token>"]`.

## Splunk HEC

`endpoint = "https://host:8088"` and `api_key = "<HEC token>"`.

## OpenTelemetry

`preset = "otel-collector"` sends OTLP/gRPC to `localhost:4317`; the connection is
made lazily, so your application can start before the collector.
`backend = "otlp-http"` posts JSON to `<endpoint>/v1/logs` (default port 4318).

## Prometheus

A pull backend: `metric()` entries are exposed as gauges (named after the
metric, tags as labels) on `prometheus_bind` (default `127.0.0.1:9090`; use
another port if Prometheus itself runs there). Log lines are ignored by this
backend.

## TCP, UDP, file, console

`net_endpoint = "host:port"` for `tcp`/`udp` (`tcp_ack = true` waits for a
one-byte acknowledgement per frame). `file` writes JSONL to `file_path` and
rotates by `file_rotation_bytes` / `file_rotation_seconds`, keeping
`file_rotations` files. `console` needs nothing.

## Several destinations

```toml
backend = "fanout"
backends = ["datadog", "file"]   # send to both
# failover = true                # ...or stop at the first backend that works
```

Every listed backend shares the same configuration (endpoints, credentials,
headers), so a fan-out entry names a *kind* of backend. With `failover`, a
permanent rejection moves on to the next backend while a transient failure
stops and is retried, so a flapping primary does not spray duplicates.

## TLS and proxies

`ca_cert = "/path/bundle.pem"` adds a trusted CA for HTTPS backends and
`proxy = "http://proxy:3128"` routes the HTTP-based backends through a proxy. An unreadable
CA file or an invalid proxy URL is reported through `on_error` and ignored.
