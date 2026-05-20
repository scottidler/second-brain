#![allow(clippy::unwrap_used)]

use super::*;
use tempfile::TempDir;

#[test]
fn rewrite_replaces_legacy_second_brain_paths() {
    let input = b"canonical-path: ~/.config/second-brain/canonical-tags.yml\nmapping-path: ~/.config/second-brain/tag-mapping.yml\n";
    let src = std::path::PathBuf::from("/tmp/borg.yml");
    let out = rewrite_legacy_paths_in_yaml(&src, input);
    let out_str = std::str::from_utf8(&out).unwrap();
    assert!(out_str.contains("~/.config/sb/canonical-tags.yml"));
    assert!(out_str.contains("~/.config/sb/tag-mapping.yml"));
    assert!(!out_str.contains("~/.config/second-brain/"));
}

#[test]
fn rewrite_leaves_custom_paths_untouched() {
    let input = b"canonical-path: /custom/path/tags.yml\n";
    let src = std::path::PathBuf::from("/tmp/borg.yml");
    let out = rewrite_legacy_paths_in_yaml(&src, input);
    let out_str = std::str::from_utf8(&out).unwrap();
    assert!(out_str.contains("/custom/path/tags.yml"));
}

#[test]
fn copy_file_migrates_when_dst_missing() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("borg-legacy.yml");
    let dst = tmp.path().join("borg-new.yml");
    std::fs::write(&src, "log-level: info\n").unwrap();

    let mut report = Report::default();
    copy_file_with_rewrite(&src, &dst, &mut report).unwrap();

    assert!(dst.exists());
    assert!(!report.had_conflicts);
    assert!(report.lines.iter().any(|l| l.contains("migrated")));
}

#[test]
fn copy_file_noops_when_dst_matches() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("borg-legacy.yml");
    let dst = tmp.path().join("borg-new.yml");
    let content = b"log-level: info\n";
    std::fs::write(&src, content).unwrap();
    std::fs::write(&dst, content).unwrap();

    let mut report = Report::default();
    copy_file_with_rewrite(&src, &dst, &mut report).unwrap();

    assert!(!report.had_conflicts);
    assert!(report.lines.iter().any(|l| l.contains("noop")));
}

#[test]
fn copy_file_conflicts_when_dst_differs() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("borg-legacy.yml");
    let dst = tmp.path().join("borg-new.yml");
    std::fs::write(&src, b"log-level: info\n").unwrap();
    std::fs::write(&dst, b"log-level: warn\n").unwrap();

    let mut report = Report::default();
    copy_file_with_rewrite(&src, &dst, &mut report).unwrap();

    assert!(report.had_conflicts);
    assert!(report.lines.iter().any(|l| l.contains("conflict")));
}
