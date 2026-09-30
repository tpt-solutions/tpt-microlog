//! `tpt-log-py` — Python bindings for `tpt-microlog` (Phase 5).
//!
//! Exposes the [`tpt_log_core::MicroLog`] engine as a native `MicroLog` class
//! (imported as `tpt_microlog.MicroLog`). Keyword arguments become structured
//! metadata: `log.info("user logged in", user_id=123, ip="1.2.3.4")`.
//!
//! Built with [maturin](https://www.maturin.rs); see `pyproject.toml`.

#[cfg(feature = "pyo3")]
mod native {
    use std::sync::Arc;

    use pyo3::exceptions::PyValueError;
    use pyo3::prelude::*;
    use pyo3::types::{PyAny, PyDict, PyList};

    use tpt_log_core::{Config, LogLevel, MicroLog as CoreLog, Secret};
    use tpt_log_transport::build_transport;

    /// Convert one Python object into the equivalent JSON value so structured
    /// metadata survives the boundary losslessly.
    fn py_to_json(v: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
        if v.is_none() {
            return Ok(serde_json::Value::Null);
        }
        // `bool` must be tested before `int`: `bool` is a subtype of `int`.
        if let Ok(b) = v.extract::<bool>() {
            return Ok(serde_json::Value::Bool(b));
        }
        if let Ok(i) = v.extract::<i64>() {
            return Ok(serde_json::Value::from(i));
        }
        if let Ok(f) = v.extract::<f64>() {
            // JSON has no NaN/Inf; keep the information as strings instead of
            // raising, so a NaN metric never breaks a log call.
            if f.is_nan() {
                return Ok(serde_json::Value::String("NaN".to_string()));
            }
            if f.is_infinite() {
                let text = if f > 0.0 { "Infinity" } else { "-Infinity" };
                return Ok(serde_json::Value::String(text.to_string()));
            }
            let n = serde_json::Number::from_f64(f)
                .ok_or_else(|| PyValueError::new_err("float is not valid JSON"))?;
            return Ok(serde_json::Value::Number(n));
        }
        if let Ok(s) = v.extract::<String>() {
            return Ok(serde_json::Value::String(s));
        }
        if let Ok(list) = v.cast::<PyList>() {
            let mut out = Vec::with_capacity(list.len());
            for item in list.iter() {
                out.push(py_to_json(&item)?);
            }
            return Ok(serde_json::Value::Array(out));
        }
        if let Ok(dict) = v.cast::<PyDict>() {
            let mut map = serde_json::Map::new();
            for (k, val) in dict.iter() {
                let key: String = k.extract()?;
                map.insert(key, py_to_json(&val)?);
            }
            return Ok(serde_json::Value::Object(map));
        }
        // Fallback: keep the information as a string.
        Ok(serde_json::Value::String(v.str()?.to_cow()?.into_owned()))
    }

