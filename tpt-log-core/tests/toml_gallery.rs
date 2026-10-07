//! Every ```toml block in docs/toml-gallery.md must parse, use only known keys
//! and pass `Config::validate()`, so the gallery cannot drift from the code.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tpt_log_core::Config;

fn gallery() -> Option<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/toml-gallery.md");
    std::fs::read_to_string(path).ok()
}

fn toml_blocks(markdown: &str) -> Vec<(usize, String)> {
    let mut blocks = Vec::new();
    let mut current: Option<(usize, String)> = None;
    for (idx, line) in markdown.lines().enumerate() {
        match (&mut current, line.trim_end()) {
            (None, "```toml") => current = Some((idx + 1, String::new())),
            (Some(_), "```") => blocks.push(current.take().unwrap()),
            (Some((_, body)), text) => {
                body.push_str(text);
                body.push('\n');
            }
            _ => {}
        }
    }
    blocks
}

#[test]
fn every_gallery_config_is_valid() {
    let Some(markdown) = gallery() else {
        eprintln!("docs/toml-gallery.md not found (packaged crate?); skipping");
        return;
    };
    let blocks = toml_blocks(&markdown);
    assert!(
        blocks.len() >= 15,
        "expected a full gallery, found {}",
        blocks.len()
    );

    // Unknown keys are reported through the diagnostics channel, not an error.
    let warnings = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = warnings.clone();
    tpt_log_core::set_on_error(Some(Arc::new(move |kind, msg| {
        sink.lock().unwrap().push(format!("{kind}: {msg}"));
    })));

    let mut failures = Vec::new();
    for (line, body) in &blocks {
        match Config::from_toml_str(body) {
            Err(e) => failures.push(format!("line {line}: does not parse: {e}")),
            Ok(cfg) => {
                for problem in cfg.validate() {
                    failures.push(format!("line {line}: {problem}"));
                }
            }
        }
    }
    tpt_log_core::set_on_error(None);

    failures.extend(
        warnings
            .lock()
            .unwrap()
            .iter()
            .map(|w| format!("diagnostic while loading the gallery: {w}")),
    );
    assert!(
        failures.is_empty(),
        "gallery problems:\n{}",
        failures.join("\n")
    );
}
