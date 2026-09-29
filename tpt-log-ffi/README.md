# tpt-log-ffi

C ABI boundary for [tpt-microlog](https://github.com/tpt-solutions/tpt-microlog) — the microscopic telemetry & log router.

`tpt-log-ffi` exposes the Rust engine through a stable, dependency-free C ABI so
that C, C++, and language bindings such as Go can embed the logger without
knowing anything about Rust. The entire surface is declared in a single header,
[`include/tpt_microlog.h`](include/tpt_microlog.h).

- **API docs:** <https://docs.rs/tpt-log-ffi>
- **Repository:** <https://github.com/tpt-solutions/tpt-microlog/tree/master/tpt-log-ffi>

## Build

```sh
cargo build --release -p tpt-log-ffi
```

The crate is built as `staticlib`, `cdylib`, and `rlib`, producing:

| Platform | Static library | Shared library |
| --- | --- | --- |
| Linux | `target/release/libtpt_log_ffi.a` | `target/release/libtpt_log_ffi.so` |
| macOS | `target/release/libtpt_log_ffi.a` | `target/release/libtpt_log_ffi.dylib` |
| Windows MSVC | `target/release/tpt_log_ffi.lib` | `target/release/tpt_log_ffi.dll` |

Link it and include the header:

```sh
cc app.c -I tpt-log-ffi/include -L target/release -ltpt_log_ffi -lpthread -ldl -lm
```

## Usage (C)

```c
#include "tpt_microlog.h"

int main(void) {
    tpt_log_config config = {0};
    config.backend = "datadog";
    config.api_key = "your-api-key";
    config.service = "checkout-api";

    tpt_log_handle* log = tpt_log_create(&config);
    if (log == NULL) {
        fprintf(stderr, "tpt-microlog: %s\n", tpt_log_last_error());
        return 1;
    }

    tpt_log_info(log, "User logged in", "user_id=123,ip=1.2.3.4");
    tpt_log_error(log, "Database connection failed", "error=timeout,retry_count=3");
    tpt_log_metric(log, "cpu_usage", 42.5, "host=server-01");

    tpt_log_shutdown(log);
    tpt_log_destroy(log);
    return 0;
}
```

Metadata and tags are passed as a `"k=v,k2=v2"` string; pass `NULL` for none.

## Usage (C++)

The header is `extern "C"`-guarded, so C++ can include it directly:

```cpp
#include "tpt_microlog.h"

tpt_log_config config{};
config.backend = "file";
config.service = "cpp-service";

tpt_log_handle* log = tpt_log_create(&config);
tpt_log_info(log, "hello from C++", nullptr);
tpt_log_shutdown(log);
tpt_log_destroy(log);
```

## API

| Function | Description |
| --- | --- |
| `tpt_log_create` | Creates a logger from a config; `NULL` uses all defaults. Returns `NULL` on failure |
| `tpt_log_trace` … `tpt_log_fatal` | Level-based logging with optional metadata |
| `tpt_log_log` | Logging with an explicit `tpt_log_level` code |
| `tpt_log_metric` | Records a numeric metric with optional tags |
| `tpt_log_shutdown` | Flushes pending batches and drains the WAL (idempotent) |
| `tpt_log_destroy` | Frees the handle; flushes implicitly |
| `tpt_log_queued` | Entries still buffered in the ingestion queue |
| `tpt_log_version` | Static library version string |
| `tpt_log_last_error` | Message for the most recent failure on the calling thread |

### Configuration

`tpt_log_config` is zero-initialized with `= {0}`; `NULL` strings and zero
numbers mean "use the default". `wal_enabled` is a tri-state: `-1` default,
`0` off, `1` on.

| Field | Default | Description |
| --- | --- | --- |
| `backend` | `none` | Backend name (see below) |
| `endpoint` | backend default | Base URL for HTTP-style backends |
| `api_key` | none | Secret token |
| `service` | none | Logical service name |
| `batch_size` | `100` | Entries per batch |
| `flush_interval` | `1.0` s | Flush interval in seconds |
| `compression_level` | `3` | zstd level |
| `max_retries` | `10` | Delivery attempts |
| `wal_enabled` | default | Tri-state WAL switch |
| `wal_dir` | platform default | WAL directory |

Backends: `none`, `http`, `datadog`, `elasticsearch`, `cloudwatch`,
`prometheus`, `grpc`, `udp`, `tcp`, `file`.

## Error handling

Every fallible function returns an `int32_t` code from `tpt_log_code`:

| Code | Value | Meaning |
| --- | --- | --- |
| `TPT_LOG_OK` | 0 | Success |
| `TPT_LOG_NULL_ARGUMENT` | 1 | A required pointer was `NULL` |
| `TPT_LOG_INVALID_UTF8` | 2 | A string argument was not valid UTF-8 |
| `TPT_LOG_CONFIG_ERROR` | 3 | The configuration was rejected |
| `TPT_LOG_INVALID_METADATA` | 4 | The `k=v` metadata string could not be parsed |

A logging or metric call returns the error code immediately and does **not**
enqueue the entry, so malformed metadata is never silently dropped. Commas
inside double-quoted values (`msg="a,b"`) do not split pairs, and values are
parsed as JSON when possible and kept as strings otherwise.

For failures with no return code
(`tpt_log_create`), use `tpt_log_last_error()`, whose returned pointer stays
valid until the next failing call on the same thread.

## Thread safety

`tpt_log_handle` is safe to use from multiple threads concurrently. A handle
must not be destroyed while other threads are still logging to it. The strings
passed in `tpt_log_config` are borrowed for the duration of the
`tpt_log_create` call only.

## Consumers

- [`tpt-log-go`](../tpt-log-go) — cgo wrapper on top of this header.
- [`examples/c/basic.c`](../examples/c/basic.c) — a complete buildable example.

## License

Dual-licensed under [MIT](https://github.com/tpt-solutions/tpt-microlog/blob/master/LICENSE-MIT) or [Apache-2.0](https://github.com/tpt-solutions/tpt-microlog/blob/master/LICENSE-APACHE),
at your option.
