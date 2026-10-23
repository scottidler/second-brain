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
fn test_read_old_owned_present() {
    let tmp = std::env::temp_dir().join("borg-test-cleanup-read");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let note = tmp.join("note.md");
    write_note(
        &note,
        "title: Hello\ntrace: ht-abc\nslides:\n  - system/attachments/images/2026-04/foo-slide-001.jpg\n  - system/attachments/images/2026-04/foo-slide-002.jpg\n",
        "# body\n",
    );
    let slides = read_old_owned(&note).unwrap().slides;
    assert_eq!(slides.len(), 2);
    assert!(slides[0].ends_with("foo-slide-001.jpg"));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_read_old_owned_missing() {
    let tmp = std::env::temp_dir().join("borg-test-cleanup-missing");
    let _ = std::fs::remove_dir_all(&tmp);
    let note = tmp.join("nonexistent.md");
    let slides = read_old_owned(&note).unwrap().slides;
    assert!(slides.is_empty());
}

#[test]
fn test_read_old_owned_no_slides_field() {
    let tmp = std::env::temp_dir().join("borg-test-cleanup-nofield");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let note = tmp.join("note.md");
    write_note(&note, "title: Hello\ntrace: ht-abc\n", "# body\n");
    let slides = read_old_owned(&note).unwrap().slides;
    assert!(slides.is_empty());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_read_old_owned_no_frontmatter_block() {
    let tmp = std::env::temp_dir().join("borg-test-cleanup-noblock");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let note = tmp.join("note.md");
    std::fs::write(&note, "no frontmatter here\n").unwrap();
    let slides = read_old_owned(&note).unwrap().slides;
    assert!(slides.is_empty());
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Inverted by the implementation-audit fold-in: malformed frontmatter used
/// to read as "owns nothing", which let a degraded reingest replace a note
/// that owns images and orphaned its files. It now fails closed.
#[test]
fn test_read_old_owned_malformed_yaml_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let note = tmp.path().join("note.md");
    write_note(&note, "images: [unclosed\n", "# body\n");
    let err = read_old_owned(&note).unwrap_err();
    assert!(format!("{err:#}").contains("parse frontmatter"), "{err:#}");
}

#[test]
fn test_read_old_owned_unterminated_frontmatter_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let note = tmp.path().join("note.md");
    std::fs::write(
        &note,
        "---\nimages:\n  - system/attachments/images/2026-10/t-1-a.png\n# body\n",
    )
    .unwrap();
    assert!(read_old_owned(&note).is_err());
}

#[test]
fn test_read_old_owned_reads_crlf_frontmatter() {
    let tmp = tempfile::tempdir().unwrap();
    let note = tmp.path().join("note.md");
    std::fs::write(
        &note,
        "---\r\ntitle: Hello\r\nimages:\r\n  - system/attachments/images/2026-10/t-1-aaaaaaaa.png\r\n---\r\n# body\r\n",
    )
    .unwrap();
    let owned = read_old_owned(&note).unwrap();
    assert_eq!(owned.images, vec!["system/attachments/images/2026-10/t-1-aaaaaaaa.png"]);
}

#[test]
fn test_read_old_owned_empty_frontmatter_owns_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let note = tmp.path().join("note.md");
    std::fs::write(&note, "---\n---\n# body\n").unwrap();
    assert_eq!(read_old_owned(&note).unwrap(), Owned::default());
}

#[test]
fn test_read_old_owned_reads_both_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let note = tmp.path().join("note.md");
    write_note(
        &note,
        "title: Hello\nslides:\n  - s/a.jpg\nimages:\n  - system/attachments/images/2026-10/tr-1-aaaaaaaa.png\n  - system/attachments/images/2026-10/tr-2-bbbbbbbb.jpg\n",
        "# body\n",
    );
    let owned = read_old_owned(&note).unwrap();
    assert_eq!(owned.slides, vec!["s/a.jpg"]);
    assert_eq!(owned.images.len(), 2);
}

#[test]
fn test_read_old_owned_unreadable_note_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    // A directory exists() but cannot be read as a note: fail closed.
    assert!(read_old_owned(tmp.path()).is_err());
}

#[test]
fn test_compute_all_orphans_diffs_each_key_on_its_own() {
    let old = Owned {
        slides: vec!["s/1.jpg".into(), "s/2.jpg".into()],
        images: vec!["i/1.png".into(), "i/2.png".into()],
    };
    let new = Owned {
        slides: vec!["s/2.jpg".into()],
        // A path that moved between keys is still an orphan of its old key.
        images: vec!["s/1.jpg".into(), "i/2.png".into()],
    };
    assert_eq!(compute_all_orphans(&old, &new), vec!["s/1.jpg", "i/1.png"]);
}

#[test]
fn test_cleanup_orphans_removes_dropped_image_and_keeps_the_kept_one() {
    let vault = tempfile::tempdir().unwrap();
    let dir = vault.path().join("system/attachments/images/2026-10");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tr-1-aaaaaaaa.png"), b"keep").unwrap();
    std::fs::write(dir.join("tr-2-bbbbbbbb.png"), b"drop").unwrap();
    let rel = |n: &str| format!("system/attachments/images/2026-10/{n}");
    let old = Owned {
        slides: Vec::new(),
        images: vec![rel("tr-1-aaaaaaaa.png"), rel("tr-2-bbbbbbbb.png")],
    };
    let new = Owned {
        slides: Vec::new(),
        images: vec![rel("tr-1-aaaaaaaa.png")],
    };
    let orphans = cleanup_orphans(vault.path(), &old, &new).unwrap();
    assert_eq!(orphans, vec![rel("tr-2-bbbbbbbb.png")]);
    assert!(dir.join("tr-1-aaaaaaaa.png").exists());
    assert!(!dir.join("tr-2-bbbbbbbb.png").exists());
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

/// Integration test against the real `rkvr` binary, which has no CI install
/// path: ignored by default, run by `otto ignored`. When run, a missing rkvr
/// fails the test rather than letting it pass without running.
#[test]
#[ignore = "needs rkvr on PATH"]
fn test_cleanup_orphans_end_to_end() {
    std::process::Command::new("rkvr")
        .arg("--version")
        .output()
        .expect("rkvr must be on PATH for this test");
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

    let old = read_old_owned(&note).unwrap();
    let new = Owned {
        slides: new_owned,
        images: Vec::new(),
    };
    let orphans = cleanup_orphans(&vault, &old, &new).unwrap();
    assert_eq!(orphans.len(), 1);
    assert!(orphans[0].ends_with("foo-slide-001.jpg"));
    // The orphan file is gone from the vault (rkvr archived it elsewhere).
    assert!(!old_a.exists());
    // The kept file is untouched.
    assert!(old_b.exists());

    let _ = std::fs::remove_dir_all(&tmp);
}
