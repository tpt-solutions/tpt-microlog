//! `tpt-log-core` — the microscopic telemetry & log router core.
//!
//! Pure-Rust logging engine providing a lock-free ingestion queue, background
//! batching, zstd compression, a write-ahead log, and exponential-backoff
//! retry. Network transports live in `tpt-log-transport` and plug in through
//! the [`Transport`] trait.

pub mod batch;
pub mod config;
pub mod diag;
pub mod entry;
pub mod events;
pub mod filter;
pub mod governor;
pub mod health;
pub mod integrations;
pub mod level;
pub mod log_type;
pub mod microlog;
pub mod queue;
pub mod redact;
pub mod retry;
pub mod trace;
pub mod transport;
pub mod wal;

pub use batch::{Batch, CompressedBatch, Compressor};
pub use config::{BodyMode, Config, Durability, QueueFullPolicy, Secret};
pub use diag::set_on_error;
pub use entry::LogEntry;
pub use health::{Health, HealthStatus};
pub use integrations::install_panic_hook;
pub use level::LogLevel;
pub use log_type::LogType;
pub use microlog::MicroLog;
pub use queue::PushOutcome;
pub use transport::{NullTransport, Transport, TransportError};
pub use wal::Wal;

/// Default end-of-file batching constants (spec §5.2).
pub mod defaults {
    /// Flush when the batch reaches this many entries.
    pub const BATCH_SIZE: usize = 100;
    /// Flush at least this often (milliseconds).
    pub const FLUSH_INTERVAL_MS: u64 = 1_000;
    /// Flush when in-memory bytes cross this threshold.
    pub const MEMORY_LIMIT_BYTES: usize = 10 * 1024 * 1024;
    /// Bounded ingestion queue capacity.
    pub const QUEUE_CAPACITY: usize = 10_000;
    /// zstd compression level (spec §5.2).
    pub const COMPRESSION_LEVEL: i32 = 3;
    /// Exponential-backoff base delay (milliseconds).
    pub const RETRY_BASE_MS: u64 = 1_000;
    /// Maximum backoff delay (milliseconds).
    pub const RETRY_MAX_BACKOFF_MS: u64 = 60_000;
    /// Maximum delivery attempts.
    pub const MAX_RETRIES: u32 = 10;
    /// Default WAL size cap (bytes).
    pub const WAL_MAX_SIZE_BYTES: u64 = 100 * 1024 * 1024;
    /// How often the WAL retry thread re-attempts delivery.
    pub const WAL_RETRY_INTERVAL_MS: u64 = 5_000;
    /// Failed replay passes before a WAL file moves to `wal/dead/`.
    pub const WAL_DEAD_LETTER_REPLAYS: u32 = 8;
    /// Hard budget for `shutdown()` before remaining batches spill to WAL.
    pub const SHUTDOWN_TIMEOUT_MS: u64 = 5_000;
    /// Upper bound honored from a backend's `Retry-After` header.
    pub const RETRY_AFTER_MAX_MS: u64 = 120_000;
}
