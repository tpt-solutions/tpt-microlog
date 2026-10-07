//! `tpt-microlog` — operator CLI.
//!
//! ```text
//! tpt-microlog init [--preset NAME] [--force]
//! tpt-microlog doctor [--config PATH]
//! tpt-microlog wal ls|replay|purge|dead [--config PATH]
//! tpt-microlog tail FILE [-f]
//! tpt-microlog send-test [MESSAGE] [--config PATH]
//! ```

mod inspect;

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use tpt_log_core::{wal, Config, MicroLog, Wal};
use tpt_log_transport::build_transport;

const USAGE: &str = "\
tpt-microlog — operator CLI

USAGE:
  tpt-microlog init [--preset NAME] [--force]   write a starter ./tpt-log.toml
  tpt-microlog doctor [--config PATH]           check config, DNS/TCP reachability, WAL
  tpt-microlog wal ls [--config PATH]           list pending WAL batches
  tpt-microlog wal replay [--config PATH]       resend pending batches now
  tpt-microlog wal dead [--config PATH]         list dead-lettered batches
  tpt-microlog wal purge [--dead] [--config PATH]  delete pending (or dead) batches
  tpt-microlog wal query [--dead] [--level L] [--grep S] [--limit N] [--json]
                                                decode and search undelivered entries
  tpt-microlog support-bundle [--out FILE|-] [--entries N]
                                                masked diagnostics to attach to an issue
  tpt-microlog tail FILE [-f]                   pretty-print a file-backend JSONL log
  tpt-microlog send-test [MESSAGE] [--config PATH]  deliver one entry and report the result

Config comes from TPT_LOG_CONFIG, ./tpt-log.toml, ~/.config/tpt-microlog/ or
/etc/tpt-microlog/, plus TPT_LOG_* environment overrides.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("init") => init(&args[1..]),
        Some("doctor") => doctor(&args[1..]),
        Some("wal") => wal_cmd(&args[1..]),
        Some("tail") => tail(&args[1..]),
        Some("support-bundle") => support_bundle(&args[1..]),
        Some("send-test") => send_test(&args[1..]),
        Some("-h") | Some("--help") | Some("help") | None => {
            println!("{USAGE}");
            Ok(true)
        }
        Some(other) => Err(format!("unknown command '{other}'\n\n{USAGE}")),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// Positional arguments: skip flags and the values of value-taking flags.
fn positionals(args: &[String]) -> Vec<&String> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
        } else if a == "--config" || a == "--preset" {
            skip = true;
        } else if !a.starts_with('-') {
            out.push(a);
        }
    }
    out
}

fn load_config(args: &[String]) -> Config {
    if let Some(path) = flag_value(args, "--config") {
        std::env::set_var("TPT_LOG_CONFIG", path);
    }
    Config::load()
}

fn wal_dir_for(cfg: &Config) -> PathBuf {
    let ns = wal::namespace(
        cfg.service.as_deref(),
        &cfg.backend,
        cfg.endpoint.as_deref(),
    );
    cfg.resolved_wal_dir().join(ns)
}

fn init(args: &[String]) -> Result<bool, String> {
    let path = PathBuf::from("tpt-log.toml");
    if path.exists() && !has_flag(args, "--force") {
        return Err("tpt-log.toml already exists (use --force to overwrite)".into());
    }
    let preset = flag_value(args, "--preset").unwrap_or_else(|| "dev".into());
    Config::default().apply_preset(&preset)?;
    let body = format!(
        "#:schema https://raw.githubusercontent.com/tpt-solutions/tpt-microlog/master/tpt-log-core/tpt-log.schema.json\n\
         # tpt-microlog configuration — see the README for every key.\n\
         preset = \"{preset}\"\n\
         service = \"my-service\"\n\n\
         # api_key = \"...\"   # or set TPT_LOG_API_KEY\n\
         # endpoint = \"...\"\n\n\
         # Redact secrets/PII before anything touches disk or the network:\n\
         # redact_keys = [\"password\", \"token\", \"authorization\"]\n\
         # redact_builtin = true\n"
    );
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    println!("wrote {} (preset '{preset}')", path.display());
    Ok(true)
}

