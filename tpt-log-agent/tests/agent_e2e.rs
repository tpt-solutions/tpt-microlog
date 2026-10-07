//! The agent against a real logger, over real sockets.

use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tpt_log_agent::{Agent, AgentConfig};
use tpt_log_core::transport::SpyTransport;
use tpt_log_core::{Config, LogEntry, LogLevel, LogType, MicroLog};

fn start(cfg: AgentConfig) -> (Agent, Arc<MicroLog>, Arc<SpyTransport>) {
    let (log, spy) = MicroLog::with_spy(Config {
        wal_enabled: false,
        batch_size: 1,
        flush_interval_ms: 20,
        shutdown_timeout_ms: 2_000,
        ..Config::default()
    });
    let log = Arc::new(log);
    let agent = Agent::start(log.clone(), &cfg).expect("agent starts");
    (agent, log, spy)
}

fn wait_for(
    log: &MicroLog,
    spy: &SpyTransport,
    what: &str,
    mut ok: impl FnMut(&[LogEntry]) -> bool,
) -> Vec<LogEntry> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        log.flush();
        let entries = spy.entries();
        if ok(&entries) {
            return entries;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; have {entries:?}"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn has(entries: &[LogEntry], msg: &str) -> bool {
    entries.iter().any(|e| e.message == msg)
}

fn http(addr: std::net::SocketAddr, request: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let code = raw.split_whitespace().nth(1).unwrap().parse().unwrap();
    (code, raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string())
}

fn post(addr: std::net::SocketAddr, path: &str, body: &str) -> (u16, String) {
    http(
        addr,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

#[test]
fn udp_delivers_logs_metrics_and_batches() {
    let (agent, log, spy) = start(AgentConfig {
        udp: Some("127.0.0.1:0".into()),
        ..Default::default()
    });
    let target = agent.addrs().udp.unwrap();
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .send_to(
            br#"{"level":"error","message":"udp boom","code":7}"#,
            target,
        )
        .unwrap();
    client
        .send_to(
            br#"{"type":"metric","name":"udp_gauge","value":2.5,"tags":{"k":"v"}}"#,
            target,
        )
        .unwrap();
    client
        .send_to(b"plain text over udp\nsecond line", target)
        .unwrap();

    let entries = wait_for(&log, &spy, "udp entries", |e| {
        has(e, "udp boom") && has(e, "udp_gauge") && has(e, "second line")
    });
    let boom = entries.iter().find(|e| e.message == "udp boom").unwrap();
    assert_eq!(boom.level, LogLevel::Error);
    assert_eq!(boom.metadata["code"], 7);
    let gauge = entries.iter().find(|e| e.message == "udp_gauge").unwrap();
    assert_eq!(gauge.log_type, LogType::Metric);
    agent.stop();
}

#[test]
fn tcp_handles_lines_split_across_packets_and_pauses() {
    let (agent, log, spy) = start(AgentConfig {
        tcp: Some("127.0.0.1:0".into()),
        ..Default::default()
    });
    let mut conn = TcpStream::connect(agent.addrs().tcp.unwrap()).unwrap();
    conn.write_all(b"{\"message\":\"first\"}\n{\"message\":\"spl")
        .unwrap();
    conn.flush().unwrap();
    // Longer than the connection's 1 s read timeout: the partial line must
    // survive the idle period.
    std::thread::sleep(Duration::from_millis(1300));
    conn.write_all(b"it across a pause\",\"level\":\"warn\"}\n")
        .unwrap();
    conn.write_all(b"unterminated last line").unwrap();
    drop(conn); // EOF terminates the last line

    let entries = wait_for(&log, &spy, "tcp entries", |e| {
        has(e, "first") && has(e, "split across a pause") && has(e, "unterminated last line")
    });
    let split = entries
        .iter()
        .find(|e| e.message == "split across a pause")
        .unwrap();
    assert_eq!(split.level, LogLevel::Warn);
    agent.stop();
}

#[test]
fn http_ingest_accepts_documents_and_rejects_bad_requests() {
    let (agent, log, spy) = start(AgentConfig {
        http: Some("127.0.0.1:0".into()),
        ..Default::default()
    });
    let addr = agent.addrs().http.unwrap();

    let (code, body) = post(
        addr,
        "/ingest",
        r#"[{"msg":"array one"},{"msg":"array two","level":50}]"#,
    );
    assert_eq!(code, 202);
    assert_eq!(body, r#"{"accepted":2}"#);
    let (code, _) = post(addr, "/ingest", r#"{"entries":[{"message":"wrapped"}]}"#);
    assert_eq!(code, 202);
    let (code, _) = post(addr, "/ingest", "line one\nline two");
    assert_eq!(code, 202);

    assert_eq!(
        http(
            addr,
            "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
        )
        .0,
        200
    );
    assert_eq!(
        http(
            addr,
            "GET /nope HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
        )
        .0,
        404
    );
    assert_eq!(
        http(
            addr,
            "DELETE /ingest HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
        )
        .0,
        405
    );
    assert_eq!(
        http(
            addr,
            "POST /ingest HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n{}"
        )
        .0,
        411,
        "Content-Length is required"
    );
    assert_eq!(
        http(
            addr,
            "POST /ingest HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        )
        .0,
        411
    );
    assert_eq!(
        http(
            addr,
            &format!(
                "POST /ingest HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
                9 * 1024 * 1024
            )
        )
        .0,
        413
    );

    let entries = wait_for(&log, &spy, "http entries", |e| {
        has(e, "array one") && has(e, "wrapped") && has(e, "line two")
    });
    let two = entries.iter().find(|e| e.message == "array two").unwrap();
    assert_eq!(two.level, LogLevel::Error);
    assert_eq!(
        agent
            .stats
            .accepted
            .load(std::sync::atomic::Ordering::Relaxed),
        5
    );
    agent.stop();
}

#[cfg(unix)]
#[test]
fn unix_socket_delivers() {
    use std::os::unix::net::UnixStream;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.sock");
    let (agent, log, spy) = start(AgentConfig {
        unix: Some(path.clone()),
        ..Default::default()
    });
    let mut conn = UnixStream::connect(&path).unwrap();
    conn.write_all(b"{\"message\":\"over a unix socket\"}\n")
        .unwrap();
    drop(conn);
    wait_for(&log, &spy, "unix entry", |e| has(e, "over a unix socket"));
    agent.stop();
    assert!(!path.exists(), "socket file removed on stop");
}

#[test]
fn connection_limit_refuses_the_excess() {
    let (agent, _log, _spy) = start(AgentConfig {
        tcp: Some("127.0.0.1:0".into()),
        max_connections: 1,
        ..Default::default()
    });
    let addr = agent.addrs().tcp.unwrap();
    let _held = TcpStream::connect(addr).unwrap(); // occupies the only slot
    std::thread::sleep(Duration::from_millis(150));
    let mut second = TcpStream::connect(addr).unwrap();
    second
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut buf = [0u8; 8];
    // A refused connection is closed by the agent: read sees EOF (or a reset).
    let closed = matches!(second.read(&mut buf), Ok(0) | Err(_));
    assert!(closed, "excess connection should be closed");
    assert!(
        agent
            .stats
            .refused
            .load(std::sync::atomic::Ordering::Relaxed)
            >= 1
    );
    agent.stop();
}
