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
use tpt_log_core::{Config, LogLevel, MicroLog as CoreLog, Secret};
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
    /// A panic escaped the engine; the process is still usable.
    pub const PANIC: i32 = 5;
    /// Stats buffer too small (returned as a negative by `tpt_log_stats_json`).
    pub const STATS_TOO_SMALL: i32 = -2;
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

/// Split on commas that sit outside double-quoted spans and outside nested
/// JSON objects/arrays (`req={"id":"a","n":2}` is a single pair).
fn split_pairs(raw: &str) -> impl Iterator<Item = &str> {
    let mut in_quotes = false;
    let mut escaped = false;
    let mut depth = 0usize;
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
            '{' | '[' if !in_quotes => depth += 1,
            '}' | ']' if !in_quotes => depth = depth.saturating_sub(1),
            ',' if !in_quotes && depth == 0 => {
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
    config.api_key = opt_str(raw.api_key)?.map(Secret::new);
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
    // Tri-state, zero-init friendly: 0 (the zero-initialized value) keeps the
    // default, positive forces on, negative forces off.
    match raw.wal_enabled {
        n if n < 0 => config.wal_enabled = false,
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
    /// 0 default, 1 on, negative off.
    pub wal_enabled: i32,
    pub wal_dir: *const c_char,
}

/// Panic barrier: a bug inside the engine must never unwind across the C ABI.
/// Panics map to [`codes::PANIC`] with a message on `tpt_log_last_error`.
fn guarded(body: impl FnOnce() -> i32) -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(code) => code,
        Err(_) => {
            set_last_error("tpt-microlog: internal panic inside the logging engine");
            codes::PANIC
        }
    }
}

/// Panic barrier for constructors: panics map to a NULL handle.
fn guarded_ptr<T>(body: impl FnOnce() -> *mut T) -> *mut T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(ptr) => ptr,
        Err(_) => {
            set_last_error("tpt-microlog: internal panic inside the logging engine");
            ptr::null_mut()
        }
    }
}

/// Create a logger. Returns NULL on configuration failure; check
/// `tpt_log_last_error()` for the message.
///
/// # Safety
/// `config` must point to a fully initialized `tpt_log_config`, or be NULL
/// (all defaults). All borrowed strings must outlive the call.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_create(config: *const TptLogConfig) -> *mut MicroLogHandle {
    guarded_ptr(|| {
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
    })
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
    guarded(|| log_at(handle, LogLevel::Trace, message, metadata))
}

/// # Safety
/// `handle` must be a live handle; strings valid UTF-8 NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_debug(
    handle: *mut MicroLogHandle,
    message: *const c_char,
    metadata: *const c_char,
) -> i32 {
    guarded(|| log_at(handle, LogLevel::Debug, message, metadata))
}

/// # Safety
/// `handle` must be a live handle; strings valid UTF-8 NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_info(
    handle: *mut MicroLogHandle,
    message: *const c_char,
    metadata: *const c_char,
) -> i32 {
    guarded(|| log_at(handle, LogLevel::Info, message, metadata))
}

/// # Safety
/// `handle` must be a live handle; strings valid UTF-8 NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_warn(
    handle: *mut MicroLogHandle,
    message: *const c_char,
    metadata: *const c_char,
) -> i32 {
    guarded(|| log_at(handle, LogLevel::Warn, message, metadata))
}

/// # Safety
/// `handle` must be a live handle; strings valid UTF-8 NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_error(
    handle: *mut MicroLogHandle,
    message: *const c_char,
    metadata: *const c_char,
) -> i32 {
    guarded(|| log_at(handle, LogLevel::Error, message, metadata))
}

/// # Safety
/// `handle` must be a live handle; strings valid UTF-8 NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_fatal(
    handle: *mut MicroLogHandle,
    message: *const c_char,
    metadata: *const c_char,
) -> i32 {
    guarded(|| log_at(handle, LogLevel::Fatal, message, metadata))
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
    guarded(|| match level_from_code(level) {
        Some(level) => log_at(handle, level, message, metadata),
        None => codes::NULL_ARGUMENT,
    })
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
    guarded(|| {
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
    })
}

/// Flush pending batches and drain the WAL. Idempotent.
///
/// # Safety
/// `handle` must be a live handle or NULL (NULL is a no-op).
#[no_mangle]
pub unsafe extern "C" fn tpt_log_shutdown(handle: *mut MicroLogHandle) -> i32 {
    guarded(|| {
        if handle.is_null() {
            return codes::NULL_ARGUMENT;
        }
        (*handle).inner.shutdown();
        codes::OK
    })
}

/// Destroy a handle created by `tpt_log_create` (flushes implicitly).
/// NULL is a no-op; the pointer must not be used afterwards.
///
/// # Safety
/// `handle` must have been returned by `tpt_log_create` and not destroyed
/// before.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_destroy(handle: *mut MicroLogHandle) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if !handle.is_null() {
            drop(Box::from_raw(handle));
        }
    }));
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

