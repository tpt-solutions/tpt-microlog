//! `tpt-log-ffi` — C ABI boundary for `tpt-microlog` (Phase 7).
//!
//! Exposes opaque handles and error codes so C, C++, Go, and other FFI
//! consumers can drive the core logging engine. See `include/tpt_microlog.h`
//! for the C contract and `examples/go` for a cgo wrapper.
//!
//! Metadata strings use `name=value` pairs separated by commas
//! (`"user_id=123,ip=1.2.3.4"`). Values are parsed as JSON when possible
//! (`123` stays a number, `true` a bool) and kept as strings otherwise.
//! Strings must be valid UTF-8 and, unless documented otherwise, borrowed for
//! the duration of the call only.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, CStr, CString};
use std::ptr;
use std::sync::Arc;

use serde_json::Value;
use tpt_log_core::{Config, LogLevel, MicroLog as CoreLog};
use tpt_log_transport::build_transport;

/// Error codes returned by every fallible entry point (`TPT_LOG_OK` == 0).
/// `CONFIG_ERROR` is part of the public ABI contract (see the C header) even
/// though configuration failures currently surface via `tpt_log_last_error`.
#[allow(dead_code)]
mod codes {
    pub const OK: i32 = 0;
    pub const NULL_ARGUMENT: i32 = 1;
    pub const INVALID_UTF8: i32 = 2;
    pub const CONFIG_ERROR: i32 = 3;
    pub const INVALID_METADATA: i32 = 4;
}

/// Opaque logger handle (spec §6.5 `tpt_log_handle`).
pub struct MicroLogHandle {
    inner: CoreLog,
}

/// Convert a borrowed C string; `None` maps to an error code.
fn str_from_ptr(ptr: *const c_char) -> Option<Result<&'static str, i32>> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: caller guarantees `ptr` points to a NUL-terminated C string for
    // the duration of the call.
    let cstr = unsafe { CStr::from_ptr(ptr) };
    Some(cstr.to_str().map_err(|_| codes::INVALID_UTF8))
}

/// Empty/NULL strings collapse to `None` (use the built-in default).
fn opt_str(ptr: *const c_char) -> Result<Option<String>, i32> {
    match str_from_ptr(ptr) {
        None => Ok(None),
        Some(Err(code)) => Err(code),
        Some(Ok("")) => Ok(None),
        Some(Ok(s)) => Ok(Some(s.to_string())),
    }
}

/// Parse `"k=v,k2=v2"` metadata into a JSON map. Values decode as JSON when
/// they parse, otherwise they are kept as strings. Commas inside quoted
/// values (e.g. `msg="a,b"`) do not split pairs.
fn parse_metadata(metadata: *const c_char) -> Result<serde_json::Map<String, Value>, i32> {
    let mut map = serde_json::Map::new();
    let raw = match str_from_ptr(metadata) {
        None => return Ok(map),
        Some(Err(code)) => return Err(code),
        Some(Ok(s)) => s,
    };
    for pair in split_pairs(raw) {
        let Some((name, value)) = pair.split_once('=') else {
            return Err(codes::INVALID_METADATA);
        };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() {
            return Err(codes::INVALID_METADATA);
        }
        let parsed = serde_json::from_str::<Value>(value)
            .unwrap_or_else(|_| Value::String(value.trim_matches('"').to_string()));
        map.insert(name.to_string(), parsed);
    }
    Ok(map)
}

