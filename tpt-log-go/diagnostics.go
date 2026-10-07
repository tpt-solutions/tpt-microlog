package microlog

/*
#include <stdlib.h>
#include "tpt_microlog.h"

extern void goErrorTrampoline(char* kind, char* message, void* user);

static void install_error_callback(void) {
	tpt_log_set_error_callback((tpt_log_error_callback)goErrorTrampoline, NULL);
}

static void clear_error_callback(void) {
	tpt_log_set_error_callback(NULL, NULL);
}
*/
import "C"

import (
	"errors"
	"runtime"
	"sync/atomic"
	"unsafe"
)

// ErrorHandler receives internal failures. kind is one of "delivery", "wal",
// "worker" or "config". It is called from background native threads and must
// be safe for concurrent use; panics it raises are the caller's problem, so
// keep it small and non-blocking.
type ErrorHandler func(kind, message string)

var errorHandler atomic.Pointer[ErrorHandler]

//export goErrorTrampoline
func goErrorTrampoline(kind *C.char, message *C.char, _ unsafe.Pointer) {
	if h := errorHandler.Load(); h != nil {
		(*h)(C.GoString(kind), C.GoString(message))
	}
}

// SetErrorHandler installs a process-wide handler for internal failures
// (delivery errors, WAL problems, worker panics, config warnings). Without
// one they are written to stderr. Pass nil to restore the default.
func SetErrorHandler(h ErrorHandler) {
	if h == nil {
		C.clear_error_callback()
		errorHandler.Store(nil)
		return
	}
	errorHandler.Store(&h)
	C.install_error_callback()
}

// NewFromTOML creates a logger from a TOML document using the same keys as
// tpt-log.toml, including redact_keys, redact_patterns and redact_builtin.
func NewFromTOML(toml string) (*MicroLog, error) {
	if toml == "" {
		return nil, errors.New("microlog: empty TOML config")
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cs := C.CString(toml)
	defer C.free(unsafe.Pointer(cs))
	handle := C.tpt_log_create_from_toml(cs)
	if handle == nil {
		return nil, errors.New(lastError())
	}
	return &MicroLog{handle: handle}, nil
}
