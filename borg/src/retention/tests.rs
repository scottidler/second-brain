#![allow(clippy::unwrap_used)]

use super::*;
use crate::config::{Config, StagingLayout};
use filetime::{FileTime, set_file_mtime};
use std::path::Path;
use tempfile::TempDir;

fn write_trace(root: &Path, trace_id: &str, rejected: bool) {
    let dir = root.join(trace_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("envelope.yml"), b"trace: t\n").unwrap();
    if rejected {
        std::fs::write(dir.join("rejection.yml"), b"trace: t\n").unwrap();
    }
}

fn age_directory(path: &Path, days_old: i64) {
    let seconds = 86_400 * days_old;
    let now = std::time::SystemTime::now();
    let past = now - std::time::Duration::from_secs(seconds as u64);
    let ft = FileTime::from_system_time(past);
    set_file_mtime(path, ft).unwrap();
}

fn make_config(root: &Path) -> Config {
    let mut config = Config::default();
    config.staging.enabled = true;
    config.staging.root = root.to_path_buf();
    config.staging.layout = StagingLayout::PerTrace;
    config.staging.retention_days = 60;
    config.staging.rejected_retention_days = 90;
    config
}

#[test]
fn sweep_empty_root_returns_zero() {
    let tmp = TempDir::new().unwrap();
    let config = make_config(&tmp.path().join("nonexistent"));
    let result = sweep(&config, false).unwrap();
    assert_eq!(result.scanned, 0);
    assert_eq!(result.deleted.len(), 0);
}

#[test]
fn sweep_retains_fresh_traces() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("stages");
    std::fs::create_dir_all(&root).unwrap();
    write_trace(&root, "tg-fresh", false);
    let config = make_config(&root);
    let result = sweep(&config, false).unwrap();
    assert_eq!(result.scanned, 1);
    assert_eq!(result.kept, 1);
    assert!(result.deleted.is_empty());
    assert!(root.join("tg-fresh").exists());
}

#[test]
fn sweep_deletes_old_successful_trace() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("stages");
    std::fs::create_dir_all(&root).unwrap();
    write_trace(&root, "tg-old", false);
    age_directory(&root.join("tg-old"), 70);
    let config = make_config(&root);
    let result = sweep(&config, false).unwrap();
    assert_eq!(result.scanned, 1);
    assert_eq!(result.deleted, vec!["tg-old".to_string()]);
    assert!(!root.join("tg-old").exists());
}

#[test]
fn sweep_retains_rejected_trace_within_longer_window() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("stages");
    std::fs::create_dir_all(&root).unwrap();
    write_trace(&root, "tg-reject", true);
    // Aged 70 days: past successful window (60d) but within rejected window (90d)
    age_directory(&root.join("tg-reject"), 70);
    let config = make_config(&root);
    let result = sweep(&config, false).unwrap();
    assert_eq!(result.scanned, 1);
    assert!(result.deleted.is_empty());
    assert!(root.join("tg-reject").exists());
}

#[test]
fn sweep_deletes_rejected_trace_beyond_rejected_window() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("stages");
    std::fs::create_dir_all(&root).unwrap();
    write_trace(&root, "tg-reject-old", true);
    age_directory(&root.join("tg-reject-old"), 100);
    let config = make_config(&root);
    let result = sweep(&config, false).unwrap();
    assert_eq!(result.deleted, vec!["tg-reject-old".to_string()]);
}

#[test]
fn sweep_dry_run_does_not_touch_disk() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("stages");
    std::fs::create_dir_all(&root).unwrap();
    write_trace(&root, "tg-old", false);
    age_directory(&root.join("tg-old"), 200);
    let config = make_config(&root);
    let result = sweep(&config, true).unwrap();
    assert_eq!(result.deleted, vec!["tg-old".to_string()]);
    assert!(root.join("tg-old").exists(), "dry-run must not delete");
}

#[test]
fn status_reports_counts_and_bytes() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("stages");
    std::fs::create_dir_all(&root).unwrap();
    write_trace(&root, "tg-a", false);
    write_trace(&root, "tg-b", true);
    let config = make_config(&root);
    let report = status(&config).unwrap();
    assert_eq!(report.traces, 2);
    assert_eq!(report.rejected, 1);
    assert!(report.total_bytes > 0);
}

// --- Phase 3/4: ingested-date parsing + trace-expires math -------------------

#[test]
fn parse_ingested_date_accepts_bare_date() {
    let d = parse_ingested_date("2026-06-20").expect("bare date parses");
    assert_eq!(d, NaiveDate::from_ymd_opt(2026, 6, 20).unwrap());
}

#[test]
fn parse_ingested_date_accepts_offset_datetime() {
    // The URL pipeline / backfill form.
    let d = parse_ingested_date("2026-06-20T20:40:27-07:00").expect("offset datetime parses");
    assert_eq!(d, NaiveDate::from_ymd_opt(2026, 6, 20).unwrap());
}

#[test]
fn parse_ingested_date_rejects_garbage() {
    assert!(parse_ingested_date("not-a-date").is_none());
    assert!(parse_ingested_date("").is_none());
}