/// Log with an explicit level code and metadata given as a JSON object string
/// (`{"user":{"id":42}}`), supporting nested structures that the flat
/// `k=v,k2=v2` syntax cannot express.
///
/// # Safety
/// Same contract as `tpt_log_log`; `json_metadata` must be a valid UTF-8 JSON
/// object or NULL.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_log_json(
    handle: *mut MicroLogHandle,
    level: i32,
    message: *const c_char,
    json_metadata: *const c_char,
) -> i32 {
    guarded(|| {
        let Some(level) = level_from_code(level) else {
            return codes::NULL_ARGUMENT;
        };
        if handle.is_null() {
            return codes::NULL_ARGUMENT;
        }
        let Some(Ok(message)) = str_from_ptr(message) else {
            return codes::NULL_ARGUMENT;
        };
        let metadata: serde_json::Map<String, Value> = match str_from_ptr(json_metadata) {
            None => serde_json::Map::new(),
            Some(Err(code)) => return code,
            Some(Ok("")) => serde_json::Map::new(),
            Some(Ok(raw)) => match serde_json::from_str::<Value>(raw) {
                Ok(Value::Object(map)) => map,
                _ => return codes::INVALID_METADATA,
            },
        };
        if metadata.is_empty() {
            match level {
                LogLevel::Trace => (*handle).inner.trace(message),
                LogLevel::Debug => (*handle).inner.debug(message),
                LogLevel::Info => (*handle).inner.info(message),
                LogLevel::Warn => (*handle).inner.warn(message),
                LogLevel::Error => (*handle).inner.error(message),
                LogLevel::Fatal => (*handle).inner.fatal(message),
            }
        } else {
            (*handle).inner.log_with(level, message, metadata);
        }
        codes::OK
    })
}

/// Flush buffered entries now (bounded by the configured shutdown timeout).
///
/// # Safety
/// `handle` must be a live handle or NULL (NULL is a no-op).
#[no_mangle]
pub unsafe extern "C" fn tpt_log_flush(handle: *mut MicroLogHandle) -> i32 {
    guarded(|| {
        if handle.is_null() {
            return codes::NULL_ARGUMENT;
        }
        (*handle).inner.flush();
        codes::OK
    })
}

/// Delivery statistics as a JSON object written into `buf` (capacity `cap`,
/// including the trailing NUL). Returns the number of bytes written on
/// success, or a negative error code: `TPT_LOG_NULL_ARGUMENT` / `-2` when the
/// buffer is too small.
///
/// # Safety
/// `handle` must be a live handle or NULL (NULL reports zeros); `buf` must be
/// writable for `cap` bytes.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_stats_json(
    handle: *mut MicroLogHandle,
    buf: *mut c_char,
    cap: usize,
) -> i32 {
    guarded(|| {
        if buf.is_null() {
            return codes::NULL_ARGUMENT;
        }
        let relaxed = std::sync::atomic::Ordering::Relaxed;
        let (stats, queued, inflight) = if handle.is_null() {
            (None, 0, 0)
        } else {
            // SAFETY: caller contract (live handle or NULL).
            let inner = unsafe { &(*handle).inner };
            (
                Some(inner.stats() as &tpt_log_core::microlog::Stats),
                inner.queued(),
                inner.inflight(),
            )
        };
        macro_rules! field {
            ($c:ident) => {
                stats.map(|s| s.$c.load(relaxed)).unwrap_or(0)
            };
        }
        let json = format!(
            "{{\"delivered_batches\":{},\"delivered_entries\":{},\"wal_writes\":{},\
             \"retried_entries\":{},\"dropped_batches\":{},\"dropped_entries\":{},\
             \"queued\":{},\"inflight\":{}}}",
            field!(delivered_batches),
            field!(delivered_entries),
            field!(wal_writes),
            field!(retried_entries),
            field!(dropped_batches),
            field!(dropped_entries),
            queued,
            inflight,
        );
        let needed = json.len() + 1;
        if cap < needed {
            return codes::STATS_TOO_SMALL;
        }
        let Ok(bytes) = std::ffi::CString::new(json) else {
            return codes::PANIC;
        };
        // SAFETY: caller guarantees `cap >= needed` writable bytes at `buf`.
        ptr::copy_nonoverlapping(bytes.as_ptr(), buf.cast(), needed);
        needed as i32
    })
}

/// C callback for internal diagnostics: `(kind, message, user_data)`. Both
/// strings are valid only for the duration of the call.
pub type TptLogErrorCallback = Option<
    unsafe extern "C" fn(kind: *const c_char, message: *const c_char, user: *mut std::ffi::c_void),
>;

struct UserData(*mut std::ffi::c_void);
// SAFETY: the C caller promises `user` may be used from any thread.
unsafe impl Send for UserData {}
unsafe impl Sync for UserData {}

