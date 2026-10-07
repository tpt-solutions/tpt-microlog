package microlog

// Linking the native engine.
//
// The static library `libtpt_log_ffi.a` is found, in order, through:
//
//   - CGO_LDFLAGS (for example -L/path/to/target/release),
//   - ${SRCDIR}/lib/<goos>_<goarch>/ (a checkout that ran the fetcher into it),
//   - the system linker path.
//
// `go run ./cmd/fetchlib` downloads a prebuilt library for the current
// platform from the project's GitHub release. The system libraries the Rust
// runtime needs on each platform follow the library itself.
//
// The C header is vendored in this directory (tpt_microlog.h); a CI check keeps
// it identical to tpt-log-ffi/include/tpt_microlog.h.

/*
#cgo LDFLAGS: -ltpt_log_ffi
#cgo linux LDFLAGS: -lm -ldl -lpthread
#cgo darwin LDFLAGS: -lm -lresolv -framework Security -framework CoreFoundation
#cgo windows LDFLAGS: -lntdll -lws2_32 -lbcrypt -luserenv
#cgo linux,amd64 LDFLAGS: -L${SRCDIR}/lib/linux_amd64
#cgo linux,arm64 LDFLAGS: -L${SRCDIR}/lib/linux_arm64
#cgo darwin,amd64 LDFLAGS: -L${SRCDIR}/lib/darwin_amd64
#cgo darwin,arm64 LDFLAGS: -L${SRCDIR}/lib/darwin_arm64
#cgo windows,amd64 LDFLAGS: -L${SRCDIR}/lib/windows_amd64
*/
import "C"
