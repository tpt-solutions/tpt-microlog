# Troubleshooting

Start with three commands: `tpt-microlog doctor`, `log.health()` (or
`GET /healthz`), and `log.stats()`. Install an `on_error` callback early; most
problems announce themselves there.

## "My logs are missing"

Find the stage where they were lost by reading the counters:

| You see | What happened | What to do |
| --- | --- | --- |
| `sampled_entries`, `rate_limited_entries` or `deduplicated_entries` growing | A [volume control](volume.md) removed them on purpose | Relax the setting, or check `min_level` / `filter_*` which are not counted |
| `dropped_entries` growing | The queue was full under `drop_newest`/`drop_oldest`, or the WAL was unavailable | Use `queue_full_policy = "block"`, enlarge `queue_capacity`, or fix the backend |
| `wal_writes` growing, `delivered_entries` flat | The backend is not accepting batches; they are safe in the WAL | `doctor`; `wal query`; fix credentials/network; they replay automatically |
| files in `wal/dead/` | A batch failed replay repeatedly | `wal query --dead` to see why; `wal replay` after fixing |
| none of the above, but entries are absent after a crash | `durability = "best-effort"` loses the in-memory buffer on a hard kill | Use `durability = "wal-first"` |
| nothing is ever logged | Nobody called `shutdown()` before exit, or a forked child process detached | Call `shutdown()`/`Close()`; in Python use `with MicroLog(...)`; set up logging after the fork |

Remember `min_level`: the default level filter drops nothing, but a preset such
as `prod` sets INFO, and a flight recorder keeps DEBUG out of the normal stream.

## Messages you may see

* **`ignoring unknown config key 'btach_size' (did you mean 'batch_size'?)`**: a
  typo in `tpt-log.toml`. Add `#:schema ...` (see `tpt-microlog init`) for editor
  validation.
* **`WAL disabled: WAL directory ... is locked by another process`**: another
  process (or a logger you forgot to `shutdown()`) already has this WAL, which is
  namespaced by service + backend + endpoint. Give the second program a different
  `service`, or stop the first.
* **`delivery gave up after N attempts: ...`** / **`permanent delivery failure`**:
  the transport error follows. Authentication (401/403) and bad payloads (400) are
  permanent; connection errors and 5xx are retried first.
* **`WAL over its N byte cap: discarded the oldest ...`**: the outage outlasted
  the WAL. Raise `wal_max_size_bytes` or accept the loss.
* **`batch dead-lettered after N failed replays`**: see the table above.
* **`worker panicked ... restarting`**: a bug; please open an issue with the
  support bundle.
* **`gRPC connect failed`** is only raised by explicit connects; the logger
  connects lazily and keeps retrying.

## The application hangs on shutdown or on logging

`shutdown()` is bounded by `shutdown_timeout_ms` (default 5 s) and spills the
rest to the WAL. A hang *while logging* means `queue_full_policy = "block"` with
a full queue because the backend is down; switch to a `drop_*` policy if logging
must never wait, and watch `health()` for `degraded`.

## Platform notes

* **Go**: cgo needs a C compiler and the static library; a path containing
  spaces breaks `${SRCDIR}` in cgo flags, so build from a path without spaces.
  See the [Go quickstart](../quickstart/go.md).
* **Windows**: the native library for Go is built with the MinGW toolchain, not
  MSVC.
* **Containers**: put `wal_dir` on a volume if batches must survive a restart;
  run the [agent](../quickstart/agent.md) as a sidecar for languages without a
  binding.
* **Browsers**: the WebAssembly build has no disk WAL; use the offline queue.

## Still stuck?

Open an issue with the output of `tpt-microlog support-bundle` (it masks
secrets) and a minimal reproduction. Contributions are accepted as issues only.
