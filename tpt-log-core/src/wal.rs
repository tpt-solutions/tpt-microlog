use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use parking_lot::Mutex;

use crate::CompressedBatch;

/// One persisted batch frame (spec §5.4).
///
/// File format: one batch per file, a single line of base64 over
/// `<marker><entry_count: u32 BE><payload>`:
/// - marker `Z` — payload is a zstd frame,
/// - marker `R` — payload is raw JSON (fast mode).
///
/// Legacy frames (marker `1` = compressed, `0` = raw, no entry count) still
/// decode with `entry_count` 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalFrame {
    pub compressed: bool,
    pub bytes: Vec<u8>,
    /// Number of log entries in the payload (0 = unknown/legacy frame).
    pub entry_count: u32,
}

const MARKER_COMPRESSED: u8 = b'Z';
const MARKER_RAW: u8 = b'R';
/// Legacy single-byte markers from before entry counts were persisted.
const LEGACY_COMPRESSED: u8 = 1;
const LEGACY_RAW: u8 = 0;

impl WalFrame {
    pub fn encode(&self) -> String {
        let mut buf = Vec::with_capacity(self.bytes.len() + 5);
        buf.push(if self.compressed {
            MARKER_COMPRESSED
        } else {
            MARKER_RAW
        });
        buf.extend_from_slice(&self.entry_count.to_be_bytes());
        buf.extend_from_slice(&self.bytes);
        B64.encode(buf)
    }

    pub fn decode(line: &str) -> Option<WalFrame> {
        let raw = B64.decode(line.trim()).ok()?;
        if raw.is_empty() {
            return None;
        }
        match raw[0] {
            MARKER_COMPRESSED | MARKER_RAW => {
                if raw.len() < 5 {
                    return None;
                }
                let entry_count = u32::from_be_bytes([raw[1], raw[2], raw[3], raw[4]]);
                Some(WalFrame {
                    compressed: raw[0] == MARKER_COMPRESSED,
                    bytes: raw[5..].to_vec(),
                    entry_count,
                })
            }
            LEGACY_COMPRESSED => Some(WalFrame {
                compressed: true,
                bytes: raw[1..].to_vec(),
                entry_count: 0,
            }),
            LEGACY_RAW => Some(WalFrame {
                compressed: false,
                bytes: raw[1..].to_vec(),
                entry_count: 0,
            }),
            _ => None,
        }
    }

    /// An empty frame used as a sentinel for corrupt/unreadable files.
    fn corrupt() -> WalFrame {
        WalFrame {
            compressed: false,
            bytes: Vec::new(),
            entry_count: 0,
        }
    }

    /// Parse an encoded frame back into a [`CompressedBatch`].
    pub fn to_compressed_batch(&self) -> CompressedBatch {
        CompressedBatch {
            payload: crate::batch::CompressedBatchPayload {
                bytes: self.bytes.clone(),
                entry_count: self.entry_count as usize,
                compressed: self.compressed,
                raw_json: None,
            },
        }
    }
}

fn default_ext() -> &'static str {
    "wal"
}

/// Name of the advisory lock file guarding a WAL directory.
const LOCK_FILE_NAME: &str = "tpt-microlog.lock";

/// Subdirectory holding batches that exceeded [`Config::wal_dead_letter_replays`]
/// failed replay passes.
const DEAD_DIR_NAME: &str = "dead";

/// Number of hex characters in the per-config namespace suffix.
const NAMESPACE_HEX: usize = 12;

/// Compute the WAL subdirectory namespace for a given logger identity
/// (`service` + `backend` + `endpoint`). Two processes logging for different
/// services/backends never share (or cross-deliver) WAL files.
pub fn namespace(service: Option<&str>, backend: &str, endpoint: Option<&str>) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    service.unwrap_or("").hash(&mut hasher);
    backend.hash(&mut hasher);
    endpoint.unwrap_or("").hash(&mut hasher);
    let hash = hasher.finish();
    let sanitized: String = service
        .unwrap_or("default")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect();
    format!("{sanitized}-{hash:0NAMESPACE_HEX$X}")
}

/// Write-ahead log: append-only persistence for batches that could not be
/// delivered (spec §5.4). One file per batch keeps delivery deletion atomic and
/// the FIFO size cap trivial (delete the oldest files).
///
/// Each directory is guarded by an advisory lock file: a second process
/// opening the same WAL namespace cannot acquire it and must not replay (or
/// otherwise consume) batches it does not own. Writes go through a temp file
/// plus atomic rename so a concurrent reader never sees a torn batch.
pub struct Wal {
    dir: PathBuf,
    max_size: u64,
    lock: Mutex<()>,
    seq: AtomicU32,
    /// O(1) running total of pending WAL bytes (guarded by `lock` writes).
    total_bytes: AtomicU64,
    /// Holds the OS-level advisory lock until [`Wal::release_lock`] or drop.
    lock_file: Mutex<Option<std::fs::File>>,
}

