//! Protocol buffers generated from the OpenTelemetry protos (build.rs).
//!
//! prost emits one Rust module per proto package, so the module tree below
//! mirrors `opentelemetry.proto.*` and each leaf includes its generated file.

pub mod opentelemetry {
    pub mod proto {
        pub mod common {
            pub mod v1 {
                tonic::include_proto!("opentelemetry.proto.common.v1");
            }
        }
        pub mod resource {
            pub mod v1 {
                tonic::include_proto!("opentelemetry.proto.resource.v1");
            }
        }
        pub mod logs {
            pub mod v1 {
                tonic::include_proto!("opentelemetry.proto.logs.v1");
            }
        }
    }
}

pub use opentelemetry::proto::logs::v1::{
    logs_service_client::LogsServiceClient, logs_service_server::LogsService,
    logs_service_server::LogsServiceServer, ExportLogsServiceRequest, LogRecord, ResourceLogs,
    ScopeLogs, SeverityNumber,
};
