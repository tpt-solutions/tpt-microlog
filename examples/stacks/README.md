# Stacks

One-command local observability stacks to try tpt-microlog against. Each
folder has a `docker-compose.yml`, a ready `tpt-log.toml` and a README.
`demo.py` generates traffic for all of them.

| Stack | Backend | Tested |
| --- | --- | --- |
| [`elk/`](elk) | `elasticsearch` | config syntax only |
| [`loki-grafana/`](loki-grafana) | `loki` | config syntax only |
| [`otel-collector/`](otel-collector) | `grpc` / `otlp-http` | config syntax only |
| [`prometheus-grafana/`](prometheus-grafana) | `prometheus` | app side verified, compose syntax only |

"Config syntax only" means the TOML loads in tpt-microlog and the YAML/JSON
parses; nobody has started these containers yet, so expect to adjust image
tags or ports. Please report what you had to change.

```sh
pip install tpt-microlog
cd examples/stacks/loki-grafana && docker compose up -d
TPT_LOG_CONFIG=tpt-log.toml python ../demo.py --seconds 20
```
