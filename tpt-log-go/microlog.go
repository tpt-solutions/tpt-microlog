// Package microlog provides Go bindings for tpt-microlog, the microscopic
// telemetry & log router (https://github.com/tpt-solutions/tpt-microlog).
//
// The binding wraps the C ABI exported by the tpt-log-ffi crate via cgo, so a
// native tpt-microlog library must be available at link time:
//
//	cargo build --release -p tpt-log-ffi
//
// Point cgo at the header and library when they are not on the default
// search paths:
//
//	export CGO_CFLAGS="-I/path/to/tpt-log-ffi/include"
//	export CGO_LDFLAGS="-L/path/to/target/release"
//
// Linux/macOS link against the shared or static archive directly
// (-ltpt_log_ffi). On Windows, build the library with the GNU toolchain
// (cargo build --release -p tpt-log-ffi --target x86_64-pc-windows-gnu) and
// link it statically:
//
//	export CGO_LDFLAGS="-L.../target/x86_64-pc-windows-gnu/release \
//	  -Wl,-Bstatic -ltpt_log_ffi -Wl,-Bdynamic \
//	  -lntdll -lws2_32 -lbcrypt -luserenv"
//
// Example:
//
//	log, err := microlog.New(microlog.Config{
//		Backend: "datadog",
//		ApiKey:  "your-api-key",
//	})
//	if err != nil { ... }
//	log.Info("User logged in", map[string]any{"user_id": 123, "ip": "1.2.3.4"})
//	log.Metric("cpu_usage", 42.5, map[string]string{"host": "server-01"})
//	log.Close()
package microlog

