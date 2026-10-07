# tpt-microlog-go

Go binding for [tpt-microlog](https://github.com/tpt-solutions/tpt-microlog) — the microscopic telemetry & log router.

This module is a thin [cgo](https://pkg.go.dev/cmd/cgo) wrapper over the native
[`tpt-log-ffi`](../tpt-log-ffi) C ABI. All batching, compression, retry, and
write-ahead log behaviour comes from the Rust engine; this package only converts
Go values into C calls and surfaces errors.

- **Module:** `github.com/tpt-solutions/tpt-microlog/tpt-log-go` (a subdirectory module: tags are `tpt-log-go/vX.Y.Z`)
- **Repository:** <https://github.com/tpt-solutions/tpt-microlog/tree/master/tpt-log-go>

## Requirements

- Go 1.22 or newer
- A C compiler
- cgo enabled (the default on supported platforms)

## Install

```sh
go get github.com/tpt-solutions/tpt-microlog/tpt-log-go
```

The package links the native engine (`libtpt_log_ffi.a`), so you need the
static library for your platform. The C header is already vendored in the
module, so no `CGO_CFLAGS` are needed.

### Option 1: prebuilt library (linux/darwin amd64+arm64, windows amd64)

Release assets are named `libtpt_log_ffi-<goos>_<goarch>.tar.gz` with a
`.sha256` beside each. The fetcher downloads, verifies and unpacks the right
one:

```sh
go run github.com/tpt-solutions/tpt-microlog/tpt-log-go/cmd/fetchlib@latest -version v0.1.0
# prints:  export CGO_LDFLAGS="-L/abs/path/tpt-lib/linux_amd64"
export CGO_LDFLAGS="-L$PWD/tpt-lib/linux_amd64"
go build ./...
```

It refuses to install a library whose SHA-256 does not match. Use `-dest`,
`-os`, `-arch` or `-base-url` (for a mirror) to adjust. In a clone of this
repository you can fetch straight into `tpt-log-go/lib/<goos>_<goarch>/`, where
the module already looks (`-dest lib/linux_amd64`) and no `CGO_LDFLAGS` are
needed.

### Option 2: build it yourself

```sh
cargo build --release -p tpt-log-ffi
export CGO_LDFLAGS="-L$PWD/target/release"
```

The directory also contains the shared library; to force static linking add
`-Wl,-Bstatic -ltpt_log_ffi -Wl,-Bdynamic` or copy `libtpt_log_ffi.a` into its
own directory. On Windows use the GNU toolchain (cgo needs `gcc`):

```sh
cargo build --release -p tpt-log-ffi --target x86_64-pc-windows-gnu
export CGO_LDFLAGS="-L$PWD/target/x86_64-pc-windows-gnu/release -Wl,-Bstatic -ltpt_log_ffi -Wl,-Bdynamic"
```

The platform's system libraries (`-lm -ldl -lpthread` on Linux, Security and
CoreFoundation frameworks on macOS, `-lntdll -lws2_32 -lbcrypt -luserenv` on
Windows) are already named in `cgo_link.go`.

## Usage

```go
package main

import (
	"log"

	microlog "github.com/tpt-solutions/tpt-microlog/tpt-log-go"
)

func main() {
	lg, err := microlog.New(microlog.Config{
		Backend: "datadog",
		ApiKey:  "your-api-key",
		Service: "checkout-api",
	})
	if err != nil {
		log.Fatalf("tpt-microlog: %v", err)
	}
	defer lg.Close()

	if err := lg.Info("User logged in", map[string]any{"user_id": 123, "ip": "1.2.3.4"}); err != nil {
		log.Printf("log error: %v", err)
	}
	if err := lg.Metric("cpu_usage", 42.5, map[string]string{"host": "server-01"}); err != nil {
		log.Printf("metric error: %v", err)
	}

	if err := lg.Shutdown(); err != nil {
		log.Printf("shutdown error: %v", err)
	}
}
```

A complete example is available in
[`examples/go/datadog.go`](../examples/go/datadog.go).

## `log/slog` integration

```go
logger := slog.New(microlog.NewSlogHandler(log, &slog.HandlerOptions{Level: slog.LevelDebug}))
logger.Info("user logged in", "user_id", 123)
logger.WithGroup("req").Warn("slow", "ms", 840) // metadata: {"req": {"ms": 840}}
logger.Log(ctx, microlog.LevelFatal, "unrecoverable")
```

Levels below DEBUG map to TRACE; `microlog.LevelFatal` (ERROR+4) maps to FATAL.
Attributes become metadata, groups become nested objects, and errors,
durations and times are rendered as strings. The handler never closes the
`MicroLog`.

## API

| Member | Description |
| --- | --- |
| `New(cfg Config) (*MicroLog, error)` | Creates a logger; returns an error if the native engine cannot start |
| `Trace` / `Debug` / `Info` / `Warn` / `Error` / `Fatal` | Level logging with an optional metadata map |
| `Metric(name string, value float64, tags map[string]string) error` | Numeric metric with tags |
| `Queued() int` | Entries still buffered in the ingestion queue |
| `Shutdown() error` | Flushes and drains the WAL (idempotent) |
| `Close() error` | Flushes implicitly and releases the native handle |
| `Version() string` | Native library version string |

All logging methods return an error describing the most recent native failure
via `tpt_log_last_error()`.

### Configuration

| Field | Default | Description |
| --- | --- | --- |
| `Backend` | `none` | Backend name (see below) |
| `Endpoint` | backend default | Base URL for HTTP-style backends |
| `ApiKey` | — | Secret token |
| `Service` | — | Logical service name |
| `BatchSize` | `100` | Entries per batch |
| `FlushInterval` | `1.0` s | Seconds between flushes |
| `MaxRetries` | `10` | Delivery attempts |
| `WalEnabled` | `false` | Persist failed batches to disk for replay |

Backends: `none`, `http`, `datadog`, `elasticsearch`, `cloudwatch`,
`prometheus`, `grpc`, `udp`, `tcp`, `file`.

## Concurrency and lifetime

`*MicroLog` is safe for concurrent use by multiple goroutines. You must still
call `Close` (or `Shutdown` followed by `Close`) when the logger is no longer
needed; the native handle is not reclaimed automatically.

## Testing

```sh
go test ./...
```

`microlog_test.go` covers the configuration mapping, metadata encoding, and
error propagation paths. Because it links the native library, the Rust artifact
must be built before running the tests.

## License

Dual-licensed under [MIT](../LICENSE-MIT) or [Apache-2.0](../LICENSE-APACHE),
at your option.