/// `host:port` from an endpoint URL or bare `host:port`.
fn host_port(endpoint: &str) -> Option<String> {
    let (scheme, rest) = match endpoint.split_once("://") {
        Some((s, r)) => (s, r),
        None => ("", endpoint),
    };
    let authority = rest.split(['/', '?']).next()?;
    let authority = authority.rsplit('@').next()?;
    if authority.contains(':') {
        Some(authority.to_string())
    } else {
        let port = if scheme == "http" { 80 } else { 443 };
        Some(format!("{authority}:{port}"))
    }
}

fn doctor(args: &[String]) -> Result<bool, String> {
    let cfg = load_config(args);
    let mut ok = true;
    let mut check = |label: &str, good: bool, detail: String| {
        println!("[{}] {label}: {detail}", if good { " ok " } else { "FAIL" });
        ok &= good;
    };

    check("backend", true, cfg.backend.clone());
    let problems = cfg.validate();
    if problems.is_empty() {
        check("config", true, "no problems found".into());
    } else {
        for p in problems {
            check("config", false, p);
        }
    }
    match build_transport(&cfg) {
        Ok(t) => check("transport", true, format!("'{}' builds", t.name())),
        Err(e) => check("transport", false, e.to_string()),
    }

    let endpoint = cfg
        .endpoint
        .clone()
        .or_else(|| cfg.inferred_endpoint())
        .or_else(|| cfg.grpc_endpoint.clone())
        .or_else(|| cfg.net_endpoint.clone());
    if let Some(hp) = endpoint.as_deref().and_then(host_port) {
        match hp.to_socket_addrs().map(|a| a.collect::<Vec<_>>()) {
            Ok(addrs) if !addrs.is_empty() => {
                check("dns", true, format!("{hp} -> {}", addrs[0]));
                match TcpStream::connect_timeout(&addrs[0], Duration::from_secs(5)) {
                    Ok(_) => check(
                        "connect",
                        true,
                        format!("TCP connect to {} succeeded", addrs[0]),
                    ),
                    Err(e) => check("connect", false, format!("{}: {e}", addrs[0])),
                }
            }
            Ok(_) => check("dns", false, format!("{hp} resolved to nothing")),
            Err(e) => check("dns", false, format!("{hp}: {e}")),
        }
    }

    if cfg.wal_enabled {
        let dir = wal_dir_for(&cfg);
        match Wal::open(&dir, cfg.wal_max_size_bytes) {
            Ok(w) => check(
                "wal",
                true,
                format!(
                    "{} writable, {} pending, {} bytes",
                    dir.display(),
                    w.len(),
                    w.size_bytes()
                ),
            ),
            Err(e) => check("wal", false, format!("{}: {e}", dir.display())),
        }
    } else {
        check("wal", true, "disabled".into());
    }
    println!(
        "{}",
        if ok {
            "\nAll checks passed."
        } else {
            "\nSome checks failed."
        }
    );
    Ok(ok)
}

fn wal_cmd(args: &[String]) -> Result<bool, String> {
    let sub = args
        .first()
        .map(String::as_str)
        .ok_or("wal needs a subcommand: ls|query|replay|dead|purge")?;
    let rest = &args[1..];
    let cfg = load_config(rest);
    let dir = wal_dir_for(&cfg);
    let wal =
        Wal::open(&dir, cfg.wal_max_size_bytes).map_err(|e| format!("{}: {e}", dir.display()))?;
    match sub {
        "ls" => {
            let items = wal.pending().map_err(|e| e.to_string())?;
            println!("{}: {} pending batch(es)", dir.display(), items.len());
            for (path, frame) in items {
                println!(
                    "  {}  {} bytes  {} entries",
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    frame.bytes.len(),
                    frame.entry_count
                );
            }
        }
        "query" => return inspect::wal_query(&wal, rest),
        "dead" => {
            let dead = wal.dead_dir();
            let names = list_files(&dead);
            println!(
                "{}: {} dead-lettered batch(es)",
                dead.display(),
                names.len()
            );
            for n in names {
                println!("  {n}");
            }
        }
        "purge" => {
            let target = if has_flag(rest, "--dead") {
                list_paths(&wal.dead_dir())
            } else {
                wal.pending_paths().map_err(|e| e.to_string())?
            };
            let n = target.len();
            for p in target {
                let _ = std::fs::remove_file(p);
            }
            println!("purged {n} batch(es)");
        }
        "replay" => {
            let transport = build_transport(&cfg).map_err(|e| e.to_string())?;
            let (mut sent, mut failed) = (0, 0);
            for path in wal.pending_paths().map_err(|e| e.to_string())? {
                let batch = match wal.read_frame(&path) {
                    Ok(f) => f.to_compressed_batch(),
                    Err(e) => {
                        eprintln!("skipping unreadable {}: {e}", path.display());
                        failed += 1;
                        continue;
                    }
                };
                match transport.send(&batch) {
                    Ok(()) => {
                        let _ = wal.remove(&path);
                        sent += 1;
                    }
                    Err(e) => {
                        eprintln!("{}: {e}", path.display());
                        failed += 1;
                    }
                }
            }
            println!("replayed {sent} batch(es), {failed} failed");
            return Ok(failed == 0);
        }
        other => return Err(format!("unknown wal subcommand '{other}'")),
    }
    Ok(true)
}