/// Split on commas that sit outside double-quoted spans.
fn split_pairs(raw: &str) -> impl Iterator<Item = &str> {
    let mut in_quotes = false;
    let mut escaped = false;
    let mut start = 0;
    let mut moves = Vec::new();
    for (i, ch) in raw.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                moves.push(&raw[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    moves.push(&raw[start..]);
    moves.into_iter()
}

fn level_from_code(level: i32) -> Option<LogLevel> {
    match level {
        0 => Some(LogLevel::Trace),
        1 => Some(LogLevel::Debug),
        2 => Some(LogLevel::Info),
        3 => Some(LogLevel::Warn),
        4 => Some(LogLevel::Error),
        5 => Some(LogLevel::Fatal),
        _ => None,
    }
}

/// Build the engine config from the C struct (zeros/NULLs mean "default").
fn config_from(raw: &TptLogConfig) -> Result<Config, i32> {
    let mut config = Config::load();
    if let Some(backend) = opt_str(raw.backend)? {
        config.backend = backend;
    }
    config.endpoint = opt_str(raw.endpoint)?;
    config.api_key = opt_str(raw.api_key)?;
    config.service = opt_str(raw.service)?;
    config.wal_dir = match opt_str(raw.wal_dir)? {
        Some(dir) => dir,
        None => config.wal_dir,
    };
    if raw.batch_size > 0 {
        config.batch_size = raw.batch_size as usize;
    }
    if raw.flush_interval > 0.0 {
        config.flush_interval_ms = (raw.flush_interval * 1000.0).round() as u64;
    }
    if raw.compression_level != 0 {
        config.compression_level = raw.compression_level;
    }
    if raw.max_retries > 0 {
        config.max_retries = raw.max_retries as u32;
    }
    match raw.wal_enabled {
        0 => config.wal_enabled = false,
        1 => config.wal_enabled = true,
        _ => {}
    }
    Ok(config)
}

/// Mirror of `tpt_log_config` in `tpt_microlog.h`. Zero-init every field you
/// do not set; zero means "use the default".
#[repr(C)]
pub struct TptLogConfig {
    pub backend: *const c_char,
    pub endpoint: *const c_char,
    pub api_key: *const c_char,
    pub service: *const c_char,
    /// Entries per batch (0 → default 100).
    pub batch_size: i32,
    /// Flush interval in seconds (0 → default 1.0).
    pub flush_interval: f64,
    /// zstd level (0 → default 3).
    pub compression_level: i32,
    /// Delivery attempts (0 → default 10).
    pub max_retries: i32,
    /// -1 default, 0 off, 1 on.
    pub wal_enabled: i32,
    pub wal_dir: *const c_char,
}

/// Create a logger. Returns NULL on configuration failure; check
/// `tpt_log_last_error()` for the message.
///
/// # Safety
/// `config` must point to a fully initialized `tpt_log_config`, or be NULL
/// (all defaults). All borrowed strings must outlive the call.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_create(config: *const TptLogConfig) -> *mut MicroLogHandle {
    let built = match (!config.is_null())
        .then(|| config.read())
        .map(|raw| config_from(&raw))
    {
        None => Config::load(),
        Some(Ok(cfg)) => cfg,
        Some(Err(code)) => {
            set_last_error(&format!("tpt_log_create failed (code {code})"));
            return ptr::null_mut();
        }
    };
    match build_transport(&built) {
        Ok(transport) => {
            let handle = Box::new(MicroLogHandle {
                inner: CoreLog::new(built, Arc::from(transport)),
            });
            Box::into_raw(handle)
        }
        Err(e) => {
            set_last_error(&format!("tpt_log_create: {e}"));
            ptr::null_mut()
        }
    }
}

/// Shared logging path for the level-specific functions.
unsafe fn log_at(
    handle: *mut MicroLogHandle,
    level: LogLevel,
    message: *const c_char,
    metadata: *const c_char,
) -> i32 {
    if handle.is_null() {
        return codes::NULL_ARGUMENT;
    }
    let Some(Ok(message)) = str_from_ptr(message) else {
        return codes::NULL_ARGUMENT;
    };
    let map = match parse_metadata(metadata) {
        Ok(map) => map,
        Err(code) => return code,
    };
    let handle = &*handle;
    if map.is_empty() {
        match level {
            LogLevel::Trace => handle.inner.trace(message),
            LogLevel::Debug => handle.inner.debug(message),
            LogLevel::Info => handle.inner.info(message),
            LogLevel::Warn => handle.inner.warn(message),
            LogLevel::Error => handle.inner.error(message),
            LogLevel::Fatal => handle.inner.fatal(message),
        }
    } else {
        handle.inner.log_with(level, message, map);
    }
    codes::OK
}

/// # Safety
/// `handle` must be a live handle; strings valid UTF-8 NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_trace(
    handle: *mut MicroLogHandle,
    message: *const c_char,
    metadata: *const c_char,
) -> i32 {
    log_at(handle, LogLevel::Trace, message, metadata)
}

/// # Safety
/// `handle` must be a live handle; strings valid UTF-8 NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_debug(
    handle: *mut MicroLogHandle,
    message: *const c_char,
    metadata: *const c_char,
) -> i32 {
    log_at(handle, LogLevel::Debug, message, metadata)
}

/// # Safety
/// `handle` must be a live handle; strings valid UTF-8 NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_info(
    handle: *mut MicroLogHandle,
    message: *const c_char,
    metadata: *const c_char,
) -> i32 {
    log_at(handle, LogLevel::Info, message, metadata)
}

/// # Safety
/// `handle` must be a live handle; strings valid UTF-8 NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_warn(
    handle: *mut MicroLogHandle,
    message: *const c_char,
    metadata: *const c_char,
) -> i32 {
    log_at(handle, LogLevel::Warn, message, metadata)
}

/// # Safety
/// `handle` must be a live handle; strings valid UTF-8 NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_error(
    handle: *mut MicroLogHandle,
    message: *const c_char,
    metadata: *const c_char,
) -> i32 {
    log_at(handle, LogLevel::Error, message, metadata)
}

/// # Safety
/// `handle` must be a live handle; strings valid UTF-8 NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_fatal(
    handle: *mut MicroLogHandle,
    message: *const c_char,
    metadata: *const c_char,
) -> i32 {
    log_at(handle, LogLevel::Fatal, message, metadata)
}

/// Log with an explicit level code (see `tpt_log_level` in the header).
///
/// # Safety
/// Same contract as `tpt_log_info`.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_log(
    handle: *mut MicroLogHandle,
    level: i32,
    message: *const c_char,
    metadata: *const c_char,
) -> i32 {
    match level_from_code(level) {
        Some(level) => log_at(handle, level, message, metadata),
        None => codes::NULL_ARGUMENT,
    }
}

