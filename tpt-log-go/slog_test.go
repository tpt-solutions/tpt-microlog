package microlog

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"os"
	"path/filepath"
	"testing"
	"time"
)

type entry struct {
	Level    string         `json:"level"`
	Message  string         `json:"message"`
	Metadata map[string]any `json:"metadata"`
}

func readEntries(t *testing.T, path string) map[string]entry {
	t.Helper()
	f, err := os.Open(path)
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	defer f.Close()
	out := map[string]entry{}
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 1<<20), 1<<20)
	for sc.Scan() {
		var e entry
		if err := json.Unmarshal(sc.Bytes(), &e); err != nil {
			t.Fatalf("bad JSONL line %q: %v", sc.Text(), err)
		}
		out[e.Message] = e
	}
	return out
}

func TestSlogHandler(t *testing.T) {
	out := filepath.Join(t.TempDir(), "slog.jsonl")
	// The file backend path is only configurable through TOML.
	log, err := NewFromTOML("backend = \"file\"\nfile_path = '" + out + "'\nwal_enabled = false\nbatch_size = 1\nmin_level = \"TRACE\"\n")
	if err != nil {
		t.Fatalf("NewFromTOML: %v", err)
	}
	defer log.Close()

	h := NewSlogHandler(log, &slog.HandlerOptions{Level: slog.Level(-8)})
	logger := slog.New(h).With("service_area", "checkout")

	logger.Info("plain", "user_id", 7, "ok", true)
	logger.Warn("with group", slog.Group("req", slog.String("id", "abc"), slog.Int("n", 2)))
	logger.WithGroup("db").Error("failed", "err", errors.New("conn reset"), "took", 1500*time.Millisecond)
	logger.Log(context.Background(), LevelFatal, "meltdown")
	logger.Log(context.Background(), slog.Level(-8), "very fine")
	logger.Debug("debug msg")

	if err := log.Flush(); err != nil {
		t.Fatalf("Flush: %v", err)
	}
	log.Shutdown()

	got := readEntries(t, out)
	if e := got["plain"]; e.Level != "INFO" || e.Metadata["user_id"] != float64(7) || e.Metadata["ok"] != true || e.Metadata["service_area"] != "checkout" {
		t.Fatalf("plain: %+v", e)
	}
	if e := got["with group"]; e.Level != "WARN" {
		t.Fatalf("with group level: %+v", e)
	} else if req, _ := e.Metadata["req"].(map[string]any); req["id"] != "abc" || req["n"] != float64(2) {
		t.Fatalf("group not nested: %+v", e.Metadata)
	}
	if e := got["failed"]; e.Level != "ERROR" {
		t.Fatalf("failed: %+v", e)
	} else if db, _ := e.Metadata["db"].(map[string]any); db["err"] != "conn reset" || db["took"] != "1.5s" {
		t.Fatalf("WithGroup/error/duration mapping: %+v", e.Metadata)
	}
	if got["meltdown"].Level != "FATAL" {
		t.Fatalf("fatal mapping: %+v", got["meltdown"])
	}
	if got["very fine"].Level != "TRACE" {
		t.Fatalf("trace mapping: %+v", got["very fine"])
	}
	if got["debug msg"].Level != "DEBUG" {
		t.Fatalf("debug mapping: %+v", got["debug msg"])
	}
}

func TestSlogHandlerLevelAndDerivedIsolation(t *testing.T) {
	log, err := New(Config{Backend: "none"})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	defer log.Close()

	h := NewSlogHandler(log, nil) // default INFO
	if h.Enabled(context.Background(), slog.LevelDebug) {
		t.Fatal("DEBUG must be disabled by default")
	}
	if !h.Enabled(context.Background(), slog.LevelWarn) {
		t.Fatal("WARN must be enabled")
	}

	a := h.WithAttrs([]slog.Attr{slog.String("k", "a")}).(*SlogHandler)
	b := a.WithAttrs([]slog.Attr{slog.String("k", "b")}).(*SlogHandler)
	if a.attrs["k"] != "a" || b.attrs["k"] != "b" {
		t.Fatalf("derived handlers share state: a=%v b=%v", a.attrs, b.attrs)
	}
	g1 := a.WithGroup("g").WithAttrs([]slog.Attr{slog.Int("x", 1)}).(*SlogHandler)
	g2 := a.WithGroup("g").WithAttrs([]slog.Attr{slog.Int("x", 2)}).(*SlogHandler)
	if g1.attrs["g"].(map[string]any)["x"] != int64(1) || g2.attrs["g"].(map[string]any)["x"] != int64(2) {
		t.Fatalf("group clones alias: %v %v", g1.attrs, g2.attrs)
	}
}
