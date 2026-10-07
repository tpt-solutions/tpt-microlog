//! `tpt-log-wasm` — browser/edge bindings for `tpt-microlog` (Phase 6).
//!
//! Self-contained by design: the Rust core relies on OS threads, a disk WAL,
//! and a blocking HTTP stack, none of which exist in a browser. This crate
//! reimplements the ingestion/batching/retry loop on web primitives:
//!
//! - entries buffer in memory and flush on `batchSize` / `flushInterval`;
//! - delivery uses the browser `fetch` API (or global `fetch` under Node 18+);
//! - failed batches return to the buffer and retry on the next tick
//!   (in-memory retry only — there is no disk WAL on the web, spec §9);
//! - `navigator.sendBeacon()` attempts a last-ditch delivery on page hide.
//!
//! Payloads are plain JSON (no zstd in WASM builds) and mirror the native
//! wire format: `{"entries": [{timestamp, level, type, message, metadata}]}`.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Function, Object, JSON};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

/// Severity levels, ordered exactly like the native `LogLevel` enum.
const LEVELS: [&str; 6] = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL"];

fn level_rank(level: &str) -> Option<usize> {
    LEVELS.iter().position(|l| l.eq_ignore_ascii_case(level))
}

/// One log entry, serialized in the native wire format.
#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    timestamp: u64,
    level: String,
    #[serde(rename = "type")]
    log_type: String,
    message: String,
    metadata: serde_json::Map<String, serde_json::Value>,
    /// In-memory retry counter (not serialized): batches that keep failing
    /// with transient errors are dropped after `max_retries` attempts.
    #[serde(default, skip_serializing)]
    retries: u32,
}

/// Mask text for redacted values (same as the native core).
const MASK: &str = "[REDACTED]";

/// Built-in detectors for `redactBuiltin: true` (kept in sync with
/// `tpt-log-core/src/redact.rs`; this crate cannot depend on the native core).
const BUILTIN_PATTERNS: &[&str] = &[
    r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
    r"(?i)\bbearer\s+[A-Za-z0-9\-._~+/]+=*",
    r"\beyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\b",
    r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b",
    r"\b(?:\d[ -]?){13,19}\b",
    r"\b\d{3}-\d{2}-\d{4}\b",
];

/// PII / secret redaction applied to every entry before it is buffered.
#[derive(Default)]
struct Redactor {
    keys: Vec<String>,
    patterns: Vec<regex::Regex>,
}

impl Redactor {
    fn scrub_str(&self, s: &str) -> Option<String> {
        let mut out: Option<String> = None;
        for re in &self.patterns {
            let cur = out.as_deref().unwrap_or(s);
            if re.is_match(cur) {
                out = Some(re.replace_all(cur, MASK).into_owned());
            }
        }
        out
    }

