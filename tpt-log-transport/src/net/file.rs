//! File transport — JSONL append with rotation (spec §5.3).
//!
//! Each log entry is written as one JSON line. Rotation is triggered when the
//! active file exceeds `file_rotation_bytes` or has been open longer than
//! `file_rotation_seconds`, keeping up to `file_rotations` rotated files
//! (`file`, `file.1`, `file.2`, … oldest dropped).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tpt_log_core::{Config, Transport, TransportError};

/// JSONL file transport.
pub struct FileTransport {
    path: PathBuf,
    rotation_bytes: u64,
    rotation_seconds: Option<u64>,
    rotations: usize,
    state: std::sync::Mutex<FileState>,
}

struct FileState {
    file: Option<File>,
    created: Instant,
    size: u64,
}

impl std::fmt::Debug for FileTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileTransport")
            .field("path", &self.path)
            .finish()
    }
}

impl FileTransport {
    /// Create a file transport writing JSONL to `path`.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, TransportError> {
        let path = path.into();
        Self::with_rotation(path, 10 * 1024 * 1024, None, 3)
    }

    /// Create with explicit rotation policy.
    pub fn with_rotation(
        path: impl Into<PathBuf>,
        rotation_bytes: u64,
        rotation_seconds: Option<u64>,
        rotations: usize,
    ) -> Result<Self, TransportError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    TransportError::Config(format!("create parent for {:?}: {e}", path))
                })?;
            }
        }
        let existing_size = std::fs::metadata(&path)
            .map(|m| m.len())
            .unwrap_or_default();
        Ok(Self {
            path,
            rotation_bytes,
            rotation_seconds,
            rotations,
            state: std::sync::Mutex::new(FileState {
                file: None,
                created: Instant::now(),
                size: existing_size,
            }),
        })
    }

    /// Build from core config (`file_path`, `file_rotation_bytes`,
    /// `file_rotation_seconds`, `file_rotations`). Errors when `file_path` is
    /// unset.
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let path = config
            .file_path
            .clone()
            .ok_or_else(|| TransportError::Config("file backend needs `file_path`".into()))?;
        let rotations = config.file_rotations.clamp(2, 99);
        Self::with_rotation(
            path,
            config.file_rotation_bytes.unwrap_or(10 * 1024 * 1024),
            config.file_rotation_seconds,
            rotations,
        )
    }

    fn current_file(&self) -> Result<std::fs::File, TransportError> {
        let mut state = self.state.lock().expect("file transport mutex poisoned");
        if let Some(file) = state.file.as_ref() {
            return Ok(file.try_clone().expect("clone file handle"));
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| TransportError::Transient(format!("open {:?}: {e}", self.path)))?;
        let cloned = file.try_clone().expect("clone file handle");
        state.file = Some(file);
        state.size = std::fs::metadata(&self.path)
            .map(|m| m.len())
            .unwrap_or_default();
        state.created = Instant::now();
        Ok(cloned)
    }

    fn maybe_rotate(&self) -> Result<(), TransportError> {
        let mut state = self.state.lock().expect("file transport mutex poisoned");
        let elapsed = state.created.elapsed();
        let too_big = state.size >= self.rotation_bytes;
        let too_old = self
            .rotation_seconds
            .map(|secs| elapsed >= Duration::from_secs(secs))
            .unwrap_or(false);
        if !too_big && !too_old {
            return Ok(());
        }
        if let Some(file) = state.file.as_ref() {
            let _ = file.sync_all();
        }
        drop(state.file.take());
        rotate(&self.path, self.rotations)
            .map_err(|e| TransportError::Transient(format!("rotate failed: {e}")))?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| TransportError::Transient(format!("reopen {:?}: {e}", self.path)))?;
        state.file = Some(file);
        state.created = Instant::now();
        state.size = 0;
        Ok(())
    }
}

/// Shift `path` → `path.1` → `path.2` … dropping names beyond `keep`.
fn rotate(path: &Path, keep: usize) -> Result<(), String> {
    if keep == 0 {
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    let mut n = keep.saturating_sub(1);
    while n > 0 {
        let from = numbered(path, n);
        let to = numbered(path, n + 1);
        if from.exists() {
            if to.exists() {
                let _ = std::fs::remove_file(&to);
            }
            std::fs::rename(&from, &to).map_err(|e| format!("rename {:?}: {e}", to))?;
        }
        n -= 1;
    }
    let first = numbered(path, 1);
    if first.exists() {
        let _ = std::fs::remove_file(&first);
    }
    std::fs::rename(path, &first).map_err(|e| format!("rename {:?}: {e}", first))?;
    Ok(())
}

fn numbered(path: &Path, n: usize) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{n}"));
    path.with_file_name(name)
}

