//! Low-level network transports: gRPC (OTLP), UDP datagrams, TCP streaming,
//! JSONL file transport with rotation, console/stdout, and fanout routing
//! (spec §5.3).

pub mod console;
pub mod fanout;
pub mod file;
pub mod grpc;
pub mod tcp;
pub mod udp;