    /// Materialize `**kwargs` into a JSON metadata map.
    fn kwargs_to_metadata(
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<serde_json::Map<String, serde_json::Value>> {
        let mut map = serde_json::Map::new();
        if let Some(kwargs) = kwargs {
            for (k, v) in kwargs.iter() {
                let key: String = k.extract()?;
                map.insert(key, py_to_json(&v)?);
            }
        }
        Ok(map)
    }

    /// Apply one constructor keyword to the config being built.
    fn apply_kwarg(config: &mut Config, key: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        match key {
            "backend" => config.backend = value.extract()?,
            "endpoint" => config.endpoint = value.extract()?,
            "api_key" => config.api_key = value.extract::<Option<String>>()?.map(Secret::new),
            "service" => config.service = value.extract()?,
            "batch_size" => config.batch_size = value.extract()?,
            "flush_interval" => {
                let secs: f64 = value.extract()?;
                if secs <= 0.0 {
                    return Err(PyValueError::new_err("flush_interval must be > 0"));
                }
                config.flush_interval_ms = (secs * 1000.0).round() as u64;
            }
            "memory_limit_mb" => {
                let mb: f64 = value.extract()?;
                config.memory_limit_bytes = (mb * 1024.0 * 1024.0) as usize;
            }
            "queue_capacity" => config.queue_capacity = value.extract()?,
            "compression_level" => config.compression_level = value.extract()?,
            "fast_mode" => config.fast_mode = value.extract()?,
            "max_retries" => config.max_retries = value.extract()?,
            "retry_base_ms" => config.retry_base_ms = value.extract()?,
            "retry_max_backoff_ms" => config.retry_max_backoff_ms = value.extract()?,
            "wal_enabled" => config.wal_enabled = value.extract()?,
            "wal_dir" => config.wal_dir = value.extract()?,
            "wal_max_size_mb" => {
                let mb: f64 = value.extract()?;
                config.wal_max_size_bytes = (mb * 1024.0 * 1024.0) as u64;
            }
            "min_level" => config.min_level = value.extract()?,
            "sample_rate" => config.sample_rate = value.extract()?,
            "enrichment" => config.enrichment = value.extract()?,
            "extra_headers" => config.extra_headers = value.extract()?,
            "grpc_endpoint" => config.grpc_endpoint = value.extract()?,
            "net_endpoint" => config.net_endpoint = value.extract()?,
            "file_path" => config.file_path = value.extract()?,
            "file_rotation_bytes" => config.file_rotation_bytes = value.extract()?,
            "file_rotation_seconds" => config.file_rotation_seconds = value.extract()?,
            "file_rotations" => config.file_rotations = value.extract()?,
            "prometheus_bind" => config.prometheus_bind = value.extract()?,
            "aws_region" => config.aws_region = value.extract()?,
            "aws_access_key" => config.aws_access_key = value.extract()?,
            "aws_secret_key" => {
                config.aws_secret_key = value.extract::<Option<String>>()?.map(Secret::new)
            }
            "aws_log_group" => config.aws_log_group = value.extract()?,
            "aws_log_stream" => config.aws_log_stream = value.extract()?,
            other => {
                return Err(PyValueError::new_err(format!(
                    "unknown MicroLog option '{other}'"
                )))
            }
        }
        Ok(())
    }

    /// The Python-facing logger (spec §6.1).
    ///
    /// ```python
    /// from tpt_microlog import MicroLog
    /// log = MicroLog(backend="datadog", api_key="...", batch_size=100)
    /// log.info("User logged in", user_id=123, ip="1.2.3.4")
    /// log.metric("cpu_usage", 42.5, tags={"host": "server-01"})
    /// log.shutdown()
    /// ```
    #[pyclass(subclass)]
    struct MicroLog {
        inner: CoreLog,
    }

    #[pymethods]
    impl MicroLog {
        /// Create a logger. Starts from `tpt-log.toml` + `TPT_LOG_*` env
        /// (auto-detection), then applies the given keyword overrides.
        #[new]
        #[pyo3(signature = (**kwargs))]
        fn new(py: Python<'_>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<Self> {
            let mut config = Config::load();
            if let Some(kwargs) = kwargs {
                for (k, v) in kwargs.iter() {
                    let key: String = k.extract()?;
                    apply_kwarg(&mut config, &key, &v)?;
                }
            }
            // Building the transport may bind sockets (Prometheus) or read
            // the filesystem: release the GIL while it runs.
            let built = py.detach(|| {
                build_transport(&config)
                    .map(|t| CoreLog::new(config, Arc::from(t)))
                    .map_err(|e| PyValueError::new_err(e.to_string()))
            });
            Ok(MicroLog { inner: built? })
        }

        #[pyo3(signature = (message, **kwargs))]
        fn trace(&self, message: &str, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
            self.log_at(LogLevel::Trace, message, kwargs)
        }

        #[pyo3(signature = (message, **kwargs))]
        fn debug(&self, message: &str, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
            self.log_at(LogLevel::Debug, message, kwargs)
        }

        #[pyo3(signature = (message, **kwargs))]
        fn info(&self, message: &str, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
            self.log_at(LogLevel::Info, message, kwargs)
        }

        #[pyo3(signature = (message, **kwargs))]
        fn warn(&self, message: &str, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
            self.log_at(LogLevel::Warn, message, kwargs)
        }

        #[pyo3(signature = (message, **kwargs))]
        fn error(&self, message: &str, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
            self.log_at(LogLevel::Error, message, kwargs)
        }

        #[pyo3(signature = (message, **kwargs))]
        fn fatal(&self, message: &str, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
            self.log_at(LogLevel::Fatal, message, kwargs)
        }

        /// Log a numeric metric with optional `tags` dict (spec §6.1).
        #[pyo3(signature = (name, value, tags=None))]
        fn metric(&self, name: &str, value: f64, tags: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
            match tags {
                Some(tags) => {
                    let mut map = serde_json::Map::new();
                    for (k, v) in tags.iter() {
                        let key: String = k.extract()?;
                        map.insert(key, py_to_json(&v)?);
                    }
                    self.inner.metric(name, value, Some(&map));
                }
                None => self.inner.metric(name, value, None),
            }
            Ok(())
        }

        /// Record a distributed-trace span (spec §8).
        #[pyo3(signature = (name, trace_id, span_id, parent_span_id=None, duration_us=0))]
        fn span(
            &self,
            name: &str,
            trace_id: &str,
            span_id: &str,
            parent_span_id: Option<String>,
            duration_us: u64,
        ) {
            self.inner
                .span(name, trace_id, span_id, parent_span_id, duration_us);
        }

        /// Number of entries still buffered in the ingestion queue.
        fn queued(&self) -> usize {
            self.inner.queued()
        }

        /// The active backend name.
        #[getter]
        fn backend(&self) -> &str {
            &self.inner.config().backend
        }

        /// Push buffered entries into the delivery pipeline now. Blocks (with
        /// the GIL released) up to the configured shutdown timeout; returns
        /// `True` when the queue and delivery channel drained.
        fn flush(&self, py: Python<'_>) -> bool {
            py.detach(|| self.inner.flush())
        }

        /// Delivery counters as a dict, e.g.
        /// `{"delivered_batches": 3, "dropped_entries": 0, ...}`.
        fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
            use std::sync::atomic::Ordering::Relaxed;
            let s = self.inner.stats();
            let dict = PyDict::new(py);
            dict.set_item("delivered_batches", s.delivered_batches.load(Relaxed))?;
            dict.set_item("delivered_entries", s.delivered_entries.load(Relaxed))?;
            dict.set_item("wal_writes", s.wal_writes.load(Relaxed))?;
            dict.set_item("retried_entries", s.retried_entries.load(Relaxed))?;
            dict.set_item("dropped_batches", s.dropped_batches.load(Relaxed))?;
            dict.set_item("dropped_entries", s.dropped_entries.load(Relaxed))?;
            dict.set_item("queued", self.inner.queued())?;
            dict.set_item("inflight", self.inner.inflight())?;
            Ok(dict)
        }

        /// Context manager entry: `with MicroLog(...) as log: ...`.
        fn __enter__(slf: Py<Self>) -> Py<Self> {
            slf
        }

        /// Context manager exit: shuts the engine down (idempotent).
        fn __exit__(
            &self,
            py: Python<'_>,
            _exc_type: Py<PyAny>,
            _exc_value: Py<PyAny>,
            _traceback: Py<PyAny>,
        ) -> PyResult<bool> {
            self.shutdown_internal(py);
            Ok(false) // do not suppress exceptions
        }

        /// Called (from Python) after a fork in the child process: the worker
        /// threads do not exist there, so the logger detaches instead of
        /// deadlocking. All further calls become cheap no-ops.
        fn _detach_after_fork(&self) {
            self.inner.detach_after_fork();
        }

        /// Graceful shutdown: flush pending batches and drain the WAL
        /// (spec §6.1). Idempotent; also runs when the object is collected.
        /// The GIL is released while the engine drains.
        fn shutdown(&self, py: Python<'_>) {
            self.shutdown_internal(py);
        }
    }