impl Wal {
    /// Open (creating if needed) the WAL directory and acquire its advisory
    /// lock. Fails when another live process holds the lock.
    pub fn open(dir: impl Into<PathBuf>, max_size: u64) -> std::io::Result<Wal> {
        let dir: PathBuf = dir.into();
        std::fs::create_dir_all(&dir)?;
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK_FILE_NAME))?;
        lock_file.try_lock().map_err(|e| match e {
            std::fs::TryLockError::WouldBlock => std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "WAL directory {} is locked by another process",
                    dir.display()
                ),
            ),
            std::fs::TryLockError::Error(e) => e,
        })?;
        let total = dir_byte_size(&dir);
        Ok(Wal {
            dir,
            max_size,
            lock: Mutex::new(()),
            seq: AtomicU32::new(0),
            total_bytes: AtomicU64::new(total),
            lock_file: Mutex::new(Some(lock_file)),
        })
    }

    /// Release the directory lock early so another instance (e.g. a logger
    /// recreated after `shutdown()`) can open the same WAL. Call only once
    /// nothing will write through this instance again. Idempotent.
    pub fn release_lock(&self) {
        if let Some(file) = self.lock_file.lock().take() {
            let _ = file.unlock();
        }
    }

    fn new_path(&self) -> PathBuf {
        let now = crate::entry::now_millis();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let rand_hex: u32 = {
            use rand::Rng;
            rand::thread_rng().gen()
        };
        self.dir.join(format!(
            "{now:020}-{pid}-{seq:04}-{rand_hex:08x}.{}",
            default_ext()
        ))
    }

    /// Directory where WAL files live.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Directory where dead-lettered batches live.
    pub fn dead_dir(&self) -> PathBuf {
        self.dir.join(DEAD_DIR_NAME)
    }

    /// Append a batch to the WAL. Returns the path of the written file.
    ///
    /// The line is written to a temp file and atomically renamed into place;
    /// readers never observe partial batches.
    pub fn write(&self, batch: &CompressedBatch) -> std::io::Result<PathBuf> {
        let _guard = self.lock.lock();
        let frame = WalFrame {
            compressed: batch.payload.compressed,
            bytes: batch.payload.bytes.clone(),
            entry_count: batch.payload.entry_count as u32,
        };
        let mut line_bytes = frame.encode().into_bytes();
        line_bytes.push(b'\n');

        let path = self.new_path();
        let tmp = path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&line_bytes)?;
            f.sync_all().ok();
        }
        std::fs::rename(&tmp, &path)?;
        self.total_bytes
            .fetch_add(line_bytes.len() as u64, Ordering::Relaxed);
        self.enforce_cap_locked();
        Ok(path)
    }

    /// Pending batch paths, oldest first. Cheap: one directory listing, no
    /// file contents are read. Consumers stream files one at a time via
    /// [`Wal::read_frame`].
    pub fn pending_paths(&self) -> std::io::Result<Vec<PathBuf>> {
        let _guard = self.lock.lock();
        self.list_locked()
    }

    /// Read and decode a single WAL file. `Err` means the file is unreadable
    /// or corrupt; the caller should remove or dead-letter it.
    pub fn read_frame(&self, path: &Path) -> std::io::Result<WalFrame> {
        let _guard = self.lock.lock();
        read_frame_unlocked(path)
    }

    /// All pending batches with their frames (convenience for tests/tools).
    pub fn pending(&self) -> std::io::Result<Vec<(PathBuf, WalFrame)>> {
        let paths = self.pending_paths()?;
        let mut out = Vec::with_capacity(paths.len());
        for path in paths {
            let frame = self
                .read_frame(&path)
                .unwrap_or_else(|_| WalFrame::corrupt());
            out.push((path, frame));
        }
        Ok(out)
    }

    /// Record a failed replay pass for `path`: the file is renamed with an
    /// incremented attempt counter, and moved into `wal/dead/` once it has
    /// failed `max_replays` times.
    pub fn mark_failed(&self, path: &Path, max_replays: u32) -> std::io::Result<PathBuf> {
        let _guard = self.lock.lock();
        let attempts = attempt_count(path) + 1;
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let target = if attempts >= max_replays.max(1) {
            let dead = self.dead_dir();
            std::fs::create_dir_all(&dead)?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("dead.{}", default_ext()));
            dead.join(name)
        } else {
            with_attempt_count(path, attempts)
        };
        // Never clobber an existing dead-letter file from a previous run.
        let target = if target.exists() {
            let stem = target
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "batch".to_string());
            let dup = target.parent().unwrap_or(Path::new(".")).join(format!(
                "{stem}-{}.{}",
                crate::entry::now_millis(),
                default_ext()
            ));
            dup
        } else {
            target
        };
        std::fs::rename(path, &target)?;
        if attempts >= max_replays.max(1) {
            // Moved out of the pending set; stop counting its bytes.
            self.total_bytes.fetch_sub(size, Ordering::Relaxed);
            crate::diag::report(
                "wal",
                &format!(
                    "batch dead-lettered after {attempts} failed replays (kept for inspection): {}",
                    target.display()
                ),
            );
        }
        Ok(target)
    }

    /// Remove a successfully delivered batch file.
    pub fn remove(&self, path: &Path) -> std::io::Result<()> {
        let _guard = self.lock.lock();
        if path.exists() {
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            std::fs::remove_file(path)?;
            self.total_bytes.fetch_sub(size, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Number of pending batches.
    pub fn len(&self) -> usize {
        self.pending_paths().map(|p| p.len()).unwrap_or(0)
    }

    /// Whether the WAL has no pending batches.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total bytes occupied by pending WAL files (O(1) cached accounting).
    pub fn size_bytes(&self) -> u64 {
        self.total_bytes.load(Ordering::Relaxed)
    }

    fn list_locked(&self) -> std::io::Result<Vec<PathBuf>> {
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
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

    fn enforce_cap_locked(&self) {
        // Delete oldest (FIFO) until the total size is under `max_size`.
        let (mut dropped_files, mut dropped_bytes) = (0u64, 0u64);
        while self.total_bytes.load(Ordering::Relaxed) > self.max_size {
            let Ok(files) = self.list_locked() else {
                break;
            };
            let Some(oldest) = files.first() else {
                break;
            };
            let size = std::fs::metadata(oldest).map(|m| m.len()).unwrap_or(0);
            if std::fs::remove_file(oldest).is_ok() {
                self.total_bytes.fetch_sub(size, Ordering::Relaxed);
                dropped_files += 1;
                dropped_bytes += size;
            } else {
                break; // avoid spinning on an undeletable file
            }
        }
        if dropped_files > 0 {
            // Undelivered data is being discarded: never do that silently.
            crate::diag::report(
                "wal",
                &format!(
                    "WAL over its {} byte cap: discarded the {dropped_files} oldest undelivered batch(es) ({dropped_bytes} bytes)",
                    self.max_size
                ),
            );
        }
    }

    /// Enforce the size cap from outside (after external writes).
    pub fn enforce_cap(&self) {
        let _guard = self.lock.lock();
        self.enforce_cap_locked();
    }
}

/// Sum of the sizes of all pending WAL files in `dir` (one scan at open).
fn dir_byte_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e == default_ext())
                    .unwrap_or(false)
            {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

fn read_frame_unlocked(path: &Path) -> std::io::Result<WalFrame> {
    let line = std::fs::read_to_string(path)?;
    let frame = line
        .trim()
        .split('\n')
        .next()
        .and_then(WalFrame::decode)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("corrupt WAL frame: {}", path.display()),
            )
        })?;
    Ok(frame)
}