impl Transport for FileTransport {
    fn send(&self, batch: &tpt_log_core::CompressedBatch) -> Result<(), TransportError> {
        let entries = batch
            .decompress_entries()
            .map_err(|e| TransportError::Rejected(format!("decode failed: {e}")))?;
        if entries.is_empty() {
            return Ok(());
        }
        self.maybe_rotate()?;
        let mut file = self.current_file()?;
        let mut bytes_written: u64 = 0;
        for entry in &entries {
            let mut line =
                serde_json::to_vec(entry).map_err(|e| TransportError::Rejected(e.to_string()))?;
            line.push(b'\n');
            file.write_all(&line)
                .map_err(|e| TransportError::Transient(format!("write {:?}: {e}", self.path)))?;
            bytes_written += line.len() as u64;
        }
        {
            let mut state = self.state.lock().expect("file transport mutex poisoned");
            state.size += bytes_written;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "file"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tpt_log_core::MicroLog;

    /// Full pipeline against the `file` backend; `rotation_bytes` controls how
    /// aggressively the file rotates.
    fn logger(dir: &Path, rotation_bytes: u64, rotations: usize) -> MicroLog {
        let cfg = Config {
            backend: "file".into(),
            file_path: Some(dir.join("logs.jsonl").to_string_lossy().into_owned()),
            file_rotation_bytes: Some(rotation_bytes),
            file_rotations: rotations,
            batch_size: 1,
            flush_interval_ms: 20,
            wal_enabled: false,
            ..Config::default()
        };
        let transport = crate::build_transport(&cfg).unwrap();
        MicroLog::new(cfg, std::sync::Arc::from(transport))
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn writes_jsonl_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs.jsonl");
        let ml = logger(dir.path(), 10 * 1024 * 1024, 3);
        ml.info("hello");
        ml.debug("world");
        ml.shutdown();
        let mut contents = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "contents: {contents}");
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["level"], "INFO");
        assert_eq!(first["message"], "hello");
    }

    #[test]
    fn rotates_on_size_and_shifts() {
        let dir = tempfile::tempdir().unwrap();
        let transport =
            FileTransport::with_rotation(dir.path().join("logs.jsonl"), 64, None, 3).unwrap();
        for i in 0..40 {
            transport
                .send(&one_entry_batch(&format!("message {i}")))
                .unwrap();
        }
        let files = file_names(dir.path());
        assert!(
            files.contains(&"logs.jsonl".to_string())
                && files.contains(&"logs.jsonl.1".to_string())
                && files.contains(&"logs.jsonl.3".to_string()),
            "files after rotation: {files:?}"
        );
        // Re-open the oldest surviving rotated file to confirm JSONL intact.
        let mut contents = String::new();
        std::fs::File::open(dir.path().join("logs.jsonl.3"))
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert!(!contents.trim().is_empty());
    }

    #[test]
    fn rotation_caps_at_keep() {
        let dir = tempfile::tempdir().unwrap();
        let transport =
            FileTransport::with_rotation(dir.path().join("logs.jsonl"), 64, None, 2).unwrap();
        for i in 0..80 {
            transport
                .send(&one_entry_batch(&format!("message {i}")))
                .unwrap();
        }
        let names = file_names(dir.path());
        assert!(
            !names.iter().any(|n| n == "logs.jsonl.3"),
            "names: {names:?}"
        );
    }

    /// A `CompressedBatch` holding a single INFO entry (rotation test fuel).
    fn one_entry_batch(message: &str) -> tpt_log_core::CompressedBatch {
        use tpt_log_core::batch::CompressedBatchPayload;
        use tpt_log_core::{Batch, CompressedBatch, Compressor, LogEntry, LogLevel};
        let batch = Batch {
            entries: vec![LogEntry::new(LogLevel::Info, message.to_string())],
        };
        let compressor = Compressor::from_config(&Config::default());
        let bytes = compressor.compress(&batch.to_json());
        CompressedBatch {
            payload: CompressedBatchPayload {
                bytes,
                entry_count: 1,
                compressed: true,
            },
        }
    }
}
