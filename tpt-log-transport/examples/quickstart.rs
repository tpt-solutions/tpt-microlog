//! The Rust quickstart from the book, as a program CI compiles and runs.
//!
//! `cargo run -p tpt-log-transport --example quickstart` writes `app.jsonl`
//! into the system temp directory and prints its path.

use tpt_log_core::{install_panic_hook, mlog};
use tpt_log_transport::Builder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::temp_dir().join("tpt-microlog-quickstart.jsonl");
    let _ = std::fs::remove_file(&out);

    let log = Builder::new()
        .backend("file")
        .configure(|c| {
            c.file_path = Some(out.to_string_lossy().into_owned());
            c.wal_enabled = false;
        })
        .service("checkout")
        .redact_keys(["password", "token"])
        .build_arc()?;

    install_panic_hook(log.clone());

    log.info("service started");
    mlog!(
        log,
        Info,
        "user logged in",
        user_id = 123,
        ip = "1.2.3.4",
        password = "hunter2"
    );
    log.metric("queue_depth", 7.0, None);

    log.shutdown();

    let written = std::fs::read_to_string(&out)?;
    assert!(
        written.contains("user logged in"),
        "entry missing:\n{written}"
    );
    assert!(
        written.contains("\"user_id\":123"),
        "metadata missing:\n{written}"
    );
    assert!(!written.contains("hunter2"), "password leaked:\n{written}");
    println!("ok: wrote {}", out.display());
    Ok(())
}
