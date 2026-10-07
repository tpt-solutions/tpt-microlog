# tpt-microlog-agent

A small sidecar for programs that have **no tpt-microlog binding** (PHP, Ruby,
shell scripts, legacy services, anything that can write to a socket). It
accepts logs over UDP, TCP, HTTP or a Unix socket and delivers them with the
usual batching, compression, WAL and retries, so those programs get
at-least-once delivery without a collector stack.

```text
your app --UDP/TCP/HTTP/unix--> tpt-microlog-agent --batch+zstd+WAL+retry--> Datadog / Loki / ...
```

## Run it

```sh
cargo build --release -p tpt-log-agent
TPT_LOG_BACKEND=loki TPT_LOG_ENDPOINT=http://localhost:3100 \
  target/release/tpt-microlog-agent --udp 127.0.0.1:5514 --http 127.0.0.1:8686
```

| Flag | Meaning |
| --- | --- |
| `--udp ADDR` | datagrams, one or more records each |
| `--tcp ADDR` | newline-delimited records on a persistent connection |
| `--http ADDR` | `POST /ingest` (needs `Content-Length`), `GET /healthz` |
| `--unix PATH` | Unix-domain stream socket, newline-delimited (Unix only) |
| `--max-connections N` | concurrent TCP/HTTP/Unix connections (default 256) |
| `--config FILE` | a `tpt-log.toml` (otherwise the usual search path + `TPT_LOG_*`) |

With no listener flag it serves UDP on `127.0.0.1:5514` and HTTP on
`127.0.0.1:8686`. Everything on the delivery side (backend, WAL, redaction,
sampling, dedup, `health_bind`, ...) is the normal tpt-microlog configuration.
Stop it with Ctrl+C or SIGTERM: it flushes before exiting.

## What to send

Per line, datagram or HTTP body, any of:

```json
{"level":"warn","message":"slow query","ms":840,"user":42}
{"type":"metric","name":"queue_depth","value":7,"tags":{"queue":"emails"}}
[{"msg":"a"},{"msg":"b","level":50}]
{"entries":[{"message":"tpt-microlog's own wire format"}]}
plain text lines are INFO entries
```

* message: `message`, `msg`, `log` or `text`
* level: `level`, `severity`, `lvl` or `levelname`: names (`warn`, `err`,
  `critical`, ...), pino numbers (10-60) or syslog severities (0-7)
* a client timestamp (`timestamp`, `time`, `ts`, `@timestamp`) is kept as
  `source_timestamp`; the entry time is when the agent received it
* every other field becomes metadata

```sh
echo '{"level":"error","message":"cron job failed","job":"backup"}' | nc -u -w0 127.0.0.1 5514
curl -s -X POST --data '[{"message":"hello"}]' http://127.0.0.1:8686/ingest   # {"accepted":1}
```

Limits: 1 MiB per line/datagram header, 8 MiB per HTTP body; chunked request
bodies are refused with `411`.

## Back-pressure and delivery

Clients are slowed down, not dropped, when the backend is slow, as long as
`queue_full_policy = "block"` (the default). For UDP the kernel drops what the
agent cannot read in time: use TCP or HTTP when every record matters. Pair the
agent with `durability = "wal-first"` if a hard kill of the agent itself must
not lose accepted records.

## Containers

```dockerfile
# tpt-log-agent/Dockerfile (build from the repository root)
FROM rust:1.89 AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p tpt-log-agent

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/tpt-microlog-agent /usr/local/bin/
EXPOSE 5514/udp 8686/tcp
ENTRYPOINT ["tpt-microlog-agent", "--udp", "0.0.0.0:5514", "--http", "0.0.0.0:8686"]
```

Kubernetes sidecar (apps in the pod log to `127.0.0.1:5514`):

```yaml
containers:
  - name: app
    image: my-app
    env: [{ name: LOG_AGENT, value: "127.0.0.1:5514" }]
  - name: log-agent
    image: my-registry/tpt-microlog-agent
    env:
      - { name: TPT_LOG_BACKEND, value: datadog }
      - name: TPT_LOG_API_KEY
        valueFrom: { secretKeyRef: { name: datadog, key: api-key } }
```

> **Status:** the agent is covered by end-to-end tests over real UDP, TCP,
> HTTP and (on Unix) socket connections. The Dockerfile and the Kubernetes
> snippet have not been built or applied.
