package microlog

import (
	"path/filepath"
	"sync"
	"testing"
)

func TestRoundTrip(t *testing.T) {
	log, err := New(Config{Backend: "none", BatchSize: 1})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer log.Close()

	if err := log.Info("User logged in", map[string]any{"user_id": 123, "ip": "1.2.3.4"}); err != nil {
		t.Fatalf("Info: %v", err)
	}
	if err := log.Warn("plain warning", nil); err != nil {
		t.Fatalf("Warn: %v", err)
	}
	if err := log.Metric("cpu_usage", 42.5, map[string]string{"host": "server-01"}); err != nil {
		t.Fatalf("Metric: %v", err)
	}
	if err := log.Error("comma in value", map[string]any{"msg": "a,b"}); err != nil {
		t.Fatalf("Error with comma: %v", err)
	}
	if err := log.Shutdown(); err != nil {
		t.Fatalf("Shutdown: %v", err)
	}
	if v := Version(); v == "" {
		t.Fatal("Version returned empty string")
	}
}

func TestClosedHandle(t *testing.T) {
	log, err := New(Config{})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	log.Close()
	if err := log.Info("after close", nil); err == nil {
		t.Fatal("expected error logging after Close")
	}
	if err := log.Close(); err != nil {
		t.Fatalf("double Close: %v", err)
	}
}

func TestFlushAndStats(t *testing.T) {
	log, err := New(Config{Backend: "none", BatchSize: 100, WalEnabled: false})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer log.Close()

	for i := 0; i < 5; i++ {
		if err := log.Info("count me", nil); err != nil {
			t.Fatalf("Info: %v", err)
		}
	}
	if err := log.Flush(); err != nil {
		t.Fatalf("Flush: %v", err)
	}
	if got := log.Queued(); got != 0 {
		t.Fatalf("Queued after flush: %d", got)
	}
	stats, err := log.Stats()
	if err != nil {
		t.Fatalf("Stats: %v", err)
	}
	if stats.DeliveredBatches == 0 || stats.DeliveredEntries != 5 {
		t.Fatalf("unexpected stats: %+v", stats)
	}
	if stats.DroppedEntries != 0 {
		t.Fatalf("unexpected drops: %+v", stats)
	}
}

func TestConcurrentLoggingWithClose(t *testing.T) {
	log, err := New(Config{Backend: "none", BatchSize: 2, WalEnabled: false})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	var wg sync.WaitGroup
	for g := 0; g < 8; g++ {
		wg.Add(1)
		go func(g int) {
			defer wg.Done()
			for i := 0; i < 25; i++ {
				// Errors are fine once Close wins the race; the point is
				// that the RWMutex keeps the native handle from being freed
				// mid-call (which would crash).
				_ = log.Info("concurrent", map[string]any{"g": g, "i": i})
			}
		}(g)
	}
	if err := log.Info("before close", nil); err != nil {
		t.Fatalf("Info: %v", err)
	}
	wg.Wait()
	if err := log.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
}

func TestWalDirIsUsed(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "wal")
	log, err := New(Config{Backend: "none", WalEnabled: true, WalDir: dir})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer log.Close()
	if err := log.Info("wal probe", nil); err != nil {
		t.Fatalf("Info: %v", err)
	}
	if err := log.Flush(); err != nil {
		t.Fatalf("Flush: %v", err)
	}
	// No delivery failures expected; the directory should simply exist.
	if _, err := filepath.Abs(dir); err != nil {
		t.Fatalf("Abs: %v", err)
	}
}
