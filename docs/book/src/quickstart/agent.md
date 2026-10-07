# Any language: the agent

For PHP, Ruby, shell scripts and anything else without a binding, run
`tpt-microlog-agent` next to it and send logs over a socket. The agent applies
the same batching, compression, WAL and retries as the library.

```sh
cargo build --release -p tpt-log-agent
TPT_LOG_BACKEND=loki TPT_LOG_ENDPOINT=http://localhost:3100 \
  target/release/tpt-microlog-agent --udp 127.0.0.1:5514 --http 127.0.0.1:8686
```

```sh
echo '{"level":"error","message":"cron job failed","job":"backup"}' | nc -u -w0 127.0.0.1 5514
curl -s -X POST --data '[{"message":"hello"}]' http://127.0.0.1:8686/ingest     # {"accepted":1}
```

Listeners: `--udp`, `--tcp` (newline-delimited), `--http` (`POST /ingest`,
`GET /healthz`) and `--unix PATH`. It accepts JSON objects (level from
`level`/`severity`, message from `message`/`msg`), metrics
(`{"type":"metric","name":"cpu","value":0.5}`), batches, and plain text lines.

Choose the transport by how much you can afford to lose: UDP is fire-and-forget
(the kernel drops what the agent cannot read in time); TCP, HTTP and Unix
sockets apply back-pressure instead. Full details, including a Dockerfile and a
Kubernetes sidecar snippet, are in the
[agent README](https://github.com/tpt-solutions/tpt-microlog/blob/master/tpt-log-agent/README.md).