    fn scrub_value(&self, v: &mut serde_json::Value) {
        match v {
            serde_json::Value::String(s) => {
                if let Some(new) = self.scrub_str(s) {
                    *s = new;
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| self.scrub_value(x)),
            serde_json::Value::Object(m) => self.scrub_map(m),
            _ => {}
        }
    }

    fn scrub_map(&self, m: &mut serde_json::Map<String, serde_json::Value>) {
        for (k, v) in m.iter_mut() {
            if self.keys.contains(&k.to_ascii_lowercase()) {
                *v = serde_json::Value::String(MASK.into());
            } else {
                self.scrub_value(v);
            }
        }
    }

    fn apply(&self, entry: &mut Entry) {
        if self.keys.is_empty() && self.patterns.is_empty() {
            return;
        }
        if let Some(new) = self.scrub_str(&entry.message) {
            entry.message = new;
        }
        self.scrub_map(&mut entry.metadata);
    }
}

/// Read an options array of strings (`redactKeys: ["password"]`).
fn option_string_list(options: &JsValue, key: &str) -> Vec<String> {
    js_sys::Reflect::get(options, &key.into())
        .ok()
        .filter(Array::is_array)
        .map(|v| {
            Array::from(&v)
                .iter()
                .filter_map(|x| x.as_string())
                .collect()
        })
        .unwrap_or_default()
}

fn option_bool(options: &JsValue, key: &str) -> bool {
    js_sys::Reflect::get(options, &key.into())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// Shared mutable state (single-threaded wasm: `RefCell` is enough).
struct Inner {
    buffer: RefCell<Vec<Entry>>,
    endpoint: Option<String>,
    headers: Vec<(String, String)>,
    service: Option<String>,
    min_level: usize,
    sample_rate: f64,
    batch_size: usize,
    flush_interval_ms: f64,
    max_retries_kept: usize,
    /// Attempts a batch may stay buffered during outages before it is dropped.
    max_retries: u32,
    /// Emit Datadog Logs-API-v2 event arrays instead of `{"entries": [...]}`.
    datadog: bool,
    redactor: Redactor,
    /// `onError(kind, message)` callback; falls back to `console.warn`.
    on_error: Option<Function>,
    timer: RefCell<Option<Closure<dyn FnMut()>>>,
    /// Id of the currently pending flush timeout, if any.
    timer_id: RefCell<Option<JsValue>>,
    unload: RefCell<Option<Closure<dyn FnMut()>>>,
    stopped: RefCell<bool>,
    /// Entries confirmed delivered (2xx) — mirrors `Stats.delivered_entries`.
    delivered_entries: std::cell::Cell<u32>,
    /// Entries dropped because the retry buffer overflowed.
    dropped_entries: std::cell::Cell<u32>,
}

impl Inner {
    /// Cancel the pending timeout, then drop its closure.
    fn stop_timer(&self) {
        if let Some(id) = self.timer_id.borrow_mut().take() {
            cancel_timeout(&id);
        }
        *self.timer.borrow_mut() = None;
    }

    /// Report an internal failure (never silent): user callback if given,
    /// otherwise `console.warn`. A throwing callback is ignored.
    fn report(&self, kind: &str, message: &str) {
        if let Some(cb) = &self.on_error {
            let _ = cb.call2(&JsValue::NULL, &kind.into(), &message.into());
            return;
        }
        if let Ok(console) = js_sys::Reflect::get(&js_sys::global(), &"console".into()) {
            if let Ok(warn) = js_sys::Reflect::get(&console, &"warn".into()) {
                if let Some(warn) = warn.dyn_ref::<Function>() {
                    let _ = warn.call1(
                        &console,
                        &format!("tpt-microlog: [{kind}] {message}").into(),
                    );
                }
            }
        }
    }

    /// Accept an entry unless it is below `min_level` or sampled away.
    fn accept(&self, level: &str) -> bool {
        let Some(rank) = level_rank(level) else {
            return true;
        };
        if rank < self.min_level {
            return false;
        }
        if self.sample_rate < 1.0 && js_sys::Math::random() > self.sample_rate {
            return false;
        }
        true
    }

    /// Append an entry; returns `true` when the buffer reached `batch_size`
    /// and the caller should spawn an async flush.
    fn push(&self, entry: Entry) -> bool {
        let mut buffer = self.buffer.borrow_mut();
        buffer.push(entry);
        buffer.len() >= self.batch_size
    }

    /// Take up to one batch of pending entries.
    fn take_batch(&self) -> Vec<Entry> {
        let mut buffer = self.buffer.borrow_mut();
        if buffer.len() > self.batch_size {
            buffer.drain(..self.batch_size).collect()
        } else {
            std::mem::take(&mut *buffer)
        }
    }

    /// Encode a batch in the wire format of the configured backend:
    /// `{"entries": [...]}` by default, or a Datadog Logs API v2 event array
    /// (`[{message, status, timestamp, ddsource, ...attributes}]`).
    fn encode_batch(&self, batch: &[Entry]) -> Vec<u8> {
        if self.datadog {
            let events: Vec<serde_json::Value> = batch.iter().map(dd_event).collect();
            serde_json::to_vec(&events).unwrap_or_default()
        } else {
            serde_json::to_vec(&serde_json::json!({ "entries": batch })).unwrap_or_default()
        }
    }

    /// Put a failed batch back, bounded by `max_retries_kept` (drop oldest).
    fn requeue(&self, batch: Vec<Entry>) {
        let mut buffer = self.buffer.borrow_mut();
        let mut combined = batch;
        combined.extend(buffer.drain(..));
        let overflow = combined.len().saturating_sub(self.max_retries_kept);
        combined.drain(..overflow);
        self.dropped_entries
            .set(self.dropped_entries.get() + overflow as u32);
        *buffer = combined;
    }

    /// Serialize every pending entry (used by the sendBeacon unload hook).
    fn pending_json(&self) -> Option<Vec<u8>> {
        let buffer = self.buffer.borrow();
        if buffer.is_empty() {
            return None;
        }
        serde_json::to_vec(&serde_json::json!({ "entries": &*buffer })).ok()
    }

    /// Take one batch and attempt delivery once.
    ///
    /// - transient failure (network, 5xx, 408/425/429): the batch returns to
    ///   the front of the buffer and retries on a later tick, up to
    ///   `max_retries` attempts (in-memory retry, spec §9);
    /// - permanent failure (other 4xx): the batch is dropped — it can never
    ///   succeed, so re-queueing it would block every batch behind it.
    async fn flush_once(&self) {
        if self.endpoint.is_none() {
            self.buffer.borrow_mut().clear();
            return;
        }
        let mut batch = self.take_batch();
        if batch.is_empty() {
            return;
        }
        let body = self.encode_batch(&batch);
        let endpoint = self.endpoint.as_deref().unwrap_or_default();
        match send_batch(endpoint, &self.headers, &body, false).await {
            SendOutcome::Delivered => {
                self.delivered_entries
                    .set(self.delivered_entries.get() + batch.len() as u32);
            }
            SendOutcome::Permanent => {
                let dropped = batch.len() as u32;
                self.dropped_entries
                    .set(self.dropped_entries.get() + dropped);
                self.report(
                    "delivery",
                    &format!(
                        "dropped {dropped} entries: endpoint rejected the batch permanently (4xx or invalid request)"
                    ),
                );
            }
            SendOutcome::Transient => {
                let before = batch.len();
                let max = self.max_retries;
                batch.retain_mut(|e| {
                    e.retries += 1;
                    e.retries < max
                });
                let expired = (before - batch.len()) as u32;
                self.dropped_entries
                    .set(self.dropped_entries.get() + expired);
                if expired > 0 {
                    self.report(
                        "delivery",
                        &format!("dropped {expired} entries after {max} failed delivery attempts"),
                    );
                }
                self.requeue(batch);
            }
        }
    }
}

/// Map one entry to a Datadog v2 log event: metadata becomes top-level
/// attributes, severity uses Datadog's `status` vocabulary.
fn dd_event(entry: &Entry) -> serde_json::Value {
    let status = match entry.level.to_ascii_uppercase().as_str() {
        "TRACE" | "DEBUG" => "debug",
        "INFO" => "info",
        "WARN" => "warning",
        "ERROR" => "error",
        _ => "critical",
    };
    let mut obj = serde_json::Map::new();
    obj.insert(
        "message".into(),
        serde_json::Value::String(entry.message.clone()),
    );
    obj.insert("status".into(), serde_json::Value::String(status.into()));
    obj.insert("timestamp".into(), serde_json::Value::from(entry.timestamp));
    obj.insert(
        "ddsource".into(),
        serde_json::Value::String("browser".into()),
    );
    for (k, v) in &entry.metadata {
        if matches!(
            k.as_str(),
            "host" | "service" | "source" | "pid" | "value" | "tags"
        ) {
            continue;
        }
        obj.entry(k.clone()).or_insert_with(|| v.clone());
    }
    if let Some(service) = entry.metadata.get("service") {
        obj.insert("service".into(), service.clone());
    }
    if let Some(tags) = entry.metadata.get("tags").filter(|t| t.is_object()) {
        obj.insert("tags".into(), tags.clone());
    }
    serde_json::Value::Object(obj)
}

/// Result of one delivery attempt, classified like the native core.
enum SendOutcome {
    Delivered,
    /// Network error, 5xx, or 408/425/429 — worth retrying later.
    Transient,
    /// Other 4xx — the backend will never accept this batch.
    Permanent,
}

/// POST one batch via `fetch` and classify the response status. With
/// `keepalive`, the request survives page unload (used by the pagehide hook).
async fn send_batch(
    endpoint: &str,
    headers: &[(String, String)],
    body: &[u8],
    keepalive: bool,
) -> SendOutcome {
    let global = js_sys::global();
    let fetch: Function = match js_sys::Reflect::get(&global, &"fetch".into()) {
        Ok(f) if f.is_function() => f.unchecked_into(),
        _ => return SendOutcome::Transient,
    };
    // `fetch` must be called with the global as `this` in browsers.
    let this: JsValue = if global.is_instance_of::<web_sys::Window>() {
        global.clone().into()
    } else {
        JsValue::UNDEFINED
    };

    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    // web-sys does not generate a `keepalive` setter; it is a plain
    // dictionary property, so set it via reflection.
    if keepalive {
        let _ = js_sys::Reflect::set(init.as_ref(), &"keepalive".into(), &JsValue::from(true));
    }
    let bytes = js_sys::Uint8Array::from(body);
    init.set_body(&bytes.into());
    let request = match web_sys::Request::new_with_str_and_init(endpoint, &init) {
        Ok(r) => r,
        Err(_) => return SendOutcome::Permanent,
    };
    let request_headers = request.headers();
    for (name, value) in headers {
        if request_headers.set(name, value).is_err() {
            return SendOutcome::Permanent;
        }
    }

    let Ok(promise) = fetch.call1(&this, &request.into()) else {
        return SendOutcome::Transient;
    };
    let Ok(promise) = promise.dyn_into::<js_sys::Promise>() else {
        return SendOutcome::Transient;
    };
    let response = match wasm_bindgen_futures::JsFuture::from(promise).await {
        Ok(resp) => resp,
        Err(_) => return SendOutcome::Transient,
    };
    let Ok(response) = response.dyn_into::<web_sys::Response>() else {
        return SendOutcome::Transient;
    };
    let status = response.status();
    match status {
        200..=299 => SendOutcome::Delivered,
        0 | 408 | 425 | 429 | 500..=599 => SendOutcome::Transient,
        // Other 4xx: retrying can never succeed.
        _ => SendOutcome::Permanent,
    }
}

/// Fire-and-forget delivery through `navigator.sendBeacon` (page unload).
fn send_beacon(endpoint: &str, body: &[u8]) -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let navigator = window.navigator();
    let bytes = js_sys::Uint8Array::from(body);
    let data: &js_sys::Object = bytes.as_ref();
    navigator
        .send_beacon_with_opt_buffer_source(endpoint, Some(data.unchecked_ref()))
        .unwrap_or(false)
}

/// Arm a callback after `ms` milliseconds (window timer, falling back to the
/// `setTimeout` global for Node-style runtimes).
fn schedule_timeout(callback: &Function, ms: i32) -> Option<JsValue> {
    let global = js_sys::global();
    let set_timeout = js_sys::Reflect::get(&global, &"setTimeout".into()).ok()?;
    set_timeout
        .dyn_ref::<Function>()?
        .call2(&global, callback, &ms.into())
        .ok()
}

/// Cancel a timer returned by [`schedule_timeout`] so a dropped closure is
/// never invoked (and Node can exit instead of waiting on a dead timer).
fn cancel_timeout(id: &JsValue) {
    let global = js_sys::global();
    if let Ok(clear) = js_sys::Reflect::get(&global, &"clearTimeout".into()) {
        if let Some(clear) = clear.dyn_ref::<Function>() {
            let _ = clear.call1(&global, id);
        }
    }
}

/// Convert a JS metadata argument into a JSON map via `JSON.stringify`.
fn js_to_metadata(value: &JsValue) -> serde_json::Map<String, serde_json::Value> {
    if value.is_undefined() || value.is_null() {
        return serde_json::Map::new();
    }
    JSON::stringify(value)
        .ok()
        .filter(|s| !s.is_undefined())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&String::from(s)).ok())
        .and_then(|v| match v {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

/// Read a `{ "Name": "Value" }` header object from the options.
fn read_headers(options: &JsValue, key: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(obj) = js_sys::Reflect::get(options, &key.into()) {
        if obj.is_object() {
            for entry in Object::entries(obj.unchecked_ref()).iter() {
                let pair: Array = entry.into();
                if let (Some(k), Some(v)) = (pair.get(0).as_string(), pair.get(1).as_string()) {
                    out.push((k, v));
                }
            }
        }
    }
    out
}

fn option_str(options: &JsValue, key: &str) -> Option<String> {
    js_sys::Reflect::get(options, &key.into())
        .ok()
        .and_then(|v| v.as_string())
}

fn option_f64(options: &JsValue, key: &str, default: f64) -> f64 {
    js_sys::Reflect::get(options, &key.into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(default)
}

/// The browser-facing logger (spec §6.2, §6.3).
///
/// ```js
/// import { MicroLog } from 'tpt-microlog';
/// const log = new MicroLog({ endpoint: 'https://my-api.com/logs', batchSize: 50 });
/// log.info('Page viewed', { page: '/home', userId: 123 });
/// log.metric('load_time', 1.23, { page: '/home' });
/// await log.shutdown();
/// ```
#[wasm_bindgen]
pub struct MicroLog {
    inner: Rc<Inner>,
}

#[wasm_bindgen]
impl MicroLog {
    /// Create a logger from an options object:
    ///
    /// - `endpoint` — delivery URL (omit for a no-op logger)
    /// - `backend` — label; `"datadog"` sends `apiKey` as `DD-API-KEY`
    /// - `apiKey` — Datadog API key
    /// - `headers` — extra request headers, `{ "Name": "Value" }`
    /// - `batchSize` — flush at this many entries (default 50)
    /// - `flushInterval` — flush at least this often, seconds (default 2)
    /// - `service` — attached to every entry's metadata
    /// - `minLevel` — drop entries below this level (default `"DEBUG"`)
    /// - `sampleRate` — fraction of entries kept, 0.0–1.0 (default 1.0)
    /// - `maxBuffer` — cap on buffered entries during outages (default 10 000)
    /// - `maxRetries` — attempts before a transiently-failing batch is dropped (default 10)
    /// - `redactKeys` / `redactPatterns` / `redactBuiltin` — mask secrets/PII before buffering
    /// - `onError` — `(kind, message)` callback for internal failures (default `console.warn`)
    /// - `backend = "datadog"` — emit Datadog Logs API v2 event arrays (pair with `apiKey`)
    #[wasm_bindgen(constructor)]
    pub fn new(options: JsValue) -> MicroLog {
        let backend = option_str(&options, "backend").unwrap_or_default();
        let mut headers = read_headers(&options, "headers");
        if backend.eq_ignore_ascii_case("datadog") {
            if let Some(key) = option_str(&options, "apiKey") {
                headers.push(("DD-API-KEY".to_string(), key));
            }
        }
        let min_level = option_str(&options, "minLevel")
            .as_deref()
            .and_then(level_rank)
            .unwrap_or(1); // DEBUG
        let endpoint = option_str(&options, "endpoint").filter(|s| !s.is_empty());
        let datadog = option_str(&options, "backend")
            .map(|b| b.eq_ignore_ascii_case("datadog"))
            .unwrap_or(false);

        let mut pending_errors: Vec<String> = Vec::new();
        let inner = Rc::new(Inner {
            buffer: RefCell::new(Vec::new()),
            endpoint,
            headers,
            service: option_str(&options, "service"),
            min_level,
            sample_rate: option_f64(&options, "sampleRate", 1.0).clamp(0.0, 1.0),
            batch_size: option_f64(&options, "batchSize", 50.0).max(1.0) as usize,
            flush_interval_ms: (option_f64(&options, "flushInterval", 2.0) * 1000.0).max(50.0),
            max_retries_kept: option_f64(&options, "maxBuffer", 10_000.0).max(1.0) as usize,
            max_retries: option_f64(&options, "maxRetries", 10.0).max(1.0) as u32,
            datadog,
            redactor: {
                let mut sources: Vec<String> = Vec::new();
                if option_bool(&options, "redactBuiltin") {
                    sources.extend(BUILTIN_PATTERNS.iter().map(|p| p.to_string()));
                }
                sources.extend(option_string_list(&options, "redactPatterns"));
                let mut patterns = Vec::new();
                for src in sources {
                    match regex::Regex::new(&src) {
                        Ok(re) => patterns.push(re),
                        Err(e) => {
                            pending_errors.push(format!("ignoring invalid redact pattern: {e}"))
                        }
                    }
                }
                Redactor {
                    keys: option_string_list(&options, "redactKeys")
                        .iter()
                        .map(|k| k.to_ascii_lowercase())
                        .collect(),
                    patterns,
                }
            },
            on_error: js_sys::Reflect::get(&options, &"onError".into())
                .ok()
                .and_then(|v| v.dyn_into::<Function>().ok()),
            timer: RefCell::new(None),
            timer_id: RefCell::new(None),
            unload: RefCell::new(None),
            stopped: RefCell::new(false),
            delivered_entries: std::cell::Cell::new(0),
            dropped_entries: std::cell::Cell::new(0),
        });
        for msg in pending_errors {
            inner.report("config", &msg);
        }
        MicroLog { inner }.init()
    }

    fn init(self) -> MicroLog {
        self.arm_timer();
        self.register_unload();
        self
    }

    /// Start the self-rescheduling flush timer.
    fn arm_timer(&self) {
        let state = Rc::clone(&self.inner);
        let interval = self.inner.flush_interval_ms as i32;
        let closure = Closure::wrap(Box::new(move || {
            if *state.stopped.borrow() {
                return;
            }
            let pending = Rc::clone(&state);
            wasm_bindgen_futures::spawn_local(async move {
                pending.flush_once().await;
            });
            // Re-arm from the stored closure (kept alive inside `state.timer`).
            if let Some(stored) = state.timer.borrow().as_ref() {
                *state.timer_id.borrow_mut() =
                    schedule_timeout(stored.as_ref().unchecked_ref(), interval);
            }
        }) as Box<dyn FnMut()>);
        *self.inner.timer_id.borrow_mut() =
            schedule_timeout(closure.as_ref().unchecked_ref(), interval);
        *self.inner.timer.borrow_mut() = Some(closure);
    }

    /// Last-ditch delivery when the page goes away. `sendBeacon` cannot set
    /// request headers, so when custom headers (or Datadog's DD-API-KEY) are
    /// configured, a `fetch(..., { keepalive: true })` is used instead — the
    /// browser completes it even during page teardown.
    fn register_unload(&self) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let state = Rc::clone(&self.inner);
        let closure = Closure::wrap(Box::new(move || {
            let Some(endpoint) = state.endpoint.clone() else {
                return;
            };
            let Some(body) = state.pending_json() else {
                return;
            };
            if state.headers.is_empty() {
                send_beacon(&endpoint, &body);
            } else {
                let headers = state.headers.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = send_batch(&endpoint, &headers, &body, true).await;
                });
            }
        }) as Box<dyn FnMut()>);
        let _ =
            window.add_event_listener_with_callback("pagehide", closure.as_ref().unchecked_ref());
        *self.inner.unload.borrow_mut() = Some(closure);
    }

    /// The delivery endpoint, when configured.
    #[wasm_bindgen(getter)]
    pub fn endpoint(&self) -> Option<String> {
        self.inner.endpoint.clone()
    }

    /// Entries currently buffered.
    pub fn queued(&self) -> usize {
        self.inner.buffer.borrow().len()
    }

    /// Delivery counters as a plain JS object:
    /// `{ deliveredEntries, droppedEntries, queued, maxBuffer }`. The native
    /// `stats()` shape (snake_case keys) is mirrored where applicable.
    #[wasm_bindgen(js_name = stats)]
    pub fn stats(&self) -> JsValue {
        let obj = Object::new();
        let set = |key: &str, value: u32| {
            let _ = js_sys::Reflect::set(&obj, &key.into(), &value.into());
        };
        set("delivered_entries", self.inner.delivered_entries.get());
        set("dropped_entries", self.inner.dropped_entries.get());
        set("queued", self.inner.buffer.borrow().len() as u32);
        set("max_buffer", self.inner.max_retries_kept as u32);
        obj.into()
    }

    /// Deliver buffered entries immediately. Resolves with the number of
    /// entries still queued (nonzero means delivery failed and will retry).
    pub fn flush(&self) -> js_sys::Promise {
        let state = Rc::clone(&self.inner);
        wasm_bindgen_futures::future_to_promise(async move {
            state.flush_once().await;
            Ok(JsValue::from(state.buffer.borrow().len() as u32))
        })
    }

    /// Stop the timer, flush, and stop retrying. Idempotent.
    pub fn shutdown(&self) -> js_sys::Promise {
        let state = Rc::clone(&self.inner);
        wasm_bindgen_futures::future_to_promise(async move {
            *state.stopped.borrow_mut() = true;
            state.stop_timer();
            state.flush_once().await;
            Ok(JsValue::from(state.buffer.borrow().len() as u32))
        })
    }

    /// Release the background timer and unload hooks (breaks ref cycles).
    pub fn dispose(&self) {
        *self.inner.stopped.borrow_mut() = true;
        self.inner.stop_timer();
        *self.inner.unload.borrow_mut() = None;
    }

    /// Log a message at an explicit level (`"TRACE"`..`"FATAL"`).
    #[wasm_bindgen(js_name = logWithLevel)]
    pub fn log_with_level(&self, level: &str, message: &str, metadata: JsValue) {
        self.emit(level, message, &metadata, "log");
    }

    /// Log a structured event: `name` is the message, `fields` the metadata,
    /// plus `event` and `event_version` fields. `options` may hold `level`
    /// (default `"INFO"`) and `version` (default 1).
    #[wasm_bindgen(js_name = event)]
    pub fn event(&self, name: &str, fields: JsValue, options: JsValue) {
        let level = option_str(&options, "level").unwrap_or_else(|| "INFO".to_string());
        let version = option_f64(&options, "version", 1.0) as u64;
        let mut md = js_to_metadata(&fields);
        md.insert(
            "event".to_string(),
            serde_json::Value::String(name.to_string()),
        );
        md.insert(
            "event_version".to_string(),
            serde_json::Value::from(version),
        );
        if let Some(service) = &self.inner.service {
            md.entry("service".to_string())
                .or_insert(serde_json::Value::String(service.clone()));
        }
        if !self.inner.accept(&level) {
            return;
        }
        self.push_entry(Entry {
            timestamp: now_ms(),
            level: level.to_ascii_uppercase(),
            log_type: "log".to_string(),
            message: name.to_string(),
            metadata: md,
            retries: 0,
        });
    }

    #[wasm_bindgen(js_name = trace)]
    pub fn trace(&self, message: &str, metadata: JsValue) {
        self.emit("TRACE", message, &metadata, "log");
    }

    #[wasm_bindgen(js_name = debug)]
    pub fn debug(&self, message: &str, metadata: JsValue) {
        self.emit("DEBUG", message, &metadata, "log");
    }

    #[wasm_bindgen(js_name = info)]
    pub fn info(&self, message: &str, metadata: JsValue) {
        self.emit("INFO", message, &metadata, "log");
    }

    #[wasm_bindgen(js_name = warn)]
    pub fn warn(&self, message: &str, metadata: JsValue) {
        self.emit("WARN", message, &metadata, "log");
    }

    #[wasm_bindgen(js_name = error)]
    pub fn error(&self, message: &str, metadata: JsValue) {
        self.emit("ERROR", message, &metadata, "log");
    }

    #[wasm_bindgen(js_name = fatal)]
    pub fn fatal(&self, message: &str, metadata: JsValue) {
        self.emit("FATAL", message, &metadata, "log");
    }

    /// Record a numeric metric with optional tags object (spec §6.2).
    #[wasm_bindgen(js_name = metric)]
    pub fn metric(&self, name: &str, value: f64, tags: JsValue) {
        if !self.inner.accept("INFO") {
            return;
        }
        // Match the native wire format: `{value, tags}` under metadata.
        let mut metadata = serde_json::Map::new();
        metadata.insert("value".to_string(), serde_json::json!(value));
        metadata.insert(
            "tags".to_string(),
            serde_json::Value::Object(js_to_metadata(&tags)),
        );
        self.push_entry(Entry {
            timestamp: now_ms(),
            level: "INFO".to_string(),
            log_type: "metric".to_string(),
            message: name.to_string(),
            metadata,
            retries: 0,
        });
    }

    /// Record a distributed-trace span (spec §8).
    #[wasm_bindgen(js_name = span)]
    pub fn span(
        &self,
        name: &str,
        trace_id: &str,
        span_id: &str,
        parent_span_id: Option<String>,
        duration_us: u64,
    ) {
        if !self.inner.accept("INFO") {
            return;
        }
        let mut metadata = serde_json::Map::new();
        metadata.insert(
            "span".to_string(),
            serde_json::json!({
                "trace_id": trace_id,
                "span_id": span_id,
                "parent_span_id": parent_span_id,
                "duration_us": duration_us,
            }),
        );
        self.push_entry(Entry {
            timestamp: now_ms(),
            level: "INFO".to_string(),
            log_type: "trace".to_string(),
            message: name.to_string(),
            metadata,
            retries: 0,
        });
    }

    fn emit(&self, level: &str, message: &str, metadata: &JsValue, log_type: &'static str) {
        if !self.inner.accept(level) {
            return;
        }
        let mut md = js_to_metadata(metadata);
        if let Some(service) = &self.inner.service {
            md.entry("service".to_string())
                .or_insert(serde_json::Value::String(service.clone()));
        }
        self.push_entry(Entry {
            timestamp: now_ms(),
            level: level.to_ascii_uppercase(),
            log_type: log_type.to_string(),
            message: message.to_string(),
            metadata: md,
            retries: 0,
        });
    }

    fn push_entry(&self, mut entry: Entry) {
        self.inner.redactor.apply(&mut entry);
        if self.inner.push(entry) {
            let state = Rc::clone(&self.inner);
            wasm_bindgen_futures::spawn_local(async move {
                state.flush_once().await;
            });
        }
    }
}

