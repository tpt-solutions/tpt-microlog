# Prometheus + Grafana

```sh
docker compose up -d
TPT_LOG_CONFIG=tpt-log.toml python ../demo.py --seconds 120   # keep it running while scraping
```

* metrics endpoint: <http://localhost:9464/metrics>
* Prometheus: <http://localhost:9090> (target `tpt-microlog-demo` should be UP)
* Grafana: <http://localhost:3000> -> dashboard **tpt-microlog demo**

Only `metric()` entries are exposed (as gauges named after the metric, with the
tags as labels); log lines are ignored by this backend. On Docker Desktop
(macOS/Windows) `host.docker.internal` resolves natively; on Linux the compose
file maps it with `host-gateway`.

> **Status: the app side is tested.** `demo.py` was run against this
> `tpt-log.toml` and `/metrics` was scraped. The compose stack, Prometheus
> config and Grafana provisioning were syntax-checked only.
