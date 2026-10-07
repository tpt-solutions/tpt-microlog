//! The tpt-microlog sidecar: accept logs over UDP, TCP, HTTP or a Unix
//! socket from programs that have no tpt-microlog binding, and deliver them
//! with the usual batching, WAL and retry guarantees.
//!
//! See [`parse`] for the accepted input shapes. Back-pressure follows the
//! logger's `queue_full_policy`: with the default `block`, a slow backend
//! slows TCP/HTTP/Unix clients down (UDP simply loses what the OS drops).

pub mod parse;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use tpt_log_core::MicroLog;

use parse::Parsed;

/// Largest accepted single line / datagram / header block.
const MAX_LINE: usize = 1024 * 1024;
/// Largest accepted HTTP body.
const MAX_BODY: usize = 8 * 1024 * 1024;
/// Poll interval of the accept loops (also bounds how fast `stop` is noticed).
const POLL: Duration = Duration::from_millis(25);

/// Which listeners to start. `None` disables a listener; use port 0 to let
/// the OS pick one (see [`Agent::addrs`]).
#[derive(Clone, Debug, Default)]
pub struct AgentConfig {
    pub udp: Option<String>,
    pub tcp: Option<String>,
    pub http: Option<String>,
    /// Unix-domain stream socket path (Unix only).
    pub unix: Option<PathBuf>,
    /// Concurrent TCP/HTTP/Unix connections served (default 256).
    pub max_connections: usize,
}

/// Addresses the listeners actually bound to.
#[derive(Clone, Debug, Default)]
pub struct BoundAddrs {
    pub udp: Option<std::net::SocketAddr>,
    pub tcp: Option<std::net::SocketAddr>,
    pub http: Option<std::net::SocketAddr>,
}

/// Counters, readable while the agent runs.
#[derive(Default)]
pub struct AgentStats {
    /// Records handed to the logger.
    pub accepted: AtomicU64,
    /// Connections refused because `max_connections` was reached.
    pub refused: AtomicU64,
}

/// A running agent. Dropping it without [`stop`](Agent::stop) leaves the
/// listener threads running until the process exits.
pub struct Agent {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    addrs: BoundAddrs,
    unix_path: Option<PathBuf>,
    pub stats: Arc<AgentStats>,
}

fn ingest(log: &MicroLog, stats: &AgentStats, parsed: Vec<Parsed>) -> usize {
    let n = parsed.len();
    for record in parsed {
        match record {
            Parsed::Log {
                level,
                message,
                metadata,
            } => log.log_with(level, message, metadata),
            Parsed::Metric { name, value, tags } => log.metric(name, value, tags.as_ref()),
        }
    }
    stats.accepted.fetch_add(n as u64, Ordering::Relaxed);
    n
}

struct Slots {
    in_use: AtomicUsize,
    max: usize,
}

struct SlotGuard(Arc<Slots>);
impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.0.in_use.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Slots {
    fn acquire(self: &Arc<Self>) -> Option<SlotGuard> {
        let prev = self.in_use.fetch_add(1, Ordering::AcqRel);
        if prev >= self.max {
            self.in_use.fetch_sub(1, Ordering::AcqRel);
            None
        } else {
            Some(SlotGuard(self.clone()))
        }
    }
}

