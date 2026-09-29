use crate::LogEntry;
use std::sync::{Arc, Mutex};

/// Bounded multi-producer, single-consumer ingestion queue.
///
/// Backed by a lock-free `crossbeam-channel`. Producers blocks when full —
/// this provides flow control without ever dropping a log (spec §2.4).
pub struct LogQueue {
    tx: crossbeam_channel::Sender<Arc<LogEntry>>,
    rx: crossbeam_channel::Receiver<Arc<LogEntry>>,
    capacity: usize,
    /// Approximate bytes currently buffered (for memory-based batching).
    buffered: Mutex<usize>,
}

impl LogQueue {
    /// Create a bounded queue with the given capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, rx) = crossbeam_channel::bounded(capacity.max(1));
        Self {
            tx,
            rx,
            capacity: capacity.max(1),
            buffered: Mutex::new(0),
        }
    }

    /// Push an entry, blocking while the queue is full (guaranteed delivery).
    pub fn push(&self, entry: LogEntry) {
        let len = entry.approx_size();
        *self.buffered.lock().unwrap() += len;
        // `send` blocks until space is available; never drops.
        let _ = self.tx.send(Arc::new(entry));
    }

    /// Non-blocking push. Returns `false` if the queue is full.
    pub fn try_push(&self, entry: LogEntry) -> bool {
        let len = entry.approx_size();
        if self.tx.try_send(Arc::new(entry)).is_ok() {
            *self.buffered.lock().unwrap() += len;
            true
        } else {
            false
        }
    }

    /// Blocking receive; `None` when the producer side is disconnected/closed.
    pub fn pop(&self) -> Option<Arc<LogEntry>> {
        match self.rx.recv() {
            Ok(entry) => {
                let len = entry.approx_size();
                let mut buf = self.buffered.lock().unwrap();
                *buf = buf.saturating_sub(len);
                Some(entry)
            }
            Err(_) => None,
        }
    }

    /// Non-blocking receive of as many entries as currently available.
    pub fn drain_available(&self) -> Vec<Arc<LogEntry>> {
        let mut out = Vec::new();
        while let Ok(entry) = self.rx.try_recv() {
            let len = entry.approx_size();
            let mut buf = self.buffered.lock().unwrap();
            *buf = buf.saturating_sub(len);
            drop(buf);
            out.push(entry);
        }
        out
    }

    /// Receiver side of the channel (for select loops in the batcher).
    pub(crate) fn receiver(&self) -> crossbeam_channel::Receiver<Arc<LogEntry>> {
        self.rx.clone()
    }

    /// Approximate bytes still buffered in this queue.
    pub fn buffered_bytes(&self) -> usize {
        *self.buffered.lock().unwrap()
    }

    /// Queue capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of entries currently queued.
    pub fn len(&self) -> usize {
        self.rx.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.rx.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LogLevel;

    #[test]
    fn push_and_pop() {
        let q = LogQueue::new(10);
        q.push(LogEntry::new(LogLevel::Info, "a"));
        q.push(LogEntry::new(LogLevel::Info, "b"));
        assert_eq!(q.len(), 2);
        let a = q.pop().unwrap();
        assert_eq!(a.message, "a");
        assert_eq!(q.len(), 1);
        assert!(q.buffered_bytes() > 0);
    }

    #[test]
    fn drain_available() {
        let q = LogQueue::new(10);
        for i in 0..5 {
            q.push(LogEntry::new(LogLevel::Debug, format!("e{i}")));
        }
        let drained = q.drain_available();
        assert_eq!(drained.len(), 5);
        assert!(q.is_empty());
    }

    #[test]
    fn bounded_backpressure() {
        let q = LogQueue::new(2);
        // Use try_push so we don't block; queue of 2 should reject the 3rd.
        assert!(q.try_push(LogEntry::new(LogLevel::Info, "1")));
        assert!(q.try_push(LogEntry::new(LogLevel::Info, "2")));
        assert!(!q.try_push(LogEntry::new(LogLevel::Info, "3")));
    }
}
