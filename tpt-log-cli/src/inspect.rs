//! WAL inspection (`wal query`) and the redacted support bundle.
//!
//! The bundle is meant to be attached to a GitHub issue: it holds the
//! effective configuration (secrets masked), environment *names* (values
//! masked when they look secret), health checks, and a sample of recent
//! undelivered entries with messages scrubbed by the built-in PII detectors
//! and metadata reduced to key names.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use tpt_log_core::redact::Redactor;
use tpt_log_core::{Config, LogEntry, LogLevel, Wal};
use tpt_log_transport::build_transport;

const MASK: &str = "[REDACTED]";

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// `Name=Value` header -> `Name=[REDACTED]`.
pub fn mask_header(raw: &str) -> String {
    match raw.split_once('=') {
        Some((name, _)) => format!("{}={MASK}", name.trim()),
        None => MASK.to_string(),
    }
}

/// Hide URL credentials and query strings (which often carry tokens).
pub fn mask_url(raw: &str) -> String {
    let (scheme, rest) = match raw.split_once("://") {
        Some((s, r)) => (format!("{s}://"), r),
        None => (String::new(), raw),
    };
    let (authority_and_path, query) = match rest.split_once('?') {
        Some((a, _)) => (a, true),
        None => (rest, false),
    };
    let authority_and_path = match authority_and_path.split_once('@') {
        // Only the part before the first '/' can hold credentials.
        Some((cred, tail)) if !cred.contains('/') => format!("{MASK}@{tail}"),
        _ => authority_and_path.to_string(),
    };
    format!(
        "{scheme}{authority_and_path}{}",
        if query { "?[REDACTED]" } else { "" }
    )
}

/// Names whose values must never be copied out.
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "SECRET", "TOKEN", "PASSWORD", "AUTH", "CREDENTIAL"]
        .iter()
        .any(|needle| upper.contains(needle))
}

fn masked_config(cfg: &Config) -> Value {
    let mut value = serde_json::to_value(cfg).unwrap_or(Value::Null);
    let Some(obj) = value.as_object_mut() else {
        return value;
    };
    if let Some(Value::Array(headers)) = obj.get_mut("extra_headers") {
        for h in headers.iter_mut() {
            if let Some(raw) = h.as_str() {
                *h = Value::String(mask_header(raw));
            }
        }
    }
    for key in ["endpoint", "grpc_endpoint", "net_endpoint", "proxy"] {
        if let Some(Value::String(raw)) = obj.get(key).cloned() {
            obj.insert(key.into(), Value::String(mask_url(&raw)));
        }
    }
    if let Some(Value::String(id)) = obj.get("aws_access_key").cloned() {
        let shown: String = id.chars().take(4).collect();
        obj.insert("aws_access_key".into(), Value::String(format!("{shown}…")));
    }
    value
}

fn masked_env() -> Value {
    let mut env = Map::new();
    let mut names: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("TPT_LOG_") || k == "AWS_REGION")
        .collect();
    names.sort();
    for (name, value) in names {
        let shown = if is_secret_name(&name) || name.ends_with("HEADERS") {
            MASK.to_string()
        } else {
            value
        };
        env.insert(name, Value::String(shown));
    }
    Value::Object(env)
}

/// Decode every entry of the batch stored at `path`.
fn read_entries(wal: &Wal, path: &Path) -> Result<Vec<LogEntry>, String> {
    let frame = wal.read_frame(path).map_err(|e| e.to_string())?;
    frame.to_compressed_batch().decompress_entries()
}