#[test]
fn trace_expires_for_matches_design_example() {
    // The design's worked example: 2026-06-20 + 60 days = 2026-08-19.
    let ingested = NaiveDate::from_ymd_opt(2026, 6, 20).unwrap();
    assert_eq!(trace_expires_for(ingested, 60), "2026-08-19");
}

#[test]
fn trace_expires_for_crosses_year_boundary() {
    let ingested = NaiveDate::from_ymd_opt(2026, 12, 20).unwrap();
    assert_eq!(trace_expires_for(ingested, 60), "2027-02-18");
}

#[test]
fn trace_expires_from_either_ingested_format_is_identical() {
    // Whether ingested arrives bare or as an offset datetime, the stamped
    // expiry is the same calendar date.
    let bare = parse_ingested_date("2026-06-20").unwrap();
    let offset = parse_ingested_date("2026-06-20T20:40:27-07:00").unwrap();
    assert_eq!(trace_expires_for(bare, 60), trace_expires_for(offset, 60));
}

// --- raw-input sidecar sweep (intake.retention-days) -----------------------

/// A config whose vault root is `root` and whose sidecar window is `days`.
fn sidecar_config(root: &Path, days: u32) -> Config {
    let mut config = Config::default();
    config.vault.root_path = Some(root.display().to_string());
    config.intake.retention_days = days;
    config
}

/// Write a sidecar under `<root>/system/intake/` and age it `days_old` days.
fn write_sidecar(root: &Path, name: &str, days_old: i64) -> std::path::PathBuf {
    let dir = root.join("system").join("intake");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, b"https://example.com\n").unwrap();
    if days_old > 0 {
        age_directory(&path, days_old);
    }
    path
}

#[test]
fn sidecar_sweep_missing_dir_is_clean() {
    let tmp = TempDir::new().unwrap();
    let config = sidecar_config(tmp.path(), 90);
    let result = sweep_sidecars(&config, false).unwrap();
    assert!(result.enabled);
    assert_eq!(result.scanned, 0);
    assert!(result.deleted.is_empty());
}

#[test]
fn sidecar_sweep_retains_fresh_sidecar() {
    let tmp = TempDir::new().unwrap();
    let fresh = write_sidecar(tmp.path(), "ht-fresh.txt", 0);
    let config = sidecar_config(tmp.path(), 90);
    let result = sweep_sidecars(&config, false).unwrap();
    assert_eq!(result.scanned, 1);
    assert_eq!(result.kept, 1);
    assert!(result.deleted.is_empty());
    assert!(fresh.exists());
}

#[test]
fn sidecar_sweep_deletes_aged_sidecar() {
    let tmp = TempDir::new().unwrap();
    let old = write_sidecar(tmp.path(), "hv-old.txt", 120);
    let fresh = write_sidecar(tmp.path(), "hv-fresh.txt", 10);
    let config = sidecar_config(tmp.path(), 90);
    let result = sweep_sidecars(&config, false).unwrap();
    assert_eq!(result.scanned, 2);
    assert_eq!(result.kept, 1);
    assert_eq!(result.deleted, vec!["hv-old.txt".to_string()]);
    assert!(result.bytes_freed > 0);
    assert!(!old.exists());
    assert!(fresh.exists());
}

#[test]
fn sidecar_sweep_dry_run_does_not_touch_disk() {
    let tmp = TempDir::new().unwrap();
    let old = write_sidecar(tmp.path(), "tg-old.txt", 120);
    let config = sidecar_config(tmp.path(), 90);
    let result = sweep_sidecars(&config, true).unwrap();
    assert_eq!(result.deleted, vec!["tg-old.txt".to_string()]);
    assert!(old.exists());
}

#[test]
fn sidecar_sweep_zero_days_keeps_forever() {
    let tmp = TempDir::new().unwrap();
    let old = write_sidecar(tmp.path(), "cl-ancient.txt", 3000);
    let config = sidecar_config(tmp.path(), 0);
    let result = sweep_sidecars(&config, false).unwrap();
    assert!(!result.enabled);
    assert_eq!(result.scanned, 0);
    assert!(result.deleted.is_empty());
    assert!(old.exists());
}

#[test]
fn sidecar_sweep_ignores_non_txt_entries() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("system").join("intake");
    std::fs::create_dir_all(&dir).unwrap();
    let keeper = dir.join("notes.md");
    std::fs::write(&keeper, b"# not a sidecar\n").unwrap();
    age_directory(&keeper, 400);
    let config = sidecar_config(tmp.path(), 90);
    let result = sweep_sidecars(&config, false).unwrap();
    assert_eq!(result.scanned, 0);
    assert!(keeper.exists());
}

#[test]
fn sidecar_status_reports_files_bytes_and_window() {
    let tmp = TempDir::new().unwrap();
    write_sidecar(tmp.path(), "ht-a.txt", 0);
    write_sidecar(tmp.path(), "ht-b.txt", 200);
    let config = sidecar_config(tmp.path(), 45);
    let report = sidecar_status(&config).unwrap();
    assert_eq!(report.files, 2);
    assert!(report.total_bytes > 0);
    assert_eq!(report.retention_days, 45);
    assert!(report.dir.ends_with("system/intake"));
}