/// Longest attempt counter accepted in a file stem. The unique suffix of a
/// WAL file name is 8 hex characters, so a `-a<digits>` marker of at most
/// this many digits can never be confused with a suffix that merely starts
/// with `a` and is otherwise numeric (e.g. `…-a8125429`).
const MAX_ATTEMPT_DIGITS: usize = 6;

/// Split a stem into `(base, attempts)`; `attempts` is `None` when the stem
/// carries no attempt marker.
fn split_attempt(stem: &str) -> (&str, Option<u32>) {
    if let Some((base, n)) = stem.rsplit_once("-a") {
        if (1..=MAX_ATTEMPT_DIGITS).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(attempts) = n.parse::<u32>() {
                return (base, Some(attempts));
            }
        }
    }
    (stem, None)
}

/// Attempt counter carried in the file stem: `…-a3.wal` has failed 3 times.
fn attempt_count(path: &Path) -> u32 {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|stem| split_attempt(stem).1)
        .unwrap_or(0)
}

/// Rewrite `path`'s stem with the given attempt counter.
fn with_attempt_count(path: &Path, attempts: u32) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "batch".to_string());
    let base = split_attempt(&stem).0;
    path.with_file_name(format!("{base}-a{attempts}.{}", default_ext()))
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
        let (path, frame) = &pending[0];
        assert_eq!(frame.entry_count, 3, "entry count persisted");
        let decoded = frame.to_compressed_batch().decompress_entries().unwrap();
        assert_eq!(decoded[0].message, "line 0 with repetition padding padding");

        wal.remove(path).unwrap();
        assert_eq!(wal.len(), 4);
    }

    #[test]
    fn legacy_frames_still_decode() {
        let line = B64.encode([LEGACY_COMPRESSED, 1, 2, 3]);
        let f = WalFrame::decode(&line).unwrap();
        assert!(f.compressed);
        assert_eq!(f.entry_count, 0);
        let line = B64.encode([LEGACY_RAW, 9]);
        let f = WalFrame::decode(&line).unwrap();
        assert!(!f.compressed);
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
        assert!(wal.size_bytes() <= 500, "size={}", wal.size_bytes());
    }

    #[test]
    fn size_accounting_tracks_writes_and_removes() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = Wal::open(tmp.path().join("wal"), 1_000_000).unwrap();
        let batch = sample_batch(2);
        let p1 = wal.write(&batch).unwrap();
        let after_write = wal.size_bytes();
        assert!(after_write > 0);
        let p2 = wal.write(&batch).unwrap();
        let after_two = wal.size_bytes();
        assert!(after_two > after_write);
        wal.remove(&p1).unwrap();
        assert_eq!(wal.size_bytes(), after_two - after_write);
        wal.remove(&p2).unwrap();
        assert_eq!(wal.size_bytes(), 0);
    }

    #[test]
    fn frame_b64_robust() {
        let f = WalFrame {
            compressed: true,
            bytes: vec![1, 2, 3, 250, 251],
            entry_count: 42,
        };
        let enc = f.encode();
        let dec = WalFrame::decode(&enc).unwrap();
        assert!(dec.compressed);
        assert_eq!(dec.bytes, f.bytes);
        assert_eq!(dec.entry_count, 42);
    }

    /// A random unique suffix such as `a8125429` must not be mistaken for an
    /// attempt counter (it used to dead-letter a batch on its first failure).
    #[test]
    fn numeric_a_prefixed_suffix_is_not_an_attempt_counter() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = Wal::open(tmp.path().join("wal"), 1_000_000).unwrap();
        let path = wal.write(&sample_batch(1)).unwrap();
        let tricky = path.with_file_name("00000001791372245552-5656-0000-a8125429.wal");
        std::fs::rename(&path, &tricky).unwrap();

        assert_eq!(attempt_count(&tricky), 0);
        let after_one = wal.mark_failed(&tricky, 3).unwrap();
        assert!(
            after_one.to_string_lossy().ends_with("-a8125429-a1.wal"),
            "{after_one:?}"
        );
        assert_eq!(wal.len(), 1, "still pending, not dead-lettered");
        assert_eq!(attempt_count(&after_one), 1);
    }

    #[test]
    fn dead_letter_after_max_replays() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = Wal::open(tmp.path().join("wal"), 1_000_000).unwrap();
        let path = wal.write(&sample_batch(1)).unwrap();

        // Two failures with max 3: renamed with attempt counters, still pending.
        let p1 = wal.mark_failed(&path, 3).unwrap();
        assert!(p1.to_string_lossy().ends_with("-a1.wal"), "{p1:?}");
        let p2 = wal.mark_failed(&p1, 3).unwrap();
        assert!(p2.to_string_lossy().ends_with("-a2.wal"), "{p2:?}");
        assert_eq!(wal.len(), 1);

        // Third failure: dead-lettered.
        let p3 = wal.mark_failed(&p2, 3).unwrap();
        assert!(p3.starts_with(wal.dead_dir()), "{p3:?}");
        assert_eq!(wal.len(), 0);
    }

    #[test]
    fn corrupt_file_reads_as_error() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = Wal::open(tmp.path().join("wal"), 1_000_000).unwrap();
        let bad = wal.dir().join("000-bad.wal");
        std::fs::write(&bad, b"!!!not base64!!!\n").unwrap();
        assert!(wal.read_frame(&bad).is_err());
        assert_eq!(wal.len(), 1);
    }

    #[test]
    fn namespacing_separates_services() {
        let a = namespace(Some("checkout"), "datadog", None);
        let b = namespace(Some("billing"), "datadog", None);
        assert_ne!(a, b);
        assert!(a.starts_with("checkout-"));
        // Same identity → same namespace (deterministic).
        assert_eq!(
            namespace(Some("svc"), "udp", Some("h:1")),
            namespace(Some("svc"), "udp", Some("h:1"))
        );
    }

    #[test]
    fn second_instance_cannot_lock_same_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("wal");
        let _first = Wal::open(&dir, 1_000_000).unwrap();
        let second = Wal::open(&dir, 1_000_000);
        assert!(second.is_err(), "expected lock conflict");
    }
}
