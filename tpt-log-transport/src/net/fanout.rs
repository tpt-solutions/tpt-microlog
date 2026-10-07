//! Multi-backend routing: fan-out (deliver to every backend) and failover
//! (deliver through backends in order, stopping at the first success).

use tpt_log_core::{CompressedBatch, Transport, TransportError};

/// Route batches to several transports.
///
/// - fan-out mode (`failover: false`, the default): every backend receives
///   the batch; the call succeeds only when all of them accept it (a failing
///   backend's error is reported so the core persists the batch to the WAL —
///   successful backends may see a redelivery after a WAL replay, which their
///   at-least-once semantics already tolerate).
/// - failover mode (`failover: true`): backends are tried in order and the
///   first success wins; permanent errors advance to the next backend while
///   transient errors stop the pass (the core will retry the batch).
pub struct RoutingTransport {
    transports: Vec<Box<dyn Transport>>,
    failover: bool,
}

impl std::fmt::Debug for RoutingTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutingTransport")
            .field("failover", &self.failover)
            .field("backends", &self.transports.len())
            .finish()
    }
}

impl RoutingTransport {
    /// Fan the batch out to every transport.
    pub fn fanout(transports: Vec<Box<dyn Transport>>) -> Self {
        Self {
            transports,
            failover: false,
        }
    }

    /// Try the transports in order, stopping at the first success.
    pub fn failover(transports: Vec<Box<dyn Transport>>) -> Self {
        Self {
            transports,
            failover: true,
        }
    }

    /// Route according to the given mode.
    pub fn new(transports: Vec<Box<dyn Transport>>, failover: bool) -> Self {
        Self {
            transports,
            failover,
        }
    }

    /// Number of routed backends.
    pub fn len(&self) -> usize {
        self.transports.len()
    }

    /// Whether no backend is routed.
    pub fn is_empty(&self) -> bool {
        self.transports.is_empty()
    }
}

impl Transport for RoutingTransport {
    fn send(&self, batch: &CompressedBatch) -> Result<(), TransportError> {
        if self.failover {
            let mut last = TransportError::Config("fanout backend list is empty".into());
            for transport in &self.transports {
                match transport.send(batch) {
                    Ok(()) => return Ok(()),
                    // Permanent rejections are unlikely to heal: advance.
                    Err(e @ TransportError::Permanent(_)) | Err(e @ TransportError::Config(_)) => {
                        last = e;
                    }
                    Err(e) => return Err(e),
                }
            }
            Err(last)
        } else {
            if self.transports.is_empty() {
                return Err(TransportError::Config(
                    "fanout backend list is empty".into(),
                ));
            }
            let mut first_error: Option<TransportError> = None;
            for transport in &self.transports {
                if let Err(e) = transport.send(batch) {
                    first_error.get_or_insert(e);
                }
            }
            match first_error {
                None => Ok(()),
                Some(e) => Err(e),
            }
        }
    }

    fn name(&self) -> &str {
        if self.failover {
            "fanout-failover"
        } else {
            "fanout"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use tpt_log_core::batch::CompressedBatchPayload;

    fn batch() -> CompressedBatch {
        CompressedBatch {
            payload: CompressedBatchPayload {
                bytes: b"{}".to_vec(),
                entry_count: 1,
                compressed: false,
                raw_json: None,
            },
        }
    }

    struct Stub {
        name: &'static str,
        outcome: Result<(), &'static str>,
        calls: AtomicU32,
    }
    impl Stub {
        fn ok(name: &'static str) -> Self {
            Self {
                name,
                outcome: Ok(()),
                calls: AtomicU32::new(0),
            }
        }
        fn err(name: &'static str, kind: &'static str) -> Self {
            Self {
                name,
                outcome: Err(kind),
                calls: AtomicU32::new(0),
            }
        }
        fn calls(&self) -> u32 {
            self.calls.load(Ordering::Relaxed)
        }
    }
    impl Transport for Stub {
        fn send(&self, _b: &CompressedBatch) -> Result<(), TransportError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            match self.outcome {
                Ok(()) => Ok(()),
                Err("permanent") => Err(TransportError::Permanent(self.name.into())),
                Err(_) => Err(TransportError::Transient(self.name.into())),
            }
        }
        fn name(&self) -> &str {
            self.name
        }
    }

    /// Wrap a stub so it satisfies `Box<dyn Transport>`.
    fn boxed(stub: Arc<Stub>) -> Box<dyn Transport> {
        Box::new(StubTransport(stub))
    }

    struct StubTransport(Arc<Stub>);

    impl Transport for StubTransport {
        fn send(&self, b: &CompressedBatch) -> Result<(), TransportError> {
            self.0.send(b)
        }
        fn name(&self) -> &str {
            self.0.name()
        }
    }

    fn stubs() -> (Arc<Stub>, Arc<Stub>) {
        (
            Arc::new(Stub::err("first", "permanent")),
            Arc::new(Stub::ok("second")),
        )
    }

    #[test]
    fn fanout_delivers_to_all_and_reports_first_failure() {
        let (a, b) = stubs();
        let ok = Arc::new(Stub::ok("ok"));
        let router =
            RoutingTransport::fanout(vec![boxed(a.clone()), boxed(ok.clone()), boxed(b.clone())]);
        let err = router.send(&batch()).unwrap_err();
        assert!(matches!(err, TransportError::Permanent(_)));
        assert_eq!(a.calls(), 1);
        assert_eq!(ok.calls(), 1, "fan-out reaches every backend");
        assert_eq!(b.calls(), 1);
    }

    #[test]
    fn failover_stops_at_first_success_and_skips_permanent() {
        let (a, b) = stubs();
        let never = Arc::new(Stub::err("third", "permanent"));
        let router = RoutingTransport::failover(vec![
            boxed(a.clone()),
            boxed(b.clone()),
            boxed(never.clone()),
        ]);
        router.send(&batch()).unwrap();
        assert_eq!(a.calls(), 1, "permanent error advances to the next backend");
        assert_eq!(b.calls(), 1);
        assert_eq!(never.calls(), 0, "first success stops the chain");
    }

    #[test]
    fn failover_transient_stops_for_retry() {
        let t = Arc::new(Stub::err("down", "transient"));
        let ok = Arc::new(Stub::ok("healthy"));
        let router = RoutingTransport::failover(vec![boxed(t.clone()), boxed(ok.clone())]);
        let err = router.send(&batch()).unwrap_err();
        assert!(matches!(err, TransportError::Transient(_)));
        assert_eq!(
            ok.calls(),
            0,
            "transient errors must not burn later backends"
        );
    }

    #[test]
    fn empty_router_is_config_error() {
        let router = RoutingTransport::fanout(vec![]);
        assert!(matches!(
            router.send(&batch()),
            Err(TransportError::Config(_))
        ));
    }
}
