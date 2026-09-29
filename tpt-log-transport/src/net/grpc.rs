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

// Thread-local current-thread runtime so blocking `send` calls can drive the
// tonic client without paying runtime construction per attempt.
thread_local! {
    static RT: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
}

/// OTLP logs gRPC transport.
pub struct GrpcTransport {
    channel: Channel,
    service: String,
}

impl std::fmt::Debug for GrpcTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcTransport")
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

impl GrpcTransport {
    /// Connect to an OTLP gRPC endpoint asynchronously.
    pub async fn connect(endpoint: &str) -> Result<Self, TransportError> {
        let uri = canonical_uri(endpoint);
        let channel = tonic::transport::Endpoint::from_shared(uri)
            .map_err(|e| TransportError::Config(format!("invalid gRPC endpoint: {e}")))?
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .connect()
            .await
            .map_err(|e| TransportError::Transient(format!("gRPC connect failed: {e}")))?;
        Ok(Self {
            channel,
            service: endpoint.to_string(),
        })
    }

    /// Connect from a blocking context (no async runtime on the caller).
    pub fn blocking_connect(endpoint: &str) -> Result<Self, TransportError> {
        RT.with(|rt| rt.block_on(Self::connect(endpoint)))
    }

    /// Build from core config (`grpc_endpoint`).
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let endpoint = config
            .grpc_endpoint
            .clone()
            .ok_or_else(|| TransportError::Config("gRPC backend needs `grpc_endpoint`".into()))?;
        Self::blocking_connect(&endpoint)
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
        RT.with(|rt| {
            rt.block_on(async {
                match client.export(request).await {
                    Ok(_) => Ok(()),
                    Err(status) => Err(classify(status)),
                }
            })
        })
    }

    fn name(&self) -> &str {
        "grpc"
    }
}
