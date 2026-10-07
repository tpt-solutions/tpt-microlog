//! `tpt-log-transport` — delivery backends for `tpt-microlog`.
//!
//! Implementations of the core [`Transport`] trait plus backend adapters and a
//! config-to-transport factory. The blocking contract keeps delivery threads
//! runtime-free; retries, WAL persistence, and graceful shutdown live in
//! `tpt-log-core`.

pub mod adapters;
pub mod builder;
pub mod factory;
// tonic-generated service signatures return large `Status` errors.
#[allow(clippy::result_large_err)]
pub mod gen;
pub mod http;
pub mod net;

pub use builder::Builder;
pub use factory::{build_transport, BACKENDS};
pub use http::HttpTransport;
pub use net::console::ConsoleTransport;
pub use net::fanout::RoutingTransport;
pub use net::file::FileTransport;
pub use net::grpc::GrpcTransport;
pub use net::tcp::TcpTransport;
pub use net::udp::UdpTransport;

pub use adapters::loki::LokiAdapter;
pub use adapters::otlp_http::OtlpHttpAdapter;
pub use adapters::splunk::SplunkAdapter;

pub use tpt_log_core::{BodyMode, Config, Transport, TransportError};
