//! Data-loss paths in the WAL must be reported, never silent. Kept in its own
//! test binary because the diagnostics handler is process-global.

use std::sync::{Arc, Mutex};

use tpt_log_core::batch::{Batch, Compressor};
use tpt_log_core::{LogEntry, LogLevel, Wal};

fn batch() -> tpt_log_core::CompressedBatch {
    let entries = vec![LogEntry::new(LogLevel::Info, "payload for the wal")];
    Compressor::new(3, false).compress_batch(&Batch { entries }, 1)
}

#[test]
fn cap_overflow_and_dead_lettering_are_reported() {
    let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let sink = seen.clone();
    tpt_log_core::set_on_error(Some(Arc::new(move |kind, msg| {
        sink.lock()
            .unwrap()
            .push((kind.to_string(), msg.to_string()));
    })));

    // 1. A tiny cap forces the oldest batches to be discarded.
    let tmp = tempfile::tempdir().unwrap();
    let small = Wal::open(tmp.path().join("small"), 100).unwrap();
    for _ in 0..4 {
        small.write(&batch()).unwrap();
    }

    // 2. A batch that keeps failing replay is dead-lettered.
    let roomy = Wal::open(tmp.path().join("roomy"), 1_000_000).unwrap();
    let path = roomy.write(&batch()).unwrap();
    let dead = roomy.mark_failed(&path, 1).unwrap();
    assert!(dead.to_string_lossy().contains("dead"), "{dead:?}");

    tpt_log_core::set_on_error(None);
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter().any(|(k, m)| k == "wal"
            && m.contains("over its 100 byte cap")
            && m.contains("discarded")),
        "cap overflow not reported: {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|(k, m)| k == "wal" && m.contains("dead-lettered")),
        "dead-lettering not reported: {seen:?}"
    );
}
