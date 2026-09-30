use std::io::Cursor;

use serde::{Deserialize, Serialize};

use crate::LogEntry;

/// A decompressed collection of entries awaiting transport (spec §5.2).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Batch {
    pub entries: Vec<LogEntry>,
}

impl Batch {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Serialize the whole batch to JSON bytes (`{"entries":[...]}`).
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Parse JSON bytes back into a batch.
    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

/// Framed wire payload delivered to a transport.
///
/// Carries the raw compressed bytes plus enough context to re-construct the
/// original entries should a re-transport need them decoded. When compression
/// is active, the uncompressed JSON travels alongside (`raw_json`) so adapters
/// that need plain JSON do not have to decompress, parse, and re-serialize.
#[derive(Clone, Debug)]
pub struct CompressedBatchPayload {
    pub bytes: Vec<u8>,
    pub entry_count: usize,
    pub compressed: bool,
    /// Uncompressed batch JSON; `Some` exactly when `compressed` is true and
    /// the payload originated from the in-process pipeline (WAL frames leave
    /// it `None` and decode lazily).
    pub raw_json: Option<std::sync::Arc<Vec<u8>>>,
}

impl CompressedBatchPayload {
    /// Batch JSON without re-parsing: uses the carried raw JSON when present,
    /// decompresses otherwise. Cheap for in-process batches.
    pub fn json_bytes(&self) -> Result<Vec<u8>, String> {
        if let Some(json) = &self.raw_json {
            return Ok((**json).clone());
        }
        if !self.compressed {
            return Ok(self.bytes.clone());
        }
        Compressor::decompress(&self.bytes).map_err(|e| format!("decompression failed: {e}"))
    }

    /// Decode the original entries (used by tests / adapters / WAL).
    pub fn decode_entries(&self) -> Result<Vec<LogEntry>, String> {
        let data = self.json_bytes()?;
        Batch::from_json(&data)
            .map(|b| b.entries)
            .map_err(|e| e.to_string())
    }
}

/// A compressed group of entries ready to hand to a [`crate::Transport`].
#[derive(Clone, Debug)]
pub struct CompressedBatch {
    pub payload: CompressedBatchPayload,
}

impl CompressedBatch {
    /// Reconstruct the decompressed entries (used by tests / adapters).
    pub fn decompress_entries(&self) -> Result<Vec<LogEntry>, String> {
        self.payload.decode_entries()
    }
}

/// zstd wrapper honoring the "fast mode" (no compression, spec §9).
///
/// Compression level 3 is the default (spec §5.2); higher levels trade CPU for
/// a smaller payload, lower levels trade size for lower latency.
#[derive(Clone, Debug)]
pub struct Compressor {
    level: i32,
    enabled: bool,
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new(crate::defaults::COMPRESSION_LEVEL, false)
    }
}

impl Compressor {
    /// Create a compressor with the given level. Pass `level == 0` or
    /// `disable == true` to disable compression ("fast mode").
    pub fn new(level: i32, disable: bool) -> Self {
        Self {
            level: level.max(1),
            enabled: level > 0 && !disable,
        }
    }

    /// Create a compressor honoring the config (fast_mode disables zstd).
    pub fn from_config(cfg: &crate::Config) -> Self {
        Self::new(cfg.compression_level, cfg.fast_mode)
    }

    /// Compress serialized JSON. Returns identity bytes when compression is
    /// disabled.
    pub fn compress(&self, data: &[u8]) -> Vec<u8> {
        if !self.enabled {
            return data.to_vec();
        }
        let mut out = Cursor::new(Vec::with_capacity(data.len() / 4));
        // `Encoder::new` cannot fail for an in-memory `Cursor` writer.
        let mut enc = zstd::stream::write::Encoder::new(&mut out, self.level).unwrap();
        let _ = std::io::Write::write_all(&mut enc, data);
        let _ = enc.finish().ok();
        out.into_inner()
    }

    /// Static decompressor for reconstructing batches from wire/WAL data.
    pub fn decompress(data: &[u8]) -> Result<Vec<u8>, String> {
        zstd::stream::decode_all(Cursor::new(data)).map_err(|e| e.to_string())
    }

    /// Compress an entire [`Batch`] into a single [`CompressedBatch`].
    pub fn compress_batch(&self, batch: &Batch, entry_count: usize) -> CompressedBatch {
        let json = batch.to_json();
        let bytes = self.compress(&json);
        CompressedBatch {
            payload: CompressedBatchPayload {
                bytes,
                entry_count,
                compressed: self.enabled,
                raw_json: self.enabled.then(|| std::sync::Arc::new(json)),
            },
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LogLevel, LogType};

    fn sample_batch(n: usize) -> Batch {
        Batch {
            entries: (0..n)
                .map(|i| LogEntry {
                    timestamp: i as u64,
                    level: LogLevel::Info,
                    log_type: LogType::Log,
                    message: format!("message number {i} padding to be compressible uhoh"),
                    metadata: Default::default(),
                })
                .collect(),
        }
    }

    #[test]
    fn roundtrip_compressed() {
        let c = Compressor::new(3, false);
        let b = sample_batch(100);
        let cb = c.compress_batch(&b, b.len());
        assert!(cb.payload.compressed);
        assert!(cb.payload.bytes.len() < b.to_json().len() / 2);

        let decoded = cb.decompress_entries().unwrap();
        assert_eq!(decoded.len(), 100);
        assert_eq!(decoded[0].message, sample_batch(100).entries[0].message);
    }

    #[test]
    fn roundtrip_uncompressed_fast_mode() {
        let c = Compressor::new(0, true);
        let b = sample_batch(3);
        let cb = c.compress_batch(&b, 3);
        assert!(!cb.payload.compressed);
        let decoded = cb.decompress_entries().unwrap();
        assert_eq!(decoded.len(), 3);
    }

    #[test]
    fn batch_json_roundtrip() {
        let b = sample_batch(4);
        let json = b.to_json();
        let back = Batch::from_json(&json).unwrap();
        assert_eq!(back.len(), 4);
    }
}
