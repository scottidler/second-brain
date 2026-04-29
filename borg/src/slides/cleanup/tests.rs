#![allow(clippy::unwrap_used)]

use super::*;

fn write_note(path: &Path, frontmatter_yaml: &str, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let content = format!("---\n{frontmatter_yaml}---\n{body}");
    std::fs::write(path, content).unwrap();
}

#[test]
fn test_read_old_slides_frontmatter_present() {
    let tmp = std::env::temp_dir().join("borg-test-cleanup-read");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let note = tmp.join("note.md");
    write_note(
        &note,
        "title: Hello\ntrace: ht-abc\nslides:\n  - system/attachments/images/2026-04/foo-slide-001.jpg\n  - system/attachments/images/2026-04/foo-slide-002.jpg\n",
        "# body\n",
    );
    let slides = read_old_slides_frontmatter(&note).unwrap();
    assert_eq!(slides.len(), 2);
    assert!(slides[0].ends_with("foo-slide-001.jpg"));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_read_old_slides_frontmatter_missing() {
    let tmp = std::env::temp_dir().join("borg-test-cleanup-missing");
    let _ = std::fs::remove_dir_all(&tmp);
    let note = tmp.join("nonexistent.md");
    let slides = read_old_slides_frontmatter(&note).unwrap();
    assert!(slides.is_empty());
}

#[test]
fn test_read_old_slides_frontmatter_no_slides_field() {
    let tmp = std::env::temp_dir().join("borg-test-cleanup-nofield");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let note = tmp.join("note.md");
    write_note(&note, "title: Hello\ntrace: ht-abc\n", "# body\n");
    let slides = read_old_slides_frontmatter(&note).unwrap();
    assert!(slides.is_empty());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_read_old_slides_frontmatter_no_frontmatter_block() {
    let tmp = std::env::temp_dir().join("borg-test-cleanup-noblock");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let note = tmp.join("note.md");
    std::fs::write(&note, "no frontmatter here\n").unwrap();
    let slides = read_old_slides_frontmatter(&note).unwrap();
    assert!(slides.is_empty());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_read_old_slides_frontmatter_malformed_yaml() {
    let tmp = std::env::temp_dir().join("borg-test-cleanup-malformed");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let note = tmp.join("note.md");
    write_note(&note, "!! not yaml ::?\n", "# body\n");
    // We tolerate malformed frontmatter; cleanup is best-effort.
    let slides = read_old_slides_frontmatter(&note).unwrap();
    assert!(slides.is_empty());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_compute_orphans_basic() {
    let old = vec!["a.jpg".to_string(), "b.jpg".to_string(), "c.jpg".to_string()];
    let new = vec!["b.jpg".to_string(), "d.jpg".to_string()];
    let orphans = compute_orphans(&old, &new);
    assert_eq!(orphans, vec!["a.jpg", "c.jpg"]);
}

#[test]
fn test_compute_orphans_empty_new_returns_all() {
    let old = vec!["a.jpg".to_string(), "b.jpg".to_string()];
    let orphans = compute_orphans(&old, &[]);
    assert_eq!(orphans, vec!["a.jpg", "b.jpg"]);
}

#[test]
fn test_compute_orphans_empty_old_returns_empty() {
    let orphans = compute_orphans(&[], &["a.jpg".to_string()]);
    assert!(orphans.is_empty());
}

#[test]
fn test_compute_orphans_no_change_returns_empty() {
    let old = vec!["a.jpg".to_string()];
    let orphans = compute_orphans(&old, &old);
    assert!(orphans.is_empty());
}

#[test]
fn test_resolve_existing_filters_missing() {
    let tmp = std::env::temp_dir().join("borg-test-cleanup-resolve");
    let _ = std::fs::remove_dir_all(&tmp);
    let real_dir = tmp.join("real");
    std::fs::create_dir_all(&real_dir).unwrap();
    let real = real_dir.join("a.jpg");
    std::fs::write(&real, b"data").unwrap();
    let resolved = resolve_existing(&tmp, &["real/a.jpg".to_string(), "real/missing.jpg".to_string()]);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0], real);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_rkvr_remove_empty_is_noop() {
    // Empty input must not invoke rkvr at all - it would otherwise error
    // on missing arguments. Keeps cleanup_orphans cheap when there's nothing to do.
    rkvr_remove(&[]).unwrap();
}

#[test]
fn test_cleanup_orphans_end_to_end() {
    // Skip if rkvr binary is unavailable - this is an integration test.
    if std::process::Command::new("rkvr").arg("--version").output().is_err() {
        eprintln!("rkvr not found; skipping test_cleanup_orphans_end_to_end");
        return;
    }
    let tmp = std::env::temp_dir().join("borg-test-cleanup-e2e");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let vault = tmp.join("vault");
    let attach = vault.join("system").join("attachments").join("images").join("2026-04");
    std::fs::create_dir_all(&attach).unwrap();

    // Pre-existing slides files (would-be orphans).
    let old_a = attach.join("foo-slide-001.jpg");
    let old_b = attach.join("foo-slide-002.jpg");
    std::fs::write(&old_a, b"old a").unwrap();
    std::fs::write(&old_b, b"old b").unwrap();

    // Note's old frontmatter still references both.
    let note = vault.join("foo.md");
    write_note(
        &note,
        "title: Foo\nslides:\n  - system/attachments/images/2026-04/foo-slide-001.jpg\n  - system/attachments/images/2026-04/foo-slide-002.jpg\n",
        "# body\n",
    );

    // After replay the new owned set keeps slide 002 but replaces 001 with 003.
    let new_owned = vec![
        "system/attachments/images/2026-04/foo-slide-002.jpg".to_string(),
        "system/attachments/images/2026-04/foo-slide-003.jpg".to_string(),
    ];

    let orphans = cleanup_orphans(&vault, &note, &new_owned).unwrap();
    assert_eq!(orphans.len(), 1);
    assert!(orphans[0].ends_with("foo-slide-001.jpg"));
    // The orphan file is gone from the vault (rkvr archived it elsewhere).
    assert!(!old_a.exists());
    // The kept file is untouched.
    assert!(old_b.exists());

    let _ = std::fs::remove_dir_all(&tmp);
}
