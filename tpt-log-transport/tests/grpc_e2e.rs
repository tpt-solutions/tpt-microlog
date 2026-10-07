//! End-to-end tests for the OTLP gRPC transport against an in-process server.

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tpt_log_core::{Config, MicroLog};
use tpt_log_transport::gen::opentelemetry::proto::logs::v1::ExportLogsServiceResponse;
use tpt_log_transport::gen::{ExportLogsServiceRequest, LogsService, LogsServiceServer};
use tpt_log_transport::{build_transport, GrpcTransport};

#[derive(Clone, Default)]
struct Collector {
    bodies: Arc<Mutex<Vec<String>>>,
}

#[tonic::async_trait]
impl LogsService for Collector {
    async fn export(
        &self,
        request: tonic::Request<ExportLogsServiceRequest>,
    ) -> Result<tonic::Response<ExportLogsServiceResponse>, tonic::Status> {
        let mut seen = self.bodies.lock().unwrap();
        for rl in &request.get_ref().resource_logs {
            for sl in &rl.scope_logs {
                for rec in &sl.log_records {
                    let text = rec
                        .body
                        .as_ref()
                        .and_then(|b| b.value.as_ref())
                        .map(|v| format!("{v:?}"))
                        .unwrap_or_default();
                    seen.push(text);
                }
            }
        }
        Ok(tonic::Response::new(ExportLogsServiceResponse::default()))
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Serve `collector` on `port` from its own thread + runtime.
fn serve(collector: Collector, port: u16) {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .build()
            .unwrap()
            .block_on(async move {
                tonic::transport::Server::builder()
                    .add_service(LogsServiceServer::new(collector))
                    .serve(format!("127.0.0.1:{port}").parse().unwrap())
                    .await
                    .unwrap();
            });
    });
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for: {what}");
}

fn config_for(port: u16) -> Config {
    Config {
        backend: "grpc".into(),
        grpc_endpoint: Some(format!("127.0.0.1:{port}")),
        wal_enabled: false,
        batch_size: 1,
        flush_interval_ms: 20,
        max_retries: 20,
        retry_base_ms: 50,
        retry_max_backoff_ms: 200,
        shutdown_timeout_ms: 5_000,
        ..Config::default()
    }
}

/// The transport is built on this thread but used by MicroLog's delivery
/// thread, so a channel bound to the constructing thread's runtime would hang.
#[test]
fn delivers_when_built_on_one_thread_and_used_on_another() {
    let port = free_port();
    let collector = Collector::default();
    serve(collector.clone(), port);
    wait_until("server accepting connections", || {
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    });

    let cfg = config_for(port);
    let transport = build_transport(&cfg).expect("transport builds");
    let log = MicroLog::new(cfg, Arc::from(transport));
    log.info("over grpc");
    assert!(log.flush());
    wait_until("collector receives the record", || {
        !collector.bodies.lock().unwrap().is_empty()
    });
    log.shutdown();
    assert!(collector.bodies.lock().unwrap()[0].contains("over grpc"));
}

/// An app that starts before its collector must still get a working logger
/// and deliver once the collector appears.
#[test]
fn logger_starts_before_the_collector_and_delivers_after_it_appears() {
    let port = free_port();
    let cfg = config_for(port);

    // Nothing is listening yet: construction must not fail.
    let transport = build_transport(&cfg).expect("build must not require a live collector");
    let log = MicroLog::new(cfg, Arc::from(transport));
    log.warn("logged while the collector was down");

    std::thread::sleep(Duration::from_millis(300));
    let collector = Collector::default();
    serve(collector.clone(), port);

    wait_until("record delivered after the collector came up", || {
        collector
            .bodies
            .lock()
            .unwrap()
            .iter()
            .any(|b| b.contains("while the collector was down"))
    });
    log.shutdown();
}

#[test]
fn explicit_connect_still_reports_unreachable_endpoints() {
    let port = free_port();
    // `blocking_connect` is the "fail fast" entry point and keeps its contract.
    assert!(GrpcTransport::blocking_connect(&format!("127.0.0.1:{port}")).is_err());
}