fn support_bundle(args: &[String]) -> Result<bool, String> {
    let cfg = load_config(args);
    let dir = wal_dir_for(&cfg);
    // A missing/locked WAL must not stop the bundle: that is often the bug.
    let wal = Wal::open(&dir, cfg.wal_max_size_bytes).ok();
    if wal.is_none() {
        eprintln!(
            "note: could not open the WAL at {} (locked by a running logger?)",
            dir.display()
        );
    }
    inspect::support_bundle(&cfg, wal.as_ref(), args)
}

fn list_paths(dir: &std::path::Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default()
}

fn list_files(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = list_paths(dir)
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    v.sort();
    v
}

/// Render one JSONL entry as `time LEVEL message k=v ...`; non-JSON passes through.
fn render_line(line: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return line.to_string();
    };
    let level = v["level"].as_str().unwrap_or("?");
    let msg = v["message"].as_str().unwrap_or("");
    let ts = v["timestamp"].as_u64().unwrap_or(0);
    let meta = v["metadata"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| format!(" {k}={v}"))
                .collect::<String>()
        })
        .unwrap_or_default();
    format!("{ts} {level:<5} {msg}{meta}")
}

fn tail(args: &[String]) -> Result<bool, String> {
    let pos = positionals(args);
    let file = pos.first().ok_or("tail needs a FILE")?;
    let mut reader = BufReader::new(std::fs::File::open(file).map_err(|e| format!("{file}: {e}"))?);
    let follow = has_flag(args, "-f");
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            if !follow {
                return Ok(true);
            }
            let pos = reader.stream_position().map_err(|e| e.to_string())?;
            std::thread::sleep(Duration::from_millis(250));
            reader
                .seek(SeekFrom::Start(pos))
                .map_err(|e| e.to_string())?;
            continue;
        }
        println!("{}", render_line(line.trim_end()));
    }
}

fn send_test(args: &[String]) -> Result<bool, String> {
    let cfg = load_config(args);
    let message = positionals(args)
        .first()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "tpt-microlog send-test".into());
    let transport = build_transport(&cfg).map_err(|e| e.to_string())?;
    let log = MicroLog::new(cfg.clone(), Arc::from(transport));
    log.info(message);
    let flushed = log.flush();
    use std::sync::atomic::Ordering::Relaxed;
    let s = log.stats();
    let (delivered, wal_writes, dropped) = (
        s.delivered_entries.load(Relaxed),
        s.wal_writes.load(Relaxed),
        s.dropped_batches.load(Relaxed),
    );
    log.shutdown();
    println!(
        "backend={} flushed={flushed} delivered_entries={delivered} wal_writes={wal_writes} dropped_batches={dropped}",
        cfg.backend
    );
    if delivered > 0 {
        println!("send-test succeeded");
        Ok(true)
    } else {
        println!("send-test FAILED: nothing was delivered (run `tpt-microlog doctor`)");
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_parsing() {
        assert_eq!(
            host_port("https://api.datadoghq.com/v2").unwrap(),
            "api.datadoghq.com:443"
        );
        assert_eq!(
            host_port("http://localhost:3100").unwrap(),
            "localhost:3100"
        );
        assert_eq!(host_port("http://example.org").unwrap(), "example.org:80");
        assert_eq!(host_port("10.0.0.1:514").unwrap(), "10.0.0.1:514");
    }

    #[test]
    fn renders_jsonl_and_passes_through_garbage() {
        let l = r#"{"timestamp":5,"level":"INFO","message":"hi","metadata":{"a":1}}"#;
        assert_eq!(render_line(l), "5 INFO  hi a=1");
        assert_eq!(render_line("not json"), "not json");
    }
}
