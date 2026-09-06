#![allow(clippy::unwrap_used)]

use super::*;
use tempfile::TempDir;

/// Drives `rotating_log_writer` (the seam `setup_logging` uses) directly, no
/// `env_logger::Builder::init()` involved - `init()` installs a process-global
/// logger exactly once, so touching it here would break every other test in
/// this binary. Writing past `LOG_ROTATE_MAX_BYTES` must produce a `.1`
/// backup: proof the 16 GB unrotated-log defect (Phase 6 design doc) cannot
/// recur silently.
#[test]
fn test_rotating_log_writer_rotates_past_the_byte_cap() {
    let dir = TempDir::new().unwrap();
    let log_path = dir.path().join("cortex.log");
    let mut writer = rotating_log_writer(&log_path);

    // Two writes each just over half the cap guarantee the cap is crossed
    // without relying on a single write exceeding it outright.
    let chunk = vec![b'a'; LOG_ROTATE_MAX_BYTES / 2 + 1];
    writer.write_all(&chunk).unwrap();
    writer.write_all(&chunk).unwrap();
    writer.flush().unwrap();

    let rotated = dir.path().join("cortex.log.1");
    assert!(
        rotated.exists(),
        "expected a rotated backup once total bytes exceeded LOG_ROTATE_MAX_BYTES"
    );
}

/// Rotating well past `LOG_ROTATE_MAX_FILES` backups must never accumulate
/// more than `LOG_ROTATE_MAX_FILES` rotated files plus the active one -
/// the retention half of the size+retention contract this phase pins.
#[test]
fn test_rotating_log_writer_caps_backup_count() {
    let dir = TempDir::new().unwrap();
    let log_path = dir.path().join("cortex.log");
    let mut writer = rotating_log_writer(&log_path);

    let chunk = vec![b'a'; LOG_ROTATE_MAX_BYTES + 1];
    for _ in 0..(LOG_ROTATE_MAX_FILES + 3) {
        writer.write_all(&chunk).unwrap();
    }
    writer.flush().unwrap();

    let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert!(
        entries.len() <= LOG_ROTATE_MAX_FILES + 1,
        "expected at most {} files (active + {} backups), found {}",
        LOG_ROTATE_MAX_FILES + 1,
        LOG_ROTATE_MAX_FILES,
        entries.len()
    );
}

#[test]
fn test_resolve_log_level_precedence_cli_over_config() {
    let level = resolve_log_level(Some("debug"), Some("warn"));
    assert_eq!(level, "debug");
}

#[test]
fn test_resolve_log_level_falls_back_to_default() {
    // Neither cli nor config nor LOG_LEVEL env set (test runner doesn't set
    // LOG_LEVEL) -> "info".
    let level = resolve_log_level(None, None);
    assert_eq!(level, "info");
}

#[test]
fn prune_dead_pid_logs_keeps_live_and_removes_dead() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let root = dir.path();
    let live = std::process::id();
    // A live server's log and one of its rotations, plus a dead one.
    for name in [
        format!("oracle-{live}.log"),
        format!("oracle-{live}.log.1"),
        "oracle-999999999.log".to_string(),
        "oracle-999999999.log.2".to_string(),
    ] {
        std::fs::write(root.join(name), b"x").expect("write log");
    }
    // Unrelated files must survive: a different stem, and a non-pid name.
    std::fs::write(root.join("cortex-1.log"), b"x").expect("write other stem");
    std::fs::write(root.join("oracle.log"), b"x").expect("write shared log");

    super::prune_dead_pid_logs(root, "oracle");

    assert!(root.join(format!("oracle-{live}.log")).exists(), "live log removed");
    assert!(
        root.join(format!("oracle-{live}.log.1")).exists(),
        "live rotation removed"
    );
    assert!(!root.join("oracle-999999999.log").exists(), "dead log kept");
    assert!(!root.join("oracle-999999999.log.2").exists(), "dead rotation kept");
    assert!(root.join("cortex-1.log").exists(), "other stem removed");
    assert!(root.join("oracle.log").exists(), "non-pid log removed");
}

#[test]
fn prune_dead_pid_logs_on_missing_dir_is_not_fatal() {
    super::prune_dead_pid_logs(std::path::Path::new("/nonexistent/xyzzy"), "oracle");
}
