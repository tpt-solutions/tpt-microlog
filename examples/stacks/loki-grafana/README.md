# Loki + Grafana

```sh
docker compose up -d
TPT_LOG_CONFIG=tpt-log.toml python ../demo.py --seconds 20
```

Open <http://localhost:3000> -> **Explore** -> data source *Loki* and query:

```logql
{service="demo-app"}                  # everything
{service="demo-app", level="ERROR"}   # errors only
```

Entries are grouped into streams by `(service, level)`; the message is the log
line and the entry metadata travels as Loki structured metadata.

> **Status: untested.** The TOML and YAML are syntax-checked only; the stack has
> not been started. Structured metadata needs Loki 3.x with the v13 schema (the
> default config in the pinned image).
