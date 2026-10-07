// tpt-microlog Go example — structured logs + metrics to Datadog.
//
// Build the native library once (see tpt-log-go package docs for
// platform-specific link flags), then run:
//
//	go run examples/go/datadog.go
package main

import (
	"log"
	"os"

	microlog "github.com/tpt-solutions/tpt-microlog/tpt-log-go"
)

func main() {
	logger, err := microlog.New(microlog.Config{
		Backend:       "datadog",
		ApiKey:        os.Getenv("DD_API_KEY"),
		Service:       "checkout-api",
		BatchSize:     100,
		FlushInterval: 1.0,
	})
	if err != nil {
		log.Fatalf("create logger: %v", err)
	}
	defer logger.Close()

	// Structured message with typed metadata.
	if err := logger.Info("User logged in", map[string]any{
		"user_id": 123,
		"ip":      "1.2.3.4",
	}); err != nil {
		log.Fatalf("info: %v", err)
	}

	// Numeric metric with tags.
	if err := logger.Metric("cpu_usage", 42.5, map[string]string{
		"host": "server-01",
	}); err != nil {
		log.Fatalf("metric: %v", err)
	}

	if err := logger.Warn("Slow query", map[string]any{
		"duration_ms": 950,
		"query":       "SELECT ...",
	}); err != nil {
		log.Fatalf("warn: %v", err)
	}

	// Flush pending batches and drain the WAL before exit.
	if err := logger.Shutdown(); err != nil {
		log.Fatalf("shutdown: %v", err)
	}
}
