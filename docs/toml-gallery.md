# `tpt-log.toml` gallery

Copy-paste starting points for common situations. Every block on this page is
parsed and validated by the test suite (`tpt-log-core/tests/toml_gallery.rs`),
so keys and values stay current. Placeholders such as `<your-key>` must be
replaced; prefer `TPT_LOG_API_KEY` or your secret manager over committing keys.

Add this first line for editor completion and validation:

```
#:schema https://raw.githubusercontent.com/tpt-solutions/tpt-microlog/master/tpt-log-core/tpt-log.schema.json
```

Layering is code overrides > `TPT_LOG_*` environment > `tpt-log.toml` > defaults.
Start with `tpt-microlog init --preset <name>` to generate a file, and check a
setup with `tpt-microlog doctor`.

## Local development

Pretty console output on a terminal, JSON lines when piped. No WAL, small batches.

```toml
preset = "dev"
service = "my-app"
```

## Production baseline

Batching, WAL and enrichment on, INFO and above, generous retries.

```toml
preset = "prod"
backend = "http"
endpoint = "https://logs.example.com/v1"
extra_headers = ["Authorization=Bearer <your-token>"]
service = "checkout-api"
```

## Datadog

```toml
backend = "datadog"
api_key = "<your-datadog-api-key>"
service = "checkout-api"
enrichment = true
```

EU site:

```toml
preset = "datadog-eu"
api_key = "<your-datadog-api-key>"
service = "checkout-api"
```

## Elasticsearch / Elastic Cloud

```toml
backend = "elasticsearch"
endpoint = "https://my-deployment.es.example.cloud:443"
api_key = "<base64 id:key>"
es_index = "app-logs"
service = "checkout-api"
```

Self-managed with basic auth:

```toml
backend = "elasticsearch"
endpoint = "https://es.internal:9200"
es_basic_auth = "elastic:<password>"
es_index = "app-logs"
```

## Grafana Loki

```toml
preset = "grafana-loki"
service = "checkout-api"
```

Grafana Cloud or any authenticated Loki:

```toml
backend = "loki"
endpoint = "https://logs-prod.example.grafana.net"
extra_headers = ["Authorization=Basic <base64 user:token>"]
service = "checkout-api"
```

## Splunk HEC

```toml
backend = "splunk"
endpoint = "https://splunk.internal:8088"
api_key = "<hec-token>"
service = "checkout-api"
```

## OpenTelemetry Collector

gRPC (the collector's port 4317):

```toml
preset = "otel-collector"
service = "checkout-api"
```

OTLP over HTTP (port 4318):

```toml
backend = "otlp-http"
endpoint = "http://otel-collector:4318"
service = "checkout-api"
```

## AWS CloudWatch Logs

```toml
preset = "cloudwatch-lambda"
aws_region = "us-east-1"
aws_access_key = "<access-key-id>"
aws_secret_key = "<secret-access-key>"
aws_log_group = "/app/checkout"
aws_log_stream = "api-1"
```

## Prometheus (pull)

Serves `metric()` values as gauges at `/metrics`.

```toml
backend = "prometheus"
prometheus_bind = "0.0.0.0:9464"
service = "checkout-api"
```

## Local JSONL file with rotation

```toml
backend = "file"
file_path = "/var/log/my-app/app.jsonl"
file_rotation_bytes = 10485760
file_rotations = 5
service = "my-app"
```

## Syslog-style UDP / TCP

```toml
backend = "tcp"
net_endpoint = "logs.internal:5140"
tcp_ack = true
service = "my-app"
```

```toml
backend = "udp"
net_endpoint = "logs.internal:5140"
service = "my-app"
```

## Several destinations

Send everything to Datadog and keep a local copy:

```toml
backend = "fanout"
backends = ["datadog", "file"]
api_key = "<your-datadog-api-key>"
file_path = "/var/log/my-app/app.jsonl"
service = "checkout-api"
```

Fail over to a file when the primary backend is rejecting data:

```toml
backend = "fanout"
backends = ["loki", "file"]
failover = true
endpoint = "https://loki.internal:3100"
file_path = "/var/log/my-app/fallback.jsonl"
service = "checkout-api"
```

## Never lose logs on a crash

Writes every batch to the WAL before the first send, at the cost of one disk
write per batch.

```toml
backend = "http"
endpoint = "https://logs.example.com/v1"
durability = "wal-first"
wal_enabled = true
wal_max_size_bytes = 524288000
wal_dead_letter_replays = 12
shutdown_timeout_ms = 10000
```

## Keep secrets and PII out of logs

```toml
backend = "http"
endpoint = "https://logs.example.com/v1"
redact_keys = ["password", "authorization", "api_key", "token"]
redact_patterns = ["ORD-\\d{6}"]
redact_builtin = true
```

## Cut volume without losing errors

WARN and above are never sampled.

```toml
backend = "http"
endpoint = "https://logs.example.com/v1"
min_level = "DEBUG"
filter_regex = "^GET /healthz"
filter_tags = ["noise=true"]

[sample_rate_by_level]
DEBUG = 0.05
INFO = 0.5
```

## Collapse noisy repeats

Identical entries (same level and message) within the window are folded into
the first one, which then carries a `repeat_count`. Windows that end with
suppressed repeats emit a summary entry (`repeat_summary = true`).

```toml
backend = "http"
endpoint = "https://logs.example.com/v1"
dedup_window_ms = 5000
```

## Ship DEBUG context only when something fails

The last 200 DEBUG/TRACE entries are held in memory (redacted) and shipped,
oldest first, right before the next ERROR or FATAL. Until then DEBUG costs no
network or storage.

```toml
backend = "http"
endpoint = "https://logs.example.com/v1"
min_level = "INFO"
flight_recorder_size = 200
flight_recorder_max_level = "DEBUG"
```

## Stay inside a daily volume budget

Sampling tightens automatically so the projected daily total lands on the
budget. WARN and above are never limited.

```toml
backend = "datadog"
api_key = "<your-datadog-api-key>"
daily_budget_entries = 5000000
```

## Shed load before the queue fills

Routine entries are sampled harder as the ingestion queue fills (from half
full down to the floor); errors always pass. A per-second cap adds a hard
ceiling on routine volume.

```toml
backend = "http"
endpoint = "https://logs.example.com/v1"
queue_capacity = 20000
adaptive_sampling = true
adaptive_sampling_floor = 0.05
rate_limit_per_sec = 2000
```

## Health endpoint and self-telemetry

`GET /healthz` returns 200 (`ok` or `degraded`) or 503 (`down`/`stopped`) with
a JSON body; `GET /stats` adds the counters. The same numbers are also emitted
as `microlog.*` metrics every 30 seconds.

```toml
backend = "datadog"
api_key = "<your-datadog-api-key>"
health_bind = "127.0.0.1:9477"
self_metrics_interval_ms = 30000
```

## Bounded memory under backpressure

Drop the oldest buffered entries instead of blocking the application.

```toml
backend = "http"
endpoint = "https://logs.example.com/v1"
queue_capacity = 20000
queue_full_policy = "drop_oldest"
memory_limit_bytes = 5242880
```

## Through a corporate proxy with a private CA

```toml
backend = "http"
endpoint = "https://logs.example.com/v1"
proxy = "http://proxy.corp.example:3128"
ca_cert = "/etc/ssl/certs/corp-root.pem"
```