    impl MicroLog {
        fn shutdown_internal(&self, py: Python<'_>) {
            py.detach(|| self.inner.shutdown());
        }
    }

    impl MicroLog {
        fn log_at(
            &self,
            level: LogLevel,
            message: &str,
            kwargs: Option<&Bound<'_, PyDict>>,
        ) -> PyResult<()> {
            let metadata = kwargs_to_metadata(kwargs)?;
            if metadata.is_empty() {
                match level {
                    LogLevel::Trace => self.inner.trace(message),
                    LogLevel::Debug => self.inner.debug(message),
                    LogLevel::Info => self.inner.info(message),
                    LogLevel::Warn => self.inner.warn(message),
                    LogLevel::Error => self.inner.error(message),
                    LogLevel::Fatal => self.inner.fatal(message),
                }
            } else {
                self.inner.log_with(level, message, metadata);
            }
            Ok(())
        }
    }

    /// Load a logger from a `tpt-log.toml` file (env overrides still apply).
    #[pyfunction]
    fn load_config(path: &str) -> PyResult<MicroLog> {
        let mut config =
            Config::from_file(path).map_err(|e| PyValueError::new_err(e.to_string()))?;
        config.apply_env();
        let transport =
            build_transport(&config).map_err(|e| PyValueError::new_err(e.to_string()))?;
        Ok(MicroLog {
            inner: CoreLog::new(config, Arc::from(transport)),
        })
    }

    #[pymodule]
    fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
        module.add_class::<MicroLog>()?;
        module.add_function(wrap_pyfunction!(load_config, module)?)?;
        module.add("__version__", env!("CARGO_PKG_VERSION"))?;
        Ok(())
    }
}

/// Placeholder surface when the crate is built without the `pyo3` feature.
#[cfg(not(feature = "pyo3"))]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
