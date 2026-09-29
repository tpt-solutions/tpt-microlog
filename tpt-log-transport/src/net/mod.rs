//! Low-level network transports: gRPC (OLTP), UDP datagrams, TCP streaming,
//! and JSONL file transport with rotation (spec §5.3).

pub mod file;
pub mod grpc;
pub mod tcp;
pub mod udp;
