# Go

The Go package is a cgo wrapper over the native engine, so it needs the static
library `libtpt_log_ffi.a` and a C compiler.

```sh
go get github.com/tpt-solutions/tpt-microlog/tpt-log-go
```

The C header is vendored in the module. For the library, either fetch a prebuilt
one from a release (checksum-verified):

```sh
go run github.com/tpt-solutions/tpt-microlog/tpt-log-go/cmd/fetchlib@latest -version v0.1.0
export CGO_LDFLAGS="-L$PWD/tpt-lib/linux_amd64"      # printed by the command
```

or build it yourself (`cargo build --release -p tpt-log-ffi` and point
`CGO_LDFLAGS` at `target/release`). Prebuilt libraries exist for Linux and
macOS (amd64, arm64) and Windows (amd64, via MinGW). Releases are not published
yet, so today you build it yourself.

## A first logger

```go
package main

import (
	"log"

	microlog "github.com/tpt-solutions/tpt-microlog/tpt-log-go"
)

func main() {
	l, err := microlog.NewFromTOML(`
backend = "file"
file_path = "app.jsonl"
service = "checkout"
redact_keys = ["password"]
`)
	if err != nil {
		log.Fatal(err)
	}
	defer l.Close() // flushes and frees the native handle

	l.Info("user logged in", map[string]any{"user_id": 123})
	l.Metric("cpu", 42.5, map[string]string{"host": "a"})
}
```

`microlog.New(microlog.Config{...})` takes the common options directly;
`NewFromTOML` accepts every key in the [gallery](../guides/toml-gallery.md).

## `log/slog`

```go
logger := slog.New(microlog.NewSlogHandler(l, nil))
logger.Info("user logged in", "user_id", 123)
logger.WithGroup("req").Warn("slow", "ms", 840)         // nested metadata
logger.Log(ctx, microlog.LevelFatal, "unrecoverable")    // FATAL has no slog level
```

## Operating it

```go
microlog.SetErrorHandler(func(kind, msg string) { /* delivery, wal, worker, config */ })
stats, _ := l.Stats()    // DeliveredEntries, DroppedEntries, SampledEntries, ...
health, _ := l.Health()  // Status "ok" | "degraded" | "down" | "stopped", WalBytes, ...
```

`*MicroLog` is safe for concurrent use. Always `Close()` it: the native handle
is not reclaimed by the garbage collector.
