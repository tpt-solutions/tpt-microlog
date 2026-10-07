# OpenTelemetry Collector

```sh
docker compose up -d
TPT_LOG_CONFIG=tpt-log.toml python ../demo.py --seconds 10
docker compose logs otel-collector     # the debug exporter prints each record
```

Replace the `debug` exporter in `otel-collector.yaml` with whatever backend you
actually use (Loki, a vendor exporter, ...); tpt-microlog only needs to reach
the OTLP receiver.

> **Status: untested.** Syntax-checked only; the stack has not been started.