fn sorted_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "wal"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `tpt-microlog wal query [--dead] [--level L] [--grep S] [--limit N] [--json]`
pub fn wal_query(wal: &Wal, args: &[String]) -> Result<bool, String> {
    let dead = has_flag(args, "--dead");
    let min_level = match flag_value(args, "--level") {
        Some(l) => l
            .parse::<LogLevel>()
            .map_err(|_| format!("unknown level '{l}'"))?,
        None => LogLevel::Trace,
    };
    let needle = flag_value(args, "--grep").map(|s| s.to_lowercase());
    let limit: usize = flag_value(args, "--limit")
        .map(|s| s.parse().map_err(|_| format!("bad --limit '{s}'")))
        .transpose()?
        .unwrap_or(50);
    let as_json = has_flag(args, "--json");

    let dir = if dead {
        wal.dead_dir()
    } else {
        wal.dir().to_path_buf()
    };
    let mut shown = 0usize;
    'files: for path in sorted_files(&dir) {
        let entries = match read_entries(wal, &path) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("{}: unreadable ({e})", file_name(&path));
                continue;
            }
        };
        for entry in entries {
            if entry.level < min_level {
                continue;
            }
            if let Some(n) = &needle {
                let haystack = format!(
                    "{} {}",
                    entry.message,
                    Value::Object(entry.metadata.clone())
                );
                if !haystack.to_lowercase().contains(n) {
                    continue;
                }
            }
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string(&entry).map_err(|e| e.to_string())?
                );
            } else {
                let meta: String = entry
                    .metadata
                    .iter()
                    .map(|(k, v)| format!(" {k}={v}"))
                    .collect();
                println!(
                    "{} {:<5} {}{}  [{}]",
                    entry.timestamp,
                    entry.level.to_string().to_ascii_uppercase(),
                    entry.message,
                    meta,
                    file_name(&path)
                );
            }
            shown += 1;
            if shown >= limit {
                break 'files;
            }
        }
    }
    if shown == 0 {
        eprintln!("no matching entries in {}", dir.display());
    }
    Ok(true)
}

/// Build the support bundle (pure function over config + WAL, for testing).
pub fn build_bundle(cfg: &Config, wal: Option<&Wal>, sample: usize) -> Value {
    let problems = cfg.validate();
    let transport = match build_transport(cfg) {
        Ok(t) => json!({"ok": true, "name": t.name()}),
        Err(e) => json!({"ok": false, "error": mask_url(&e.to_string())}),
    };

    let mut wal_info = Value::Null;
    let mut recent = Vec::new();
    if let Some(wal) = wal {
        let redactor = Redactor::new(&cfg.redact_keys, &cfg.redact_patterns, true);
        let mut pending = Vec::new();
        let files = sorted_files(wal.dir());
        for path in &files {
            let (count, size) = (
                wal.read_frame(path).map(|f| f.entry_count).unwrap_or(0),
                std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            );
            pending.push(json!({"file": file_name(path), "bytes": size, "entries": count}));
        }
        let dead: Vec<String> = sorted_files(&wal.dead_dir())
            .iter()
            .map(|p| file_name(p))
            .collect();
        wal_info = json!({
            "dir": wal.dir().display().to_string(),
            "pending_files": pending,
            "pending_bytes": wal.size_bytes(),
            "dead_letter_files": dead,
        });
        // Newest batches first; messages scrubbed, metadata reduced to keys.
        'outer: for path in files.iter().rev() {
            let Ok(mut entries) = read_entries(wal, path) else {
                continue;
            };
            entries.reverse();
            for mut entry in entries {
                redactor.apply(&mut entry);
                let mut keys: Vec<&String> = entry.metadata.keys().collect();
                keys.sort();
                recent.push(json!({
                    "timestamp": entry.timestamp,
                    "level": entry.level.to_string(),
                    "type": format!("{:?}", entry.log_type).to_lowercase(),
                    "message": entry.message,
                    "metadata_keys": keys,
                }));
                if recent.len() >= sample {
                    break 'outer;
                }
            }
        }
    }

    json!({
        "bundle_version": 1,
        "tool": {
            "name": "tpt-microlog",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "system": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "family": std::env::consts::FAMILY,
        },
        "notes": "Secrets, header values and URL credentials are masked; entry metadata is reduced to key names; messages are scrubbed by the built-in PII detectors. Review before sharing.",
        "config": masked_config(cfg),
        "config_problems": problems,
        "transport": transport,
        "environment": masked_env(),
        "wal": wal_info,
        "recent_undelivered_entries": recent,
    })
}

