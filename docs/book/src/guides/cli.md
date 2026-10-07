# Command line

`tpt-microlog` (the `tpt-log-cli` crate) is for operators: set up, diagnose and
rescue.

```sh
cargo build --release -p tpt-log-cli      # target/release/tpt-microlog
```

It reads the same configuration as the library: `$TPT_LOG_CONFIG`, then
`./tpt-log.toml`, `~/.config/tpt-microlog/tpt-log.toml`,
`/etc/tpt-microlog/tpt-log.toml`, then `TPT_LOG_*` overrides. `--config PATH`
overrides the lookup.

| Command | What it does |
| --- | --- |
| `init [--preset NAME] [--force]` | writes a starter `tpt-log.toml` with the JSON Schema line for editor support |
| `doctor` | checks the config, that the backend builds, DNS and a TCP connect to the endpoint, and that the WAL is writable. Exits non-zero on a failure |
| `send-test [MESSAGE]` | sends one entry through the real pipeline and reports whether it was delivered |
| `tail FILE [-f]` | pretty-prints a file-backend JSONL log, following it with `-f` |
| `wal ls` | lists pending WAL batches with sizes and entry counts |
| `wal query [--dead] [--level L] [--grep S] [--limit N] [--json]` | decodes and searches undelivered entries (add `--dead` for dead-lettered ones) |
| `wal replay` | resends pending batches now, outside the running logger |
| `wal dead` | lists dead-lettered batches |
| `wal purge [--dead]` | deletes pending (or dead-lettered) batches |
| `support-bundle [--out FILE\|-] [--entries N]` | writes a masked diagnostic file to attach to a bug report |

## Rescuing a WAL

```sh
tpt-microlog wal ls                        # what is stuck?
tpt-microlog wal query --level error --grep payment
tpt-microlog doctor                        # can we reach the backend from here?
tpt-microlog wal replay                    # resend once the backend is back
```

A WAL directory is locked while a logger has it open, so `wal` commands need the
application to be stopped (or pointed at a different namespace).

## The support bundle

`support-bundle` is designed to be pasted into an issue. It contains the
effective configuration (secrets, header values and URL credentials masked),
environment variable *names* (values masked when they look secret), config and
transport checks, the WAL inventory and a sample of recent undelivered entries
with messages scrubbed by the built-in PII detectors and metadata reduced to key
names. It is still worth reading before you share it.
