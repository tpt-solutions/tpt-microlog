# Contributing

**tpt-microlog is developed in the open but contributed to through issues
only.** Pull requests are not accepted; any that arrive will be closed
unreviewed, however good they are. This keeps the engine, the six language
bindings and their delivery guarantees in the hands of one maintainer who can
keep them coherent.

Issues are genuinely welcome, and a good issue is a real contribution.

## What to open an issue for

- **A bug**, especially lost, duplicated or reordered logs, hangs on shutdown,
  and anything where the WAL did not behave as documented.
- **A backend or integration that does not behave** against the real service
  (include the service and version).
- **A feature request**, described as the problem you have rather than the
  implementation you imagine.
- **A documentation gap** or an example that does not run.

Use the issue forms; they ask for what is needed to reproduce a problem.

## Writing a report that gets fixed quickly

1. **Say which binding and version** (`pip show tpt-microlog`, `npm ls
   tpt-microlog`, `cargo tree -i tpt-log-core`, `go list -m`), your OS, and the
   backend.
2. **Give a minimal reproduction**: the smallest program or `tpt-log.toml` that
   shows it. `tpt-microlog doctor` output is very useful for delivery problems.
3. **Include what you expected and what happened.** For missing logs, the
   `stats()` counters (`dropped_entries`, `wal_writes`, ...) and any message
   reported through `set_on_error` / stderr tell us where they went.
4. **Redact secrets** (API keys, tokens, customer data) before pasting.

## Security issues

Do **not** open a public issue for a vulnerability. See
[`SECURITY.md`](SECURITY.md).

## Conduct

Be respectful; see [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).

## Licensing

The project is dual-licensed under MIT or Apache-2.0. Anything you post in an
issue (code snippets, configs) may be used to fix the problem under those
terms.
