//! End-to-end behaviour of the volume governors through a real `MicroLog`.

use std::sync::atomic::Ordering::Relaxed;

use tpt_log_core::{Config, LogEntry, LogLevel, MicroLog};

fn config() -> Config {
    Config {
        backend: "none".into(),
        wal_enabled: false,
        batch_size: 1000,
        flush_interval_ms: 20,
        shutdown_timeout_ms: 2_000,
        ..Config::default()
    }
}

fn delivered(spy: &tpt_log_core::transport::SpyTransport) -> Vec<LogEntry> {
    spy.entries()
}

#[test]
fn dedup_folds_a_burst_and_reports_repeat_count() {
    let mut cfg = config();
    cfg.dedup_window_ms = 60_000;
    let (log, spy) = MicroLog::with_spy(cfg);
    for _ in 0..5 {
        log.error("database unreachable");
    }
    log.info("something else");
    assert!(log.flush());

    let entries = delivered(&spy);
    let boom: Vec<&LogEntry> = entries
        .iter()
        .filter(|e| e.message == "database unreachable")
        .collect();
    assert_eq!(boom.len(), 2, "first entry + one summary: {boom:?}");
    assert!(boom[0].metadata.get("repeat_count").is_none());
    assert_eq!(boom[1].metadata["repeat_count"], 4);
    assert_eq!(boom[1].metadata["repeat_summary"], true);
    assert_eq!(log.stats().deduplicated_entries.load(Relaxed), 4);
    assert!(entries.iter().any(|e| e.message == "something else"));
}

#[test]
fn dedup_summary_is_flushed_on_shutdown() {
    let mut cfg = config();
    cfg.dedup_window_ms = 60_000;
    let (log, spy) = MicroLog::with_spy(cfg);
    for _ in 0..3 {
        log.warn("flaky upstream");
    }
    log.shutdown();
    let entries = delivered(&spy);
    assert!(
        entries.iter().any(|e| e.message == "flaky upstream"
            && e.metadata.get("repeat_count") == Some(&serde_json::json!(2))),
        "{entries:?}"
    );
}

#[test]
fn rate_limit_thins_routine_entries_but_never_warnings() {
    let mut cfg = config();
    cfg.rate_limit_per_sec = 10;
    let (log, spy) = MicroLog::with_spy(cfg);
    for i in 0..200 {
        log.info(format!("routine {i}"));
    }
    for i in 0..20 {
        log.warn(format!("important {i}"));
    }
    assert!(log.flush());

    let entries = delivered(&spy);
    let infos = entries.iter().filter(|e| e.level == LogLevel::Info).count();
    let warns = entries.iter().filter(|e| e.level == LogLevel::Warn).count();
    assert!(
        (10..=60).contains(&infos),
        "one burst plus slow refill, got {infos}"
    );
    assert_eq!(warns, 20, "WARN is never rate limited");
    assert_eq!(
        log.stats().rate_limited_entries.load(Relaxed) as usize,
        200 - infos
    );
}

#[test]
fn flight_recorder_ships_debug_context_only_when_an_error_follows() {
    let mut cfg = config();
    cfg.flight_recorder_size = 50;
    cfg.min_level = Some("INFO".into()); // DEBUG is below it, yet still recorded
    let (log, spy) = MicroLog::with_spy(cfg);

    for i in 0..5 {
        log.debug(format!("step {i}"));
    }
    log.info("request finished fine");
    assert!(log.flush());
    assert!(
        !delivered(&spy).iter().any(|e| e.level == LogLevel::Debug),
        "no error yet: DEBUG must not ship"
    );
    assert_eq!(log.stats().recorded_entries.load(Relaxed), 5);

    log.debug("about to fail");
    log.error("request failed");
    assert!(log.flush());

    let entries = delivered(&spy);
    let order: Vec<&str> = entries.iter().map(|e| e.message.as_str()).collect();
    let err_at = order.iter().position(|m| *m == "request failed").unwrap();
    let ctx_at = order.iter().position(|m| *m == "about to fail").unwrap();
    assert!(ctx_at < err_at, "context precedes the error: {order:?}");
    let ctx: Vec<&LogEntry> = entries
        .iter()
        .filter(|e| e.metadata.get("flight_recorder") == Some(&serde_json::json!(true)))
        .collect();
    assert_eq!(ctx.len(), 6, "5 earlier steps + the last one: {order:?}");

    // The ring was emptied: a second error brings no stale context.
    log.error("another failure");
    assert!(log.flush());
    let after = delivered(&spy);
    assert_eq!(
        after
            .iter()
            .filter(|e| e.metadata.get("flight_recorder").is_some())
            .count(),
        6
    );
}

#[test]
fn daily_budget_caps_routine_volume_but_not_errors() {
    let mut cfg = config();
    cfg.daily_budget_entries = 5;
    let (log, spy) = MicroLog::with_spy(cfg);
    for i in 0..100 {
        log.info(format!("routine {i}"));
    }
    for i in 0..10 {
        log.error(format!("failure {i}"));
    }
    assert!(log.flush());

    let entries = delivered(&spy);
    let infos = entries.iter().filter(|e| e.level == LogLevel::Info).count();
    let errors = entries
        .iter()
        .filter(|e| e.level == LogLevel::Error)
        .count();
    assert!(infos <= 5, "budget of 5 routine entries, got {infos}");
    assert_eq!(errors, 10, "errors are never budget-limited");
    assert!(log.stats().sampled_entries.load(Relaxed) >= 95);
}
