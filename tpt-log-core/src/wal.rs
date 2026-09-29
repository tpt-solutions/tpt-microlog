use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use parking_lot::Mutex;

use crate::CompressedBatch;

/// One persisted batch frame (spec §5.4).
///
/// File format: one batch per file, encoded as a single line of
/// base64(<marker><compressed payload>), where `<marker>` is `1` when the
/// payload is a zstd frame and `0` when it is stored raw (fast mode).
pub struct WalFrame {
    pub compressed: bool,
    pub bytes: Vec<u8>,
}

const MARKER_COMPRESSED: u8 = 1;
const MARKER_RAW: u8 = 0;

impl WalFrame {
    pub fn encode(&self) -> String {
        let mut buf = Vec::with_capacity(self.bytes.len() + 1);
        buf.push(if self.compressed {
            MARKER_COMPRESSED
        } else {
            MARKER_RAW
        });
        buf.extend_from_slice(&self.bytes);
        B64.encode(buf)
    }

    pub fn decode(line: &str) -> Option<WalFrame> {
        let raw = B64.decode(line.trim()).ok()?;
        if raw.is_empty() {
            return None;
        }
        match raw[0] {
            MARKER_COMPRESSED => Some(WalFrame {
                compressed: true,
                bytes: raw[1..].to_vec(),
            }),
            MARKER_RAW => Some(WalFrame {
                compressed: false,
                bytes: raw[1..].to_vec(),
            }),
            _ => None,
        }
    }

    /// Parse an encoded line back into a [`CompressedBatch`].
    pub fn to_compressed_batch(&self) -> CompressedBatch {
        CompressedBatch {
            payload: crate::batch::CompressedBatchPayload {
                bytes: self.bytes.clone(),
                entry_count: 0,
                compressed: self.compressed,
            },
        }
    }
}

fn default_ext() -> &'static str {
    "wal"
}

/// Write-ahead log: append-only persistence for batches that could not be
/// delivered (spec §5.4). One file per batch keeps delivery deletion atomic and
/// the FIFO size cap trivial (delete the oldest files).
pub struct Wal {
    dir: PathBuf,
    max_size: u64,
    lock: Mutex<()>,
    seq: AtomicU32,
}

