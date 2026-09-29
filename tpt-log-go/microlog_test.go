package microlog

import "testing"

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
