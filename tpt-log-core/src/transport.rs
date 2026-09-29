use crate::CompressedBatch;

/// Errors that can arise while delivering a compressed batch to a backend.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// Transport failed but may succeed on retry (network down, 5xx, timeout).
    #[error("transient transport failure: {0}")]
    Transient(String),
    /// Transport failed permanently (4xx auth, bad payload). Do not retry.
    #[error("permanent transport failure: {0}")]
    Permanent(String),
    /// Configuration error (invalid URL, missing credentials).
    #[error("configuration error: {0}")]
    Config(String),
    /// Local I/O error (WAL write, file transport).
    #[error("io error: {0}")]
    Io(std::io::Error),
    /// Backend rejected the payload with an unexpected status.
    #[error("backend rejected payload: {0}")]
    Rejected(String),
}

impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        TransportError::Io(e)
    }
}

/// A pluggable destination for compressed log batches.
///
/// Implementations are expected to be `Send + Sync`. The blocking contract
/// keeps the internal pipeline thread-based (no async runtime required), which
/// matches the resource-constrained design (spec §2, §4.2).
pub trait Transport: Send + Sync {
    /// Deliver one compressed batch. `Ok(())` frees the batch from memory;
    /// `Err` sends it to the WAL for guaranteed delivery (spec §4.2).
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError>;

    /// Human-readable backend name for diagnostics.
    fn name(&self) -> &str {
        "transport"
    }
}

/// Transport that discards everything — used when no backend is configured.
#[derive(Default)]
pub struct NullTransport;

impl Transport for NullTransport {
    fn send(&self, _batch: &CompressedBatch) -> Result<(), TransportError> {
        Ok(())
    }

    fn name(&self) -> &str {
        "none"
    }
}

/// A transport spy that records delivered payloads (used by tests and the
/// FFI "survey" mode).
#[derive(Debug, Default)]
pub struct SpyTransport {
    pub(crate) delivered: std::sync::Mutex<Vec<crate::batch::CompressedBatchPayload>>,
}

impl SpyTransport {
    /// Snapshot of recorded payloads.
    pub fn snapshot(&self) -> Vec<crate::batch::CompressedBatchPayload> {
        self.delivered.lock().unwrap().clone()
    }

    /// Decoded entries across all recorded payloads.
    pub fn entries(&self) -> Vec<crate::LogEntry> {
        let mut out = Vec::new();
        for payload in self.snapshot() {
            out.extend(payload.decode_entries().unwrap_or_default());
        }
        out
    }
}

impl Transport for SpyTransport {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        self.delivered.lock().unwrap().push(batch.payload.clone());
        Ok(())
    }

    fn name(&self) -> &str {
        "spy"
    }
}
