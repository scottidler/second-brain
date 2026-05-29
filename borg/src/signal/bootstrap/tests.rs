use super::*;
use tempfile::TempDir;

fn marker_path(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("signal-bootstrap.json")
}

#[test]
fn absent_marker_reads_not_done() {
    let dir = TempDir::new().expect("tempdir");
    assert!(!bootstrap_done(&marker_path(&dir), "+15039990803", 2));
}

#[test]
fn recorded_marker_reads_done_for_matching_identity() {
    let dir = TempDir::new().expect("tempdir");
    let path = marker_path(&dir);
    record_bootstrap(&path, "+15039990803", 2, 1780035141921);
    assert!(bootstrap_done(&path, "+15039990803", 2));
}

#[test]
fn mismatched_account_reads_not_done() {
    let dir = TempDir::new().expect("tempdir");
    let path = marker_path(&dir);
    record_bootstrap(&path, "+15039990803", 2, 1780035141921);
    assert!(!bootstrap_done(&path, "+19998887777", 2));
}

#[test]
fn mismatched_device_id_reads_not_done() {
    let dir = TempDir::new().expect("tempdir");
    let path = marker_path(&dir);
    record_bootstrap(&path, "+15039990803", 2, 1780035141921);
    assert!(!bootstrap_done(&path, "+15039990803", 3));
}

#[test]
fn corrupt_marker_reads_not_done() {
    let dir = TempDir::new().expect("tempdir");
    let path = marker_path(&dir);
    std::fs::write(&path, "{not valid json").expect("write corrupt marker");
    assert!(!bootstrap_done(&path, "+15039990803", 2));
}

#[test]
fn record_creates_missing_parent_dirs() {
    let dir = TempDir::new().expect("tempdir");
    let nested = dir.path().join("sb").join("borg").join("signal-bootstrap.json");
    record_bootstrap(&nested, "+15039990803", 2, 1780035141921);
    assert!(bootstrap_done(&nested, "+15039990803", 2));
}
