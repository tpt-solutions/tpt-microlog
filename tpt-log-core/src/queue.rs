use crate::config::QueueFullPolicy;
use crate::LogEntry;

/// Result of a [`LogQueue::push`] under the configured overflow policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    /// Entry is buffered.
    Accepted,
    /// Entry was dropped (`drop_newest` with a full queue).
    Dropped,
    /// Entry is buffered, but the oldest queued entry was evicted to make
    /// room (`drop_oldest`).
    EvictedOldest,
}

/// Bounded multi-producer, single-consumer ingestion queue.
///
/// Backed by a lock-free `crossbeam-channel`. What happens when the queue is
/// full is decided by [`QueueFullPolicy`] (spec §2.4):
///
/// - [`QueueFullPolicy::Block`] applies backpressure to the producer (never
///   drops a log, but can stall the calling thread),
/// - [`QueueFullPolicy::DropNewest`] drops the entry being pushed,
/// - [`QueueFullPolicy::DropOldest`] evicts the oldest buffered entry to make
///   room, keeping the freshest telemetry.
pub struct LogQueue {
    tx: crossbeam_channel::Sender<LogEntry>,
    rx: crossbeam_channel::Receiver<LogEntry>,
    capacity: usize,
    policy: QueueFullPolicy,
}

impl LogQueue {
    /// Create a bounded queue with the given capacity and overflow policy.
    pub fn new(capacity: usize, policy: QueueFullPolicy) -> Self {
        let (tx, rx) = crossbeam_channel::bounded(capacity.max(1));
        Self {
            tx,
            rx,
            capacity: capacity.max(1),
            policy,
        }
    }

    /// Push an entry per the configured overflow policy.
    pub fn push(&self, entry: LogEntry) -> PushOutcome {
        match self.policy {
            QueueFullPolicy::Block => {
                // `send` blocks until space is available; never drops.
                self.tx
                    .send(entry)
                    .map(|_| PushOutcome::Accepted)
                    .unwrap_or(PushOutcome::Dropped)
            }
            QueueFullPolicy::DropNewest => match self.tx.try_send(entry) {
                Ok(()) => PushOutcome::Accepted,
                Err(crossbeam_channel::TrySendError::Full(_)) => PushOutcome::Dropped,
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => PushOutcome::Dropped,
            },
            QueueFullPolicy::DropOldest => match self.tx.try_send(entry) {
                Ok(()) => PushOutcome::Accepted,
                Err(crossbeam_channel::TrySendError::Full(entry)) => {
                    // Evict the oldest entry, then retry once.
                    match self.rx.recv_timeout(std::time::Duration::from_millis(50)) {
                        Ok(_) => match self.tx.try_send(entry) {
                            Ok(()) => PushOutcome::EvictedOldest,
                            _ => PushOutcome::Dropped,
                        },
                        Err(_) => PushOutcome::Dropped,
                    }
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => PushOutcome::Dropped,
            },
        }
    }

    /// Non-blocking push ignoring policy. Returns `false` if the queue is full.
    pub fn try_push(&self, entry: LogEntry) -> bool {
        self.tx.try_send(entry).is_ok()
    }

    /// Blocking receive; `None` when the producer side is disconnected/closed.
    pub fn pop(&self) -> Option<LogEntry> {
        self.rx.recv().ok()
    }

    /// Receiver side of the channel (for select loops in the batcher).
    pub(crate) fn receiver(&self) -> crossbeam_channel::Receiver<LogEntry> {
        self.rx.clone()
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
        let q = LogQueue::new(10, QueueFullPolicy::Block);
        assert_eq!(
            q.push(LogEntry::new(LogLevel::Info, "a")),
            PushOutcome::Accepted
        );
        assert_eq!(
            q.push(LogEntry::new(LogLevel::Info, "b")),
            PushOutcome::Accepted
        );
        assert_eq!(q.len(), 2);
        let a = q.pop().unwrap();
        assert_eq!(a.message, "a");
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn bounded_backpressure() {
        let q = LogQueue::new(2, QueueFullPolicy::Block);
        assert!(q.try_push(LogEntry::new(LogLevel::Info, "1")));
        assert!(q.try_push(LogEntry::new(LogLevel::Info, "2")));
        assert!(!q.try_push(LogEntry::new(LogLevel::Info, "3")));
    }

    #[test]
    fn drop_newest_rejects_when_full() {
        let q = LogQueue::new(2, QueueFullPolicy::DropNewest);
        assert_eq!(
            q.push(LogEntry::new(LogLevel::Info, "1")),
            PushOutcome::Accepted
        );
        assert_eq!(
            q.push(LogEntry::new(LogLevel::Info, "2")),
            PushOutcome::Accepted
        );
        assert_eq!(
            q.push(LogEntry::new(LogLevel::Info, "3")),
            PushOutcome::Dropped
        );
        assert_eq!(q.pop().unwrap().message, "1");
    }

    #[test]
    fn drop_oldest_keeps_newest() {
        let q = LogQueue::new(2, QueueFullPolicy::DropOldest);
        assert_eq!(
            q.push(LogEntry::new(LogLevel::Info, "1")),
            PushOutcome::Accepted
        );
        assert_eq!(
            q.push(LogEntry::new(LogLevel::Info, "2")),
            PushOutcome::Accepted
        );
        assert_eq!(
            q.push(LogEntry::new(LogLevel::Info, "3")),
            PushOutcome::EvictedOldest
        );
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop().unwrap().message, "2");
        assert_eq!(q.pop().unwrap().message, "3");
    }
}