impl Agent {
    /// Bind the configured listeners and start serving.
    pub fn start(log: Arc<MicroLog>, cfg: &AgentConfig) -> std::io::Result<Agent> {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(AgentStats::default());
        let slots = Arc::new(Slots {
            in_use: AtomicUsize::new(0),
            max: if cfg.max_connections == 0 {
                256
            } else {
                cfg.max_connections
            },
        });
        let mut threads = Vec::new();
        let mut addrs = BoundAddrs::default();

        if let Some(bind) = &cfg.udp {
            let socket = UdpSocket::bind(bind)?;
            socket.set_read_timeout(Some(POLL))?;
            addrs.udp = Some(socket.local_addr()?);
            let (log, stats, stop) = (log.clone(), stats.clone(), stop.clone());
            threads.push(spawn("udp", move || serve_udp(socket, log, stats, stop)));
        }
        if let Some(bind) = &cfg.tcp {
            let listener = TcpListener::bind(bind)?;
            listener.set_nonblocking(true)?;
            addrs.tcp = Some(listener.local_addr()?);
            let (log, stats, stop, slots) =
                (log.clone(), stats.clone(), stop.clone(), slots.clone());
            threads.push(spawn("tcp", move || {
                serve_tcp(listener, log, stats, stop, slots, Mode::Lines)
            }));
        }
        if let Some(bind) = &cfg.http {
            let listener = TcpListener::bind(bind)?;
            listener.set_nonblocking(true)?;
            addrs.http = Some(listener.local_addr()?);
            let (log, stats, stop, slots) =
                (log.clone(), stats.clone(), stop.clone(), slots.clone());
            threads.push(spawn("http", move || {
                serve_tcp(listener, log, stats, stop, slots, Mode::Http)
            }));
        }
        #[allow(unused_mut)] // only assigned on Unix
        let mut unix_path = None;
        #[cfg(unix)]
        if let Some(path) = &cfg.unix {
            let _ = std::fs::remove_file(path); // stale socket from a previous run
            let listener = std::os::unix::net::UnixListener::bind(path)?;
            listener.set_nonblocking(true)?;
            unix_path = Some(path.clone());
            let (log, stats, stop, slots) =
                (log.clone(), stats.clone(), stop.clone(), slots.clone());
            threads.push(spawn("unix", move || {
                serve_unix(listener, log, stats, stop, slots)
            }));
        }
        #[cfg(not(unix))]
        if cfg.unix.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Unix sockets are not available on this platform",
            ));
        }

        Ok(Agent {
            stop,
            threads,
            addrs,
            unix_path,
            stats,
        })
    }

    /// Where each listener is bound.
    pub fn addrs(&self) -> &BoundAddrs {
        &self.addrs
    }

    /// Stop accepting, wait for the listener threads, remove the socket file.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        if let Some(path) = &self.unix_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("tpt-agent-{name}"))
        .spawn(body)
        .expect("spawn agent thread")
}

fn serve_udp(socket: UdpSocket, log: Arc<MicroLog>, stats: Arc<AgentStats>, stop: Arc<AtomicBool>) {
    let mut buf = vec![0u8; 65_536];
    while !stop.load(Ordering::Relaxed) {
        match socket.recv(&mut buf) {
            Ok(n) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                ingest(&log, &stats, parse::parse_payload(&text));
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => std::thread::sleep(POLL),
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Lines,
    Http,
}

fn serve_tcp(
    listener: TcpListener,
    log: Arc<MicroLog>,
    stats: Arc<AgentStats>,
    stop: Arc<AtomicBool>,
    slots: Arc<Slots>,
    mode: Mode,
) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let Some(guard) = slots.acquire() else {
                    stats.refused.fetch_add(1, Ordering::Relaxed);
                    continue; // dropping the stream closes it
                };
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                let (log, stats, stop) = (log.clone(), stats.clone(), stop.clone());
                let _ = std::thread::Builder::new()
                    .name("tpt-agent-conn".into())
                    .spawn(move || {
                        let _guard = guard;
                        match mode {
                            Mode::Lines => serve_lines(stream, &log, &stats, &stop),
                            Mode::Http => serve_http(stream, &log, &stats),
                        }
                    });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

#[cfg(unix)]
fn serve_unix(
    listener: std::os::unix::net::UnixListener,
    log: Arc<MicroLog>,
    stats: Arc<AgentStats>,
    stop: Arc<AtomicBool>,
    slots: Arc<Slots>,
) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let Some(guard) = slots.acquire() else {
                    stats.refused.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let (log, stats, stop) = (log.clone(), stats.clone(), stop.clone());
                let _ = std::thread::Builder::new()
                    .name("tpt-agent-conn".into())
                    .spawn(move || {
                        let _guard = guard;
                        serve_lines(stream, &log, &stats, &stop);
                    });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// Newline-delimited records until the peer closes (or the agent stops).
fn serve_lines<S: Read>(stream: S, log: &MicroLog, stats: &AgentStats, stop: &AtomicBool) {
    let mut reader = BufReader::new(stream);
    // A partial line survives read timeouts: it is only cleared once handled.
    let mut line = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match read_line_bounded(&mut reader, &mut line) {
            Ok(n) => {
                if !line.is_empty() {
                    let text = String::from_utf8_lossy(&line);
                    if let Some(record) = parse::parse_line(&text) {
                        ingest(log, stats, vec![record]);
                    }
                    line.clear();
                }
                if n == 0 {
                    return; // peer closed
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {} // idle: re-check the stop flag, keep the partial line
            Err(_) => return,
        }
    }
}

/// `read_until(b'\n')` that refuses lines longer than [`MAX_LINE`].
fn read_line_bounded<R: BufRead>(reader: &mut R, out: &mut Vec<u8>) -> std::io::Result<usize> {
    let mut total = 0;
    loop {
        let (done, used) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Ok(total);
            }
            match available.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    out.extend_from_slice(&available[..i]);
                    (true, i + 1)
                }
                None => {
                    out.extend_from_slice(available);
                    (false, available.len())
                }
            }
        };
        reader.consume(used);
        total += used;
        if out.len() > MAX_LINE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line too long",
            ));
        }
        if done {
            return Ok(total);
        }
    }
}

