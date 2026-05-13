#![allow(clippy::unwrap_used)]

use super::*;
use chrono::Duration;
use std::sync::Mutex;
use vault::intake::{self as vintake, IntakeEntry, IntakeKind};
use vault::schema::Method;

fn make_intake_row(trace: &str, ago: Duration) -> ParsedIntakeRow {
    let when = Local::now() - ago;
    ParsedIntakeRow {
        date: when.format("%Y-%m-%d").to_string(),
        time: when.format("%H:%M").to_string(),
        method: "telegram".to_string(),
        origin_ctx: "chat-1".to_string(),
        kind: IntakeKind::Url.as_str().to_string(),
        preview: "https://example.com".to_string(),
        trace_id: trace.to_string(),
    }
}

#[test]
fn intake_age_secs_handles_past_timestamps() {
    let row = make_intake_row("tg-aaaaaa", Duration::seconds(3600));
    let age = intake_age_secs(&row).expect("parses");
    assert!((3500..=3700).contains(&age), "expected ~1 hour, got {age}s");
}

#[test]
fn intake_age_secs_returns_none_for_bogus_timestamps() {
    let row = ParsedIntakeRow {
        date: "garbage".to_string(),
        time: "nope".to_string(),
        method: "telegram".to_string(),
        origin_ctx: "x".to_string(),
        kind: "url".to_string(),
        preview: "x".to_string(),
        trace_id: "tg-xxxxxx".to_string(),
    };
    assert!(intake_age_secs(&row).is_none());
}

/// Build a `Config` whose vault paths live inside `vault_root`. The intake +
/// DLQ files are created so the watchdog scan succeeds.
fn config_for(vault_root: &std::path::Path) -> Config {
    let mut cfg = Config::default();
    cfg.vault.root_path = vault_root.display().to_string();
    // Short hard_timeout so a row aged a few minutes is past deadline
    // without requiring the test to manipulate clocks back ~30 minutes.
    cfg.pipeline.hard_timeout_secs = 60;
    vintake::ensure_intake_exists(&vintake::intake_path(vault_root)).unwrap();
    vault::dlq::ensure_dlq_exists(&vault::dlq::dlq_path(vault_root)).unwrap();
    cfg
}

fn write_old_intake(vault_root: &std::path::Path, trace_id: &str, ago: Duration) {
    let when = Local::now() - ago;
    let entry = IntakeEntry {
        date: when.format("%Y-%m-%d").to_string(),
        time: when.format("%H:%M").to_string(),
        method: Method::Telegram,
        origin_ctx: "chat-1".to_string(),
        kind: IntakeKind::Url,
        preview: "https://example.com/aged".to_string(),
        trace_id: trace_id.to_string(),
    };
    vintake::append_entry(&vintake::intake_path(vault_root), &entry).unwrap();
}

fn dlq_orphan_count(vault_root: &std::path::Path) -> usize {
    vault::dlq::parse_entries(&vault::dlq::dlq_path(vault_root))
        .unwrap()
        .iter()
        .filter(|r| r.stage == "watchdog-orphan")
        .count()
}

#[test]
fn run_once_skips_orphan_when_predicate_says_active() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_for(dir.path());
    let trace_id = "tg-active";
    // Intake age ~ 10 minutes, deadline = hard_timeout(60) + buffer(60) = 120s.
    write_old_intake(dir.path(), trace_id, Duration::seconds(600));

    let active: Mutex<std::collections::HashSet<String>> = Mutex::new([trace_id.to_string()].into_iter().collect());
    let predicate = |t: &str| active.lock().unwrap().contains(t);

    let orphans = run_once(&cfg, &predicate).unwrap();
    assert_eq!(orphans, 0, "active trace should NOT be orphaned");
    assert_eq!(dlq_orphan_count(dir.path()), 0);
}

#[test]
fn run_once_orphans_when_predicate_says_inactive() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_for(dir.path());
    let trace_id = "tg-inactive";
    write_old_intake(dir.path(), trace_id, Duration::seconds(600));

    let predicate = |_t: &str| false;
    let orphans = run_once(&cfg, &predicate).unwrap();
    assert_eq!(orphans, 1, "inactive aged trace should be orphaned");
    assert_eq!(dlq_orphan_count(dir.path()), 1);
}

#[test]
fn run_once_does_not_orphan_fresh_traces() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_for(dir.path());
    // 10 seconds old, well under deadline of 120s.
    write_old_intake(dir.path(), "tg-fresh", Duration::seconds(10));

    let predicate = |_t: &str| false;
    let orphans = run_once(&cfg, &predicate).unwrap();
    assert_eq!(orphans, 0);
}