/// `tpt-microlog support-bundle [--out FILE|-] [--entries N]`
pub fn support_bundle(cfg: &Config, wal: Option<&Wal>, args: &[String]) -> Result<bool, String> {
    let sample: usize = flag_value(args, "--entries")
        .map(|s| s.parse().map_err(|_| format!("bad --entries '{s}'")))
        .transpose()?
        .unwrap_or(20);
    let bundle = build_bundle(cfg, wal, sample);
    let text = serde_json::to_string_pretty(&bundle).map_err(|e| e.to_string())?;
    match flag_value(args, "--out").as_deref() {
        Some("-") => println!("{text}"),
        other => {
            let path = other.unwrap_or("tpt-microlog-support-bundle.json");
            std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
            println!("wrote {path} — review it, then attach it to your issue");
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_log_core::batch::{Batch, Compressor};
    use tpt_log_core::Secret;

    #[test]
    fn masking_helpers() {
        assert_eq!(
            mask_header("Authorization=Bearer abc"),
            "Authorization=[REDACTED]"
        );
        assert_eq!(
            mask_url("https://user:pw@host:3128/x?token=1"),
            "https://[REDACTED]@host:3128/x?[REDACTED]"
        );
        assert_eq!(mask_url("http://host:9200"), "http://host:9200");
        assert_eq!(mask_url("host:5140"), "host:5140");
        assert!(is_secret_name("TPT_LOG_API_KEY"));
        assert!(is_secret_name("TPT_LOG_AWS_SECRET_KEY"));
        assert!(is_secret_name("TPT_LOG_EXTRA_AUTHORIZATION"));
        assert!(!is_secret_name("TPT_LOG_BACKEND"));
    }

    #[test]
    fn bundle_contains_no_secrets_or_metadata_values() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = Wal::open(tmp.path().join("wal"), 1_000_000).unwrap();
        let mut entry = LogEntry::new(LogLevel::Error, "login by alice@example.com failed");
        entry.metadata.insert("password".into(), json!("hunter2"));
        entry.metadata.insert("user_id".into(), json!(1234567));
        let batch = Compressor::new(3, false).compress_batch(
            &Batch {
                entries: vec![entry],
            },
            1,
        );
        wal.write(&batch).unwrap();

        let cfg = Config {
            backend: "http".into(),
            endpoint: Some("https://svc:topsecretpw@logs.example.com/v1?apikey=SEKRET".into()),
            api_key: Some(Secret::new("super-secret-key")),
            extra_headers: vec!["Authorization=Bearer SEKRET-HEADER".into()],
            proxy: Some("http://proxyuser:proxypw@proxy:3128".into()),
            aws_access_key: Some("AKIAEXAMPLEKEY1234".into()),
            aws_secret_key: Some(Secret::new("aws-secret-value")),
            ..Config::default()
        };
        let bundle = build_bundle(&cfg, Some(&wal), 10);
        let text = serde_json::to_string(&bundle).unwrap();
        for leaked in [
            "super-secret-key",
            "SEKRET",
            "topsecretpw",
            "proxypw",
            "aws-secret-value",
            "AKIAEXAMPLEKEY1234",
            "alice@example.com",
            "hunter2",
            "1234567",
        ] {
            assert!(!text.contains(leaked), "bundle leaked {leaked:?}: {text}");
        }
        let recent = &bundle["recent_undelivered_entries"][0];
        assert_eq!(recent["message"], "login by [REDACTED] failed");
        assert_eq!(recent["metadata_keys"], json!(["password", "user_id"]));
        assert_eq!(bundle["wal"]["pending_files"][0]["entries"], 1);
        assert_eq!(bundle["transport"]["ok"], true);
    }
}
