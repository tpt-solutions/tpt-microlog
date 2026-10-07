//! `tpt-log-transport` — delivery backends for `tpt-microlog`.
//!
//! Implementations of the core [`Transport`] trait plus backend adapters and a
//! config-to-transport factory. The blocking contract keeps delivery threads
//! runtime-free; retries, WAL persistence, and graceful shutdown live in
//! `tpt-log-core`.

pub mod adapters;
pub mod factory;
// tonic-generated service signatures return large `Status` errors.
#[allow(clippy::result_large_err)]
pub mod gen;
pub mod http;
pub mod net;

pub use factory::build_transport;
pub use http::HttpTransport;
pub use net::file::FileTransport;
pub use net::grpc::GrpcTransport;
pub use net::tcp::TcpTransport;
pub use net::udp::UdpTransport;

pub use tpt_log_core::{BodyMode, Config, Transport, TransportError};