/// Record a numeric metric (spec §6.5).
///
/// # Safety
/// `handle` must be a live handle; `name`/`tags` valid UTF-8 NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_metric(
    handle: *mut MicroLogHandle,
    name: *const c_char,
    value: f64,
    tags: *const c_char,
) -> i32 {
    if handle.is_null() {
        return codes::NULL_ARGUMENT;
    }
    let Some(Ok(name)) = str_from_ptr(name) else {
        return codes::NULL_ARGUMENT;
    };
    let tag_map = match parse_metadata(tags) {
        Ok(map) => map,
        Err(code) => return code,
    };
    let tags = if tag_map.is_empty() {
        None
    } else {
        Some(&tag_map)
    };
    (*handle).inner.metric(name, value, tags);
    codes::OK
}

/// Flush pending batches and drain the WAL. Idempotent.
///
/// # Safety
/// `handle` must be a live handle or NULL (NULL is a no-op).
#[no_mangle]
pub unsafe extern "C" fn tpt_log_shutdown(handle: *mut MicroLogHandle) -> i32 {
    if handle.is_null() {
        return codes::NULL_ARGUMENT;
    }
    (*handle).inner.shutdown();
    codes::OK
}

/// Destroy a handle created by `tpt_log_create` (flushes implicitly).
/// NULL is a no-op; the pointer must not be used afterwards.
///
/// # Safety
/// `handle` must have been returned by `tpt_log_create` and not destroyed
/// before.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_destroy(handle: *mut MicroLogHandle) {
    if !handle.is_null() {
        drop(Box::from_raw(handle));
    }
}

/// Entries still buffered in the ingestion queue.
///
/// # Safety
/// `handle` must be a live handle or NULL (NULL returns 0).
#[no_mangle]
pub unsafe extern "C" fn tpt_log_queued(handle: *mut MicroLogHandle) -> usize {
    if handle.is_null() {
        0
    } else {
        (*handle).inner.queued()
    }
}

/// Library version string (static, valid for the program lifetime).
#[no_mangle]
pub extern "C" fn tpt_log_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

thread_local! {
    static LAST_ERROR: std::cell::RefCell<Option<CString>> = const { std::cell::RefCell::new(None) };
}

fn set_last_error(message: &str) {
    LAST_ERROR.with(|slot| {
        *slot.borrow_mut() = CString::new(message).ok();
    });
}

/// Message for the most recent failure on this thread, or NULL.
///
/// The returned pointer stays valid until the next failing call on the same
/// thread.
#[no_mangle]
pub extern "C" fn tpt_log_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(ptr::null())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_logger() -> (*mut MicroLogHandle, tempdir::TempDirGuard) {
        let dir = tempdir::create();
        let wal_dir = CString::new(dir.path.to_string_lossy().into_owned()).unwrap();
        let cfg = TptLogConfig {
            backend: ptr::null(),
            endpoint: ptr::null(),
            api_key: ptr::null(),
            service: ptr::null(),
            batch_size: 1,
            flush_interval: 0.05,
            compression_level: 0,
            max_retries: 1,
            wal_enabled: 0,
            wal_dir: wal_dir.as_ptr(),
        };
        let handle = unsafe { tpt_log_create(&cfg) };
        assert!(!handle.is_null());
        (handle, dir)
    }

    mod tempdir {
        use std::path::PathBuf;
        pub struct TempDirGuard {
            pub path: PathBuf,
        }
        impl Drop for TempDirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }
        pub fn create() -> TempDirGuard {
            let path = std::env::temp_dir().join(format!("tpt-ffi-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            TempDirGuard { path }
        }
    }

    #[test]
    fn round_trip_api() {
        let (handle, _guard) = make_logger();
        let msg = c"engine started";
        let md = c"user_id=123,ip=1.2.3.4,active=true";
        assert_eq!(
            unsafe { tpt_log_info(handle, msg.as_ptr(), md.as_ptr()) },
            0
        );
        let name = c"cpu_usage";
        let tags = c"host=server-01";
        assert_eq!(
            unsafe { tpt_log_metric(handle, name.as_ptr(), 42.5, tags.as_ptr()) },
            0
        );
        assert_eq!(unsafe { tpt_log_shutdown(handle) }, 0);
        unsafe { tpt_log_destroy(handle) };
    }

    #[test]
    fn null_and_malformed_inputs() {
        let msg = c"x";
        assert_eq!(
            unsafe { tpt_log_info(ptr::null_mut(), msg.as_ptr(), ptr::null()) },
            codes::NULL_ARGUMENT
        );
        let (handle, _guard) = make_logger();
        let bad = c"no-equals-sign";
        assert_eq!(
            unsafe { tpt_log_info(handle, msg.as_ptr(), bad.as_ptr()) },
            codes::INVALID_METADATA
        );
        assert_eq!(
            unsafe { tpt_log_log(handle, 99, msg.as_ptr(), ptr::null()) },
            codes::NULL_ARGUMENT
        );
        unsafe { tpt_log_destroy(handle) };
    }

    #[test]
    fn version_string_is_valid() {
        let ptr = tpt_log_version();
        let s = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap();
        assert!(s.split('.').count() == 3, "version: {s}");
    }
}
