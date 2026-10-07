//! `tpt-microlog-agent` — a sidecar for languages without a tpt-microlog binding.
//!
//! ```text
//! tpt-microlog-agent [--udp ADDR] [--tcp ADDR] [--http ADDR] [--unix PATH]
//!                    [--config tpt-log.toml] [--max-connections N]
//! ```
//!
//! With no listener flags it serves UDP on 127.0.0.1:5514 and HTTP on
//! 127.0.0.1:8686. The delivery side (backend, WAL, redaction, sampling,
//! health endpoint, ...) comes from `tpt-log.toml` / `TPT_LOG_*` as usual.

use std::process::ExitCode;
use std::sync::mpsc;

use tpt_log_agent::{Agent, AgentConfig};
use tpt_log_transport::Builder;

const USAGE: &str = "\
tpt-microlog-agent - accept logs over UDP/TCP/HTTP/Unix socket and deliver them reliably

USAGE:
  tpt-microlog-agent [--udp ADDR] [--tcp ADDR] [--http ADDR] [--unix PATH]
                     [--config FILE] [--max-connections N]

Defaults when no listener is given: --udp 127.0.0.1:5514 --http 127.0.0.1:8686

Input (per line, datagram or HTTP body): JSON objects such as
  {\"level\":\"warn\",\"message\":\"slow\",\"user\":42}   a log entry
  {\"type\":\"metric\",\"name\":\"cpu\",\"value\":0.5}     a metric
  [..] / {\"entries\":[..]}                              batches
or plain text lines (INFO). HTTP: POST /ingest (Content-Length required);
GET /healthz. Delivery is configured by tpt-log.toml / TPT_LOG_*.";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if let Some(path) = flag(&args, "--config") {
        std::env::set_var("TPT_LOG_CONFIG", path);
    }

    let mut cfg = AgentConfig {
        udp: flag(&args, "--udp"),
        tcp: flag(&args, "--tcp"),
        http: flag(&args, "--http"),
        unix: flag(&args, "--unix").map(Into::into),
        max_connections: flag(&args, "--max-connections")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
    };
    if cfg.udp.is_none() && cfg.tcp.is_none() && cfg.http.is_none() && cfg.unix.is_none() {
        cfg.udp = Some("127.0.0.1:5514".into());
        cfg.http = Some("127.0.0.1:8686".into());
    }

    let log = match Builder::new().build_arc() {
        Ok(log) => log,
        Err(e) => {
            eprintln!("error: cannot start the logger: {e}");
            return ExitCode::from(2);
        }
    };
    let agent = match Agent::start(log.clone(), &cfg) {
        Ok(agent) => agent,
        Err(e) => {
            eprintln!("error: cannot start listeners: {e}");
            return ExitCode::from(2);
        }
    };
    let addrs = agent.addrs();
    for (name, addr) in [("udp", addrs.udp), ("tcp", addrs.tcp), ("http", addrs.http)] {
        if let Some(addr) = addr {
            println!("listening: {name} {addr}");
        }
    }
    if let Some(path) = &cfg.unix {
        println!("listening: unix {}", path.display());
    }
    println!(
        "delivering via backend '{}' (Ctrl+C / SIGTERM to flush and exit)",
        log.config().backend
    );

    let (tx, rx) = mpsc::channel();
    if let Err(e) = ctrlc::set_handler(move || {
        let _ = tx.send(());
    }) {
        eprintln!("warning: cannot install the signal handler: {e}");
    }
    let _ = rx.recv();

    println!("shutting down...");
    agent.stop();
    log.shutdown();
    ExitCode::SUCCESS
}