/*
#cgo LDFLAGS: -ltpt_log_ffi
#include <stdlib.h>
#include <string.h>
#include "tpt_microlog.h"
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"unsafe"
)

// Config mirrors the C tpt_log_config struct. Empty strings and zero numbers
// mean "use the library default".
type Config struct {
	Backend       string  // "none", "http", "datadog", "elasticsearch", "cloudwatch", "prometheus", "grpc", "udp", "tcp", "file"
	Endpoint      string  // base URL for HTTP-style backends
	ApiKey        string  // secret token (Datadog DD-API-KEY, ...)
	Service       string  // logical service name
	BatchSize     int     // entries per batch (0 -> default 100)
	FlushInterval float64 // flush interval seconds (0 -> default 1.0)
	MaxRetries    int     // delivery attempts (0 -> default 10)
	WalEnabled    bool    // disk write-ahead log on delivery failure
}

// MicroLog is a handle to a native tpt-microlog engine. Safe for concurrent
// use by multiple goroutines.
type MicroLog struct {
	handle *C.tpt_log_handle
}

// New creates a logger. Callers must eventually call Close (or Shutdown then
// Close) to flush and free the native engine.
func New(cfg Config) (*MicroLog, error) {
	cCfg := C.tpt_log_config{}
	cCfg.backend = cStringOpt(cfg.Backend)
	cCfg.endpoint = cStringOpt(cfg.Endpoint)
	cCfg.api_key = cStringOpt(cfg.ApiKey)
	cCfg.service = cStringOpt(cfg.Service)
	cCfg.batch_size = C.int32_t(cfg.BatchSize)
	cCfg.flush_interval = C.double(cfg.FlushInterval)
	cCfg.max_retries = C.int32_t(cfg.MaxRetries)
	if cfg.WalEnabled {
		cCfg.wal_enabled = 1
	} else {
		cCfg.wal_enabled = -1
	}

	handle := C.tpt_log_create(&cCfg)
	freeOpt(cCfg.backend)
	freeOpt(cCfg.endpoint)
	freeOpt(cCfg.api_key)
	freeOpt(cCfg.service)
	if handle == nil {
		return nil, errors.New(lastError())
	}
	return &MicroLog{handle: handle}, nil
}

func cStringOpt(s string) *C.char {
	if s == "" {
		return nil
	}
	return C.CString(s)
}

func freeOpt(p *C.char) {
	if p != nil {
		C.free(unsafe.Pointer(p))
	}
}

func lastError() string {
	if msg := C.tpt_log_last_error(); msg != nil {
		return C.GoString(msg)
	}
	return "tpt_log_create failed"
}

// encodeMetadata renders a Go map as the C ABI "k=v,k2=v2" string. Values are
// JSON-encoded so numbers and booleans survive as typed values.
func encodeMetadata(md map[string]any) (string, error) {
	if len(md) == 0 {
		return "", nil
	}
	pairs := make([]string, 0, len(md))
	for k, v := range md {
		encoded, err := json.Marshal(v)
		if err != nil {
			return "", fmt.Errorf("metadata %q: %w", k, err)
		}
		pairs = append(pairs, k+"="+string(encoded))
	}
	return strings.Join(pairs, ","), nil
}

func (m *MicroLog) logAt(level C.int32_t, message string, md map[string]any) error {
	if m == nil || m.handle == nil {
		return errors.New("microlog: logger is closed")
	}
	meta, err := encodeMetadata(md)
	if err != nil {
		return err
	}
	cMsg := C.CString(message)
	defer C.free(unsafe.Pointer(cMsg))
	var cMeta *C.char
	if meta != "" {
		cMeta = C.CString(meta)
		defer C.free(unsafe.Pointer(cMeta))
	}
	if code := C.tpt_log_log(m.handle, level, cMsg, cMeta); code != C.TPT_LOG_OK {
		return fmt.Errorf("microlog: log failed (code %d): %s", code, lastError())
	}
	return nil
}

// Trace logs at TRACE level with optional structured metadata.
func (m *MicroLog) Trace(message string, md map[string]any) error {
	return m.logAt(C.TPT_LOG_TRACE, message, md)
}

// Debug logs at DEBUG level with optional structured metadata.
func (m *MicroLog) Debug(message string, md map[string]any) error {
	return m.logAt(C.TPT_LOG_DEBUG, message, md)
}

// Info logs at INFO level with optional structured metadata.
func (m *MicroLog) Info(message string, md map[string]any) error {
	return m.logAt(C.TPT_LOG_INFO, message, md)
}

// Warn logs at WARN level with optional structured metadata.
func (m *MicroLog) Warn(message string, md map[string]any) error {
	return m.logAt(C.TPT_LOG_WARN, message, md)
}

// Error logs at ERROR level with optional structured metadata.
func (m *MicroLog) Error(message string, md map[string]any) error {
	return m.logAt(C.TPT_LOG_ERROR, message, md)
}

// Fatal logs at FATAL level with optional structured metadata.
func (m *MicroLog) Fatal(message string, md map[string]any) error {
	return m.logAt(C.TPT_LOG_FATAL, message, md)
}

// Metric records a numeric gauge/counter with optional tags.
func (m *MicroLog) Metric(name string, value float64, tags map[string]string) error {
	if m == nil || m.handle == nil {
		return errors.New("microlog: logger is closed")
	}
	md := make(map[string]any, len(tags))
	for k, v := range tags {
		md[k] = v
	}
	meta, err := encodeMetadata(md)
	if err != nil {
		return err
	}
	cName := C.CString(name)
	defer C.free(unsafe.Pointer(cName))
	var cMeta *C.char
	if meta != "" {
		cMeta = C.CString(meta)
		defer C.free(unsafe.Pointer(cMeta))
	}
	if code := C.tpt_log_metric(m.handle, cName, C.double(value), cMeta); code != C.TPT_LOG_OK {
		return fmt.Errorf("microlog: metric failed (code %d): %s", code, lastError())
	}
	return nil
}

// Queued reports how many entries are still buffered in the ingestion queue.
func (m *MicroLog) Queued() int {
	if m == nil || m.handle == nil {
		return 0
	}
	return int(C.tpt_log_queued(m.handle))
}

// Shutdown flushes pending batches and drains the write-ahead log. Idempotent.
func (m *MicroLog) Shutdown() error {
	if m == nil || m.handle == nil {
		return errors.New("microlog: logger is closed")
	}
	if code := C.tpt_log_shutdown(m.handle); code != C.TPT_LOG_OK {
		return fmt.Errorf("microlog: shutdown failed (code %d)", code)
	}
	return nil
}

// Version returns the native library version string.
func Version() string {
	return C.GoString(C.tpt_log_version())
}

// Close shuts the engine down (if needed) and frees the native handle. It is
// safe to call more than once.
func (m *MicroLog) Close() error {
	if m == nil || m.handle == nil {
		return nil
	}
	_ = C.tpt_log_shutdown(m.handle)
	C.tpt_log_destroy(m.handle)
	m.handle = nil
	return nil
}