fn respond(stream: &mut TcpStream, code: u16, reason: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

/// One HTTP request per connection: `POST /ingest` (or `/`), `GET /healthz`.
fn serve_http(mut stream: TcpStream, log: &MicroLog, stats: &AgentStats) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut head = Vec::new();
    let mut content_length: Option<usize> = None;
    let mut request_line = String::new();
    // Request line + headers, bounded.
    for i in 0..128 {
        head.clear();
        match read_line_bounded(&mut reader, &mut head) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&head)
            .trim_end_matches('\r')
            .to_string();
        if i == 0 {
            request_line = text;
            continue;
        }
        if text.is_empty() {
            break;
        }
        if let Some((name, value)) = text.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => content_length = value.trim().parse().ok(),
                "transfer-encoding" => {
                    respond(
                        &mut stream,
                        411,
                        "Length Required",
                        r#"{"error":"chunked bodies are not supported; send Content-Length"}"#,
                    );
                    return;
                }
                _ => {}
            }
        }
    }
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let path = target.split('?').next().unwrap_or("");
    match (method, path) {
        ("GET", "/healthz") => respond(&mut stream, 200, "OK", r#"{"status":"ok"}"#),
        ("POST", "/ingest") | ("POST", "/") | ("POST", "/v1/logs") => {
            let Some(len) = content_length else {
                respond(
                    &mut stream,
                    411,
                    "Length Required",
                    r#"{"error":"Content-Length required"}"#,
                );
                return;
            };
            if len > MAX_BODY {
                respond(
                    &mut stream,
                    413,
                    "Payload Too Large",
                    r#"{"error":"body too large"}"#,
                );
                return;
            }
            let mut body = vec![0u8; len];
            if reader.read_exact(&mut body).is_err() {
                respond(
                    &mut stream,
                    400,
                    "Bad Request",
                    r#"{"error":"incomplete body"}"#,
                );
                return;
            }
            let accepted = ingest(
                log,
                stats,
                parse::parse_payload(&String::from_utf8_lossy(&body)),
            );
            respond(
                &mut stream,
                202,
                "Accepted",
                &format!(r#"{{"accepted":{accepted}}}"#),
            );
        }
        ("POST", _) | ("GET", _) => {
            respond(&mut stream, 404, "Not Found", r#"{"error":"not found"}"#)
        }
        _ => respond(
            &mut stream,
            405,
            "Method Not Allowed",
            r#"{"error":"method not allowed"}"#,
        ),
    }
}