/// Library version, re-exported for the npm package.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(message: &str) -> Entry {
        Entry {
            timestamp: 0,
            level: "INFO".into(),
            log_type: "log".into(),
            message: message.into(),
            metadata: serde_json::Map::new(),
            retries: 0,
        }
    }

    #[test]
    fn redactor_masks_keys_patterns_and_builtins() {
        let r = Redactor {
            keys: vec!["token".into()],
            patterns: BUILTIN_PATTERNS
                .iter()
                .map(|p| regex::Regex::new(p).unwrap())
                .collect(),
        };
        let mut e = entry("signup by alice@example.com");
        e.metadata.insert("Token".into(), "abc".into());
        e.metadata.insert(
            "nested".into(),
            serde_json::json!({"a": ["ssn 123-45-6789"], "n": 5}),
        );
        r.apply(&mut e);
        assert_eq!(e.message, "signup by [REDACTED]");
        assert_eq!(e.metadata["Token"], MASK);
        assert_eq!(e.metadata["nested"]["a"][0], "ssn [REDACTED]");
        assert_eq!(e.metadata["nested"]["n"], 5);
    }

    #[test]
    fn empty_redactor_is_a_no_op() {
        let mut e = entry("a@b.co");
        Redactor::default().apply(&mut e);
        assert_eq!(e.message, "a@b.co");
    }
}
