//! gRPC transport — OpenTelemetry Collector-compatible (spec §5.3).
//!
//! Exports batches through the OTLP `LogsService/Export` RPC on a plaintext
//! (or `https`) tonic channel. Retries / WAL fallback live in the core engine;
//! this transport performs a single attempt per call, classifying failures as
//! transient (retryable) or permanent.

use std::time::Duration;

use tonic::transport::Channel;
use tpt_log_core::{Config, LogEntry, LogLevel, Transport, TransportError};

use crate::gen::opentelemetry::proto::common::v1::any_value::Value;
use crate::gen::opentelemetry::proto::common::v1::{AnyValue, InstrumentationScope, KeyValue};
use crate::gen::opentelemetry::proto::resource::v1::Resource;
use crate::gen::{
    ExportLogsServiceRequest, LogRecord, LogsServiceClient, ResourceLogs, ScopeLogs, SeverityNumber,
};

/// OTLP logs gRPC transport.
///
/// Owns a small dedicated tokio runtime. tonic's channel spawns a background
/// worker task onto the runtime that is current when the channel is built, so
/// the channel must live on a runtime that keeps running independently of the
/// caller: the logger builds the transport on one thread and sends from its
/// delivery thread.
pub struct GrpcTransport {
    channel: Channel,
    service: String,
    /// `Option` so `Drop` can release it without blocking (dropping a runtime
    /// from inside another async context would otherwise panic).
    rt: Option<tokio::runtime::Runtime>,
}

impl std::fmt::Debug for GrpcTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcTransport")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

impl Drop for GrpcTransport {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

fn new_runtime() -> Result<tokio::runtime::Runtime, TransportError> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("tpt-microlog-grpc")
        .enable_all()
        .build()
        .map_err(|e| TransportError::Config(format!("cannot start gRPC runtime: {e}")))
}

fn endpoint_for(endpoint: &str) -> Result<tonic::transport::Endpoint, TransportError> {
    Ok(
        tonic::transport::Endpoint::from_shared(canonical_uri(endpoint))
            .map_err(|e| TransportError::Config(format!("invalid gRPC endpoint: {e}")))?
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15)),
    )
}

impl GrpcTransport {
    /// Connect to an OTLP gRPC endpoint, failing if it is unreachable now.
    pub async fn connect(endpoint: &str) -> Result<Self, TransportError> {
        let target = endpoint_for(endpoint)?;
        let rt = new_runtime()?;
        // Connect on the transport's own runtime so the channel worker lives
        // there; only the result is awaited on the caller's runtime.
        let channel = rt
            .spawn(async move { target.connect().await })
            .await
            .map_err(|e| TransportError::Transient(format!("gRPC connect task failed: {e}")))?
            .map_err(|e| TransportError::Transient(format!("gRPC connect failed: {e}")))?;
        Ok(Self {
            channel,
            service: endpoint.to_string(),
            rt: Some(rt),
        })
    }

    /// Connect from a blocking context, failing if the endpoint is unreachable.
    pub fn blocking_connect(endpoint: &str) -> Result<Self, TransportError> {
        let target = endpoint_for(endpoint)?;
        let rt = new_runtime()?;
        let channel = rt
            .block_on(async move { target.connect().await })
            .map_err(|e| TransportError::Transient(format!("gRPC connect failed: {e}")))?;
        Ok(Self {
            channel,
            service: endpoint.to_string(),
            rt: Some(rt),
        })
    }

    /// Build a transport that connects on first use and reconnects on demand.
    /// Never touches the network, so an application can start before its
    /// collector; failures surface as retryable errors from `send`, which the
    /// engine retries and falls back to the WAL for.
    pub fn lazy(endpoint: &str) -> Result<Self, TransportError> {
        let target = endpoint_for(endpoint)?;
        let rt = new_runtime()?;
        let channel = {
            let _guard = rt.enter();
            target.connect_lazy()
        };
        Ok(Self {
            channel,
            service: endpoint.to_string(),
            rt: Some(rt),
        })
    }

    /// Build from core config (`grpc_endpoint`); connects lazily.
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let endpoint = config
            .grpc_endpoint
            .clone()
            .ok_or_else(|| TransportError::Config("gRPC backend needs `grpc_endpoint`".into()))?;
        Self::lazy(&endpoint)
    }
}

fn canonical_uri(endpoint: &str) -> String {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        endpoint.to_string()
    } else {
        format!("http://{endpoint}")
    }
}

fn severity_for(level: LogLevel) -> SeverityNumber {
    match level {
        LogLevel::Trace => SeverityNumber::Trace,
        LogLevel::Debug => SeverityNumber::Debug,
        LogLevel::Info => SeverityNumber::Info,
        LogLevel::Warn => SeverityNumber::Warn,
        LogLevel::Error => SeverityNumber::Error,
        LogLevel::Fatal => SeverityNumber::Fatal,
    }
}

fn kv(key: &str, value: String) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(Value::StringValue(value)),
        }),
    }
}

fn log_record(entry: &LogEntry) -> LogRecord {
    let attributes: Vec<KeyValue> = entry
        .metadata
        .iter()
        .map(|(k, v)| kv(k, v.to_string()))
        .collect();
    LogRecord {
        time_unix_nano: entry.timestamp.saturating_mul(1_000_000),
        observed_time_unix_nano: 0,
        severity_number: severity_for(entry.level).into(),
        severity_text: entry.level.to_string().to_ascii_lowercase(),
        body: Some(AnyValue {
            value: Some(Value::StringValue(entry.message.clone())),
        }),
        attributes,
        dropped_attributes_count: 0,
        flags: 0,
        trace_id: Vec::new(),
        span_id: Vec::new(),
    }
}

fn build_request(service: &str, entries: &[LogEntry]) -> ExportLogsServiceRequest {
    let mut resource_attrs = vec![kv("service.name", service.to_string())];
    for e in entries {
        if let Some(host) = e.metadata.get("host").and_then(|v| v.as_str()) {
            if !resource_attrs.iter().any(|a| a.key == "host.name") {
                resource_attrs.push(kv("host.name", host.to_string()));
            }
            break;
        }
    }
    let scope = ScopeLogs {
        scope: Some(InstrumentationScope {
            name: "tpt-microlog".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
        log_records: entries.iter().map(log_record).collect(),
    };
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: resource_attrs,
                dropped_attributes_count: 0,
            }),
            scope_logs: vec![scope],
        }],
    }
}

/// Convert a tonic status into a transport error classification.
fn classify(status: tonic::Status) -> TransportError {
    use tonic::Code::*;
    match status.code() {
        Unauthenticated | PermissionDenied | InvalidArgument | FailedPrecondition | NotFound
        | AlreadyExists | OutOfRange | Unimplemented => {
            TransportError::Permanent(status.to_string())
        }
        _ => TransportError::Transient(status.to_string()),
    }
}

impl Transport for GrpcTransport {
    fn send(&self, batch: &tpt_log_core::CompressedBatch) -> Result<(), TransportError> {
        let entries = batch
            .decompress_entries()
            .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;
        if entries.is_empty() {
            return Ok(());
        }
        let request = build_request(&self.service, &entries);
        let mut client = LogsServiceClient::new(self.channel.clone());
        let rt = self
            .rt
            .as_ref()
            .ok_or_else(|| TransportError::Transient("gRPC transport is shut down".into()))?;
        rt.block_on(async {
            match client.export(request).await {
                Ok(_) => Ok(()),
                Err(status) => Err(classify(status)),
            }
        })
    }

    fn name(&self) -> &str {
        "grpc"
    }
}