/// Install (or clear, with a NULL callback) the process-wide diagnostics
/// callback: delivery failures, WAL problems, worker panics and config
/// warnings are reported here instead of stderr. The callback may be invoked
/// from background threads.
///
/// # Safety
/// `callback` must be thread-safe and `user` must stay valid until the
/// callback is cleared.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_set_error_callback(
    callback: TptLogErrorCallback,
    user: *mut std::ffi::c_void,
) {
    match callback {
        None => tpt_log_core::set_on_error(None),
        Some(cb) => {
            let user = UserData(user);
            tpt_log_core::set_on_error(Some(Arc::new(move |kind: &str, msg: &str| {
                let (Ok(k), Ok(m)) = (CString::new(kind), CString::new(msg)) else {
                    return;
                };
                let user = &user;
                // SAFETY: contract documented on `tpt_log_set_error_callback`.
                unsafe { cb(k.as_ptr(), m.as_ptr(), user.0) };
            })));
        }
    }
}

/// Create a logger from a TOML document (the same keys as `tpt-log.toml`,
/// including `redact_keys` / `redact_patterns` / `redact_builtin`). Returns
/// NULL on parse or transport errors; see `tpt_log_last_error()`.
///
/// # Safety
/// `toml` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn tpt_log_create_from_toml(toml: *const c_char) -> *mut MicroLogHandle {
    guarded_ptr(|| {
        let raw = match str_from_ptr(toml) {
            Some(Ok(s)) => s,
            _ => {
                set_last_error("tpt_log_create_from_toml: NULL or non-UTF-8 argument");
                return ptr::null_mut();
            }
        };
        let mut built = match Config::from_toml_str(raw) {
            Ok(c) => c,
            Err(e) => {
                set_last_error(&format!("tpt_log_create_from_toml: {e}"));
                return ptr::null_mut();
            }
        };
        built.apply_env();
        match build_transport(&built) {
            Ok(transport) => Box::into_raw(Box::new(MicroLogHandle {
                inner: CoreLog::new(built, Arc::from(transport)),
            })),
            Err(e) => {
                set_last_error(&format!("tpt_log_create_from_toml: {e}"));
                ptr::null_mut()
            }
        }
    })
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

    #[test]
    fn nested_json_values_are_not_split_at_inner_commas() {
        let raw = CString::new(r#"a=1,req={"id":"x","n":2,"l":[1,2]},msg="p,q",b=true"#).unwrap();
        let map = parse_metadata(raw.as_ptr()).unwrap();
        assert_eq!(map.len(), 4, "{map:?}");
        assert_eq!(map["req"]["l"][1], 2);
        assert_eq!(map["msg"], "p,q");
        assert_eq!(map["b"], true);
    }

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
            wal_enabled: -1,
            wal_dir: wal_dir.as_ptr(),
        };
        let handle = unsafe { tpt_log_create(&cfg) };
        assert!(!handle.is_null());
        (handle, dir)
    }

    mod tempdir {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU32, Ordering};
        pub struct TempDirGuard {
            pub path: PathBuf,
        }
        impl Drop for TempDirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }
        /// Unique per call: tests run in parallel and the WAL takes an advisory
        /// lock per directory, so a shared name would fail the second logger.
        pub fn create() -> TempDirGuard {
            static SEQ: AtomicU32 = AtomicU32::new(0);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "tpt-ffi-test-{}-{nanos}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
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

    #[test]
    fn zero_init_config_means_defaults() {
        // Zero-initialized config must be accepted and keep the WAL default
        // (on): the old ABI treated wal_enabled == 0 as "off".
        let handle = unsafe { tpt_log_create(ptr::null()) };
        assert!(!handle.is_null());
        unsafe { tpt_log_destroy(handle) };
    }

    #[test]
    fn log_json_and_stats_and_flush() {
        let (handle, _guard) = make_logger();
        let msg = c"nested";
        let md = c"{\"user\":{\"id\":42},\"tags\":[\"a\",\"b\"]}";
        assert_eq!(
            unsafe { tpt_log_log_json(handle, 2, msg.as_ptr(), md.as_ptr()) },
            0
        );
        // Malformed JSON is rejected.
        let bad = c"{not json}";
        assert_eq!(
            unsafe { tpt_log_log_json(handle, 2, msg.as_ptr(), bad.as_ptr()) },
            codes::INVALID_METADATA
        );
        assert_eq!(unsafe { tpt_log_flush(handle) }, 0);

        let mut buf = vec![0u8; 512];
        let written = unsafe { tpt_log_stats_json(handle, buf.as_mut_ptr().cast(), buf.len()) };
        assert!(written > 0, "stats must be written");
        let text = unsafe { CStr::from_ptr(buf.as_ptr().cast()) }
            .to_str()
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["delivered_batches"], 1, "flush must deliver");
        assert_eq!(parsed["queued"], 0);

        // Too-small buffer reports the dedicated error code.
        let mut tiny = [0u8; 4];
        assert_eq!(
            unsafe { tpt_log_stats_json(handle, tiny.as_mut_ptr().cast(), tiny.len()) },
            codes::STATS_TOO_SMALL
        );
        unsafe { tpt_log_destroy(handle) };
    }
}
