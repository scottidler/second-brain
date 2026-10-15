#![allow(clippy::unwrap_used)]

use super::*;
use tempfile::TempDir;

fn config(staging: &Path, archive: &Path) -> Config {
    let mut config = Config::default();
    config.staging.root = staging.to_path_buf();
    config.archive.root = archive.to_path_buf();
    config
}

fn stage(root: &Path, trace: &str, extractor: &str, transcript: Option<&str>, transcript_md: Option<&str>) {
    let dir = root.join(trace);
    std::fs::create_dir_all(&dir).unwrap();
    let mut distilled = Distilled {
        summary: "s".to_string(),
        transcript: transcript.map(str::to_string),
        ..Distilled::default()
    };
    distilled.meta.extractor = extractor.to_string();
    std::fs::write(dir.join(DISTILLED_FILENAME), serde_yaml::to_string(&distilled).unwrap()).unwrap();
    if let Some(md) = transcript_md {
        std::fs::write(dir.join(SOURCE_FILENAME), md).unwrap();
    }
}

fn fixture() -> (TempDir, TempDir, Config) {
    let staging = TempDir::new().unwrap();
    let archive = TempDir::new().unwrap();
    stage(staging.path(), "v1", "distill-video-v1", Some("video text"), None);
    stage(staging.path(), "r1", "distill-repo-v1", None, Some("readme text"));
    stage(staging.path(), "t1", "distill-thread-v1", None, Some("thread text"));
    stage(staging.path(), "s1", "distill-session-v1", None, None);
    std::fs::create_dir_all(staging.path().join("no-distilled")).unwrap();
    let cfg = config(staging.path(), archive.path());
    (staging, archive, cfg)
}

#[test]
fn dry_run_reports_and_writes_nothing() {
    let (_s, archive, cfg) = fixture();
    let report = run(&cfg, false).unwrap();
    assert_eq!(report.scanned, 4);
    assert_eq!(report.to_copy, vec!["r1", "t1", "v1"]);
    assert_eq!(report.skipped_session, 1);
    assert!(report.failed.is_empty());
    assert_eq!(std::fs::read_dir(archive.path()).unwrap().count(), 0);
}

#[test]
fn apply_copies_per_kind_and_second_run_has_nothing_to_copy() {
    let (_s, archive, cfg) = fixture();
    let report = run(&cfg, true).unwrap();
    assert_eq!(report.to_copy.len(), 3);
    let root = archive.path();
    assert!(root.join("v1").join(DISTILLED_FILENAME).is_file());
    assert!(!root.join("v1").join(SOURCE_FILENAME).exists());
    assert_eq!(
        std::fs::read_to_string(root.join("r1").join(SOURCE_FILENAME)).unwrap(),
        "readme text"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("t1").join(SOURCE_FILENAME)).unwrap(),
        "thread text"
    );
    assert!(!root.join("s1").exists(), "sessions are never archived");

    let again = run(&cfg, true).unwrap();
    assert_eq!(again.to_copy.len(), 0);
    assert_eq!(again.already_archived, 3);
    assert_eq!(again.skipped_session, 1);
}

#[test]
fn never_overwrites_an_existing_archive() {
    let (_s, archive, cfg) = fixture();
    let dir = archive.path().join("v1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(DISTILLED_FILENAME), "summary: keep me\n").unwrap();
    let report = run(&cfg, true).unwrap();
    assert!(!report.to_copy.contains(&"v1".to_string()));
    assert_eq!(
        std::fs::read_to_string(dir.join(DISTILLED_FILENAME)).unwrap(),
        "summary: keep me\n"
    );
}

#[test]
fn unparseable_distilled_is_reported_not_skipped() {
    let (staging, _a, cfg) = fixture();
    std::fs::write(staging.path().join("v1").join(DISTILLED_FILENAME), "{{not yaml").unwrap();
    let report = run(&cfg, true).unwrap();
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0, "v1");
}

#[test]
fn missing_staging_root_is_empty_report() {
    let archive = TempDir::new().unwrap();
    let cfg = config(Path::new("/nonexistent/staging-root"), archive.path());
    let report = run(&cfg, true).unwrap();
    assert_eq!(report.scanned, 0);
}