impl Wal {
    /// Open (creating if needed) the WAL directory.
    pub fn open(dir: impl Into<PathBuf>, max_size: u64) -> std::io::Result<Wal> {
        let dir: PathBuf = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Wal {
            dir,
            max_size,
            lock: Mutex::new(()),
            seq: AtomicU32::new(0),
        })
    }

    fn new_path(&self) -> PathBuf {
        let now = crate::entry::now_millis();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.dir
            .join(format!("{now:020}-{seq:04}.{}", default_ext()))
    }

    /// Append a batch to the WAL. Returns the path of the written file.
    pub fn write(&self, batch: &CompressedBatch) -> std::io::Result<PathBuf> {
        let _guard = self.lock.lock();
        let frame = WalFrame {
            compressed: batch.payload.compressed,
            bytes: batch.payload.bytes.clone(),
        };
        let line = frame.encode();
        let mut line_bytes = line.into_bytes();
        line_bytes.push(b'\n');

        let path = self.new_path();
        std::fs::write(&path, line_bytes)?;
        self.enforce_cap_locked();
        Ok(path)
    }

    /// Directory where WAL files live.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn list_locked(&self) -> std::io::Result<Vec<PathBuf>> {
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e == default_ext())
                .unwrap_or(false)
            {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }

    fn total_size_locked(&self) -> std::io::Result<u64> {
        let mut total = 0u64;
        for path in self.list_locked()? {
            total += std::fs::metadata(&path)?.len();
        }
        Ok(total)
    }

    fn enforce_cap_locked(&self) {
        // Delete oldest (FIFO) until the total size is under `max_size`.
        while let (Ok(total), Ok(files)) = (self.total_size_locked(), self.list_locked()) {
            if total <= self.max_size || files.is_empty() {
                break;
            }
            let oldest = files[0].clone();
            let _ = std::fs::remove_file(&oldest);
        }
    }

    /// Enforce the size cap from outside (after external writes).
    pub fn enforce_cap(&self) {
        let _guard = self.lock.lock();
        self.enforce_cap_locked();
    }

    /// All pending batches, oldest first. The `entry_count` hint is 0 because
    /// the frame does not persist the count; callers may reconstruct entries
    /// via [`WalFrame::to_compressed_batch`] / decompression when needed.
    pub fn pending(&self) -> std::io::Result<Vec<(PathBuf, WalFrame)>> {
        let _guard = self.lock.lock();
        let mut out = Vec::new();
        for path in self.list_locked()? {
            if let Ok(line) = std::fs::read_to_string(&path) {
                if let Some(frame) = line.trim().split('\n').next().and_then(WalFrame::decode) {
                    out.push((path, frame));
                    continue;
                }
            }
            // Unreadable/corrupt: still return it so the caller can try to
            // remove it rather than blocking forever.
            out.push((
                path.clone(),
                WalFrame {
                    compressed: false,
                    bytes: Vec::new(),
                },
            ));
        }
        Ok(out)
    }

    /// Remove a successfully delivered batch file.
    pub fn remove(&self, path: &Path) -> std::io::Result<()> {
        let _guard = self.lock.lock();
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    /// Number of pending batches.
    pub fn len(&self) -> usize {
        self.pending().map(|p| p.len()).unwrap_or(0)
    }

    /// Whether the WAL has no pending batches.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total bytes occupied by WAL files.
    pub fn size_bytes(&self) -> u64 {
        let _guard = self.lock.lock();
        self.list_locked()
            .map(|files| {
                files
                    .iter()
                    .map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0)
    }
}

/// Convenience wrapper around a shared `Wal`.
#[derive(Clone)]
pub struct SharedWal(pub Arc<Wal>);

impl SharedWal {
    pub fn open(dir: impl Into<PathBuf>, max_size: u64) -> std::io::Result<SharedWal> {
        Ok(SharedWal(Arc::new(Wal::open(dir, max_size)?)))
    }
    pub fn inner(&self) -> &Wal {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Batch;

    fn sample_batch(n: usize) -> CompressedBatch {
        let entries: Vec<_> = (0..n)
            .map(|i| {
                crate::LogEntry::new(
                    crate::LogLevel::Info,
                    format!("line {i} with repetition padding padding"),
                )
            })
            .collect();
        let b = Batch { entries };
        crate::Compressor::new(3, false).compress_batch(&b, b.len())
    }

    #[test]
    fn write_and_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = Wal::open(tmp.path().join("wal"), 1_000_000).unwrap();
        let batch = sample_batch(3);
        for _ in 0..5 {
            wal.write(&batch).unwrap();
        }
        assert_eq!(wal.len(), 5);
        let pending = wal.pending().unwrap();
        let decoded = pending[0]
            .1
            .to_compressed_batch()
            .decompress_entries()
            .unwrap();
        assert_eq!(decoded[0].message, "line 0 with repetition padding padding");

        wal.remove(&pending[0].0).unwrap();
        assert_eq!(wal.len(), 4);
    }

    #[test]
    fn fifo_cap_drops_oldest() {
        let tmp = tempfile::tempdir().unwrap();
        // Cap tiny: single batch file ~ 200 bytes.
        let wal = Wal::open(tmp.path().join("wal"), 100).unwrap();
        let batch = sample_batch(1);
        for _ in 0..6 {
            wal.write(&batch).unwrap();
        }
        // Cap 100 => expect ~0-1 files surviving.
        assert!(wal.len() <= 1, "len={}", wal.len());
    }

    #[test]
    fn frame_b64_robust() {
        let f = WalFrame {
            compressed: true,
            bytes: vec![1, 2, 3, 250, 251],
        };
        let enc = f.encode();
        let dec = WalFrame::decode(&enc).unwrap();
        assert!(dec.compressed);
        assert_eq!(dec.bytes, f.bytes);
    }
}
