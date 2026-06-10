use super::*;

#[test]
fn test_extract_frontmatter_field_source() {
    let content = "---\ntitle: \"Test\"\nsource: \"https://example.com\"\ntype: article\n---\n\n# Body\n";
    assert_eq!(
        extract_frontmatter_field(content, "source"),
        Some("https://example.com".to_string())
    );
}

#[test]
fn test_extract_frontmatter_field_type() {
    let content = "---\ntitle: \"Test\"\ntype: youtube\n---\n\n# Body\n";
    assert_eq!(extract_frontmatter_field(content, "type"), Some("youtube".to_string()));
}

#[test]
fn test_extract_frontmatter_field_missing() {
    let content = "---\ntitle: \"Test\"\n---\n\n# Body\n";
    assert_eq!(extract_frontmatter_field(content, "source"), None);
}

#[test]
fn test_extract_frontmatter_no_frontmatter() {
    let content = "# Just a heading\n";
    assert_eq!(extract_frontmatter_field(content, "type"), None);
}

#[test]
fn test_audit_finding_display() {
    let finding = AuditFinding::Mistype {
        source: "https://github.com/owner/repo".to_string(),
        current_type: "article".to_string(),
        expected_type: "github".to_string(),
        note_path: None,
    };
    let display = format!("{finding}");
    assert!(display.contains("[MISTYPE]"));
    assert!(display.contains("github"));
    assert!(display.contains("article"));
}

#[test]
fn test_audit_finding_display_blocked() {
    let finding = AuditFinding::Blocked {
        source: "https://example.com".to_string(),
        title: "Just a moment...".to_string(),
        note_path: None,
    };
    let display = format!("{finding}");
    assert!(display.contains("[BLOCKED]"));
    assert!(display.contains("Just a moment"));
}

#[test]
fn test_audit_finding_display_raw_title() {
    let finding = AuditFinding::RawTitle {
        source: "https://example.com".to_string(),
        title: "https://example.com".to_string(),
        note_path: None,
    };
    let display = format!("{finding}");
    assert!(display.contains("[RAW-TITLE]"));
}

#[test]
fn test_audit_finding_display_duplicate() {
    let finding = AuditFinding::Duplicate {
        source: "https://example.com".to_string(),
        note_paths: vec![PathBuf::from("/a.md"), PathBuf::from("/b.md")],
    };
    let display = format!("{finding}");
    assert!(display.contains("[DUPLICATE]"));
    assert!(display.contains("2 notes"));
}

#[test]
fn test_audit_finding_display_github_creator_missing() {
    let finding = AuditFinding::GithubCreatorMissing {
        source: "https://github.com/open-webui/open-terminal".to_string(),
        owner: "open-webui".to_string(),
        note_path: PathBuf::from("/vault/open-webui-open-terminal.md"),
    };
    let display = format!("{finding}");
    assert!(display.contains("[GITHUB-CREATOR-MISSING]"));
    assert!(display.contains("open-webui"));
}

#[test]
fn test_set_creator_if_empty_inserts_after_type() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("repo.md");
    std::fs::write(
            &path,
            "---\ntitle: \"open-webui/open-terminal\"\nsource: \"https://github.com/open-webui/open-terminal\"\ntype: github\ntags:\n  - github\n---\n\n# Body\n",
        )
        .expect("write");

    let changed = set_creator_if_empty(&path, "open-webui").expect("set");
    assert!(changed, "should report a write");

    let content = std::fs::read_to_string(&path).expect("read");
    assert_eq!(
        extract_frontmatter_field(&content, "creator"),
        Some("open-webui".to_string())
    );
    // Inserted directly after the type line.
    let type_pos = content.find("type: github").expect("type line");
    let creator_pos = content.find("creator: \"open-webui\"").expect("creator line");
    assert!(creator_pos > type_pos, "creator should follow type");
    // Body and other fields are untouched.
    assert!(content.contains("# Body"));
    assert!(content.contains("source: \"https://github.com/open-webui/open-terminal\""));
}

#[test]
fn test_set_creator_if_empty_replaces_empty_line_no_duplicate() {
    // A note with `creator: ""` (treated as absent by extract) must end up
    // with exactly ONE creator line carrying the owner - not a duplicate.
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("repo.md");
    std::fs::write(
        &path,
        "---\ntitle: \"o/r\"\nsource: \"https://github.com/o/r\"\ntype: github\ncreator: \"\"\n---\n\n# Body\n",
    )
    .expect("write");

    let changed = set_creator_if_empty(&path, "o").expect("set");
    assert!(changed);

    let content = std::fs::read_to_string(&path).expect("read");
    assert_eq!(
        content.matches("creator:").count(),
        1,
        "exactly one creator line:\n{content}"
    );
    assert_eq!(extract_frontmatter_field(&content, "creator"), Some("o".to_string()));
}

#[test]
fn test_set_creator_if_empty_skips_when_present() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("repo.md");
    let original =
        "---\ntitle: \"o/r\"\nsource: \"https://github.com/o/r\"\ntype: github\ncreator: \"hand-set\"\n---\n\n# Body\n";
    std::fs::write(&path, original).expect("write");

    let changed = set_creator_if_empty(&path, "o").expect("set");
    assert!(!changed, "must not overwrite an existing creator");

    let content = std::fs::read_to_string(&path).expect("read");
    assert_eq!(content, original, "file unchanged when creator present");
}

#[test]
fn test_fix_note_type() {
    let dir = std::env::temp_dir().join("obsidian-borg-test-fix-type");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("test-fix.md");
    std::fs::write(
        &path,
        "---\ntitle: \"Test\"\ntype: article\ntags:\n  - test\n---\n\n# Body\n",
    )
    .expect("write");

    fix_note_type(&path, "github").expect("fix");

    let content = std::fs::read_to_string(&path).expect("read");
    assert!(content.contains("type: github"));
    assert!(!content.contains("type: article"));
    assert!(content.contains("title: \"Test\""));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_audit_finding_display_orphaned_replacement() {
    let finding = AuditFinding::OrphanReplace {
        source: "https://example.com/video".to_string(),
        replaced_date: "2026-03-18".to_string(),
    };
    let display = format!("{finding}");
    assert!(display.contains("[ORPHAN-REPLACE]"));
    assert!(display.contains("2026-03-18"));
    assert!(display.contains("no replacement"));
}

/// Phase 3 determinism guard: parallel `build_note_index` must produce the same per-source
/// `Vec<PathBuf>` ordering as the previous sequential implementation. The contract: notes
/// that share a source URL appear in the same order they would appear in the sorted
/// `collect_md_files` output. Build a fixture where two notes point at the same source URL
/// and verify both the key set and the per-key path order.
#[test]
fn build_note_index_per_source_order_matches_sorted_md_files_under_par_iter() {
    // Fixture of 50 notes - the size the design doc specifies. The 50 notes split across
    // three source URLs so each URL's Vec<PathBuf> has many entries, making any par_iter
    // ordering bug obvious. Names use zero-padded indices so sort-order matches numeric
    // order independent of platform locale.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    let shared_url = "https://example.com/shared";
    let other_url = "https://example.com/other";
    let unique_url = "https://example.com/unique";

    let mut expected_shared = Vec::new();
    let mut expected_other = Vec::new();
    for i in 0u32..50 {
        let name = format!("note-{i:03}.md");
        // Spread across three buckets: 25 shared, 24 other, 1 unique.
        let src = if i == 25 {
            unique_url
        } else if i % 2 == 0 {
            shared_url
        } else {
            other_url
        };
        std::fs::write(
            root.join(&name),
            format!("---\ntitle: N{i}\nsource: {src}\n---\nbody\n"),
        )
        .expect("write fixture note");
        if src == shared_url {
            expected_shared.push(name.clone());
        } else if src == other_url {
            expected_other.push(name.clone());
        }
    }

    let index = build_note_index(root, &[]).expect("index");

    let shared = index.get(shared_url).expect("shared key present");
    let shared_names: Vec<String> = shared
        .iter()
        .map(|p| p.file_name().expect("path has filename").to_string_lossy().to_string())
        .collect();
    assert_eq!(
        shared_names, expected_shared,
        "per-source path order for {shared_url} must match sorted collect_md_files order"
    );

    let other = index.get(other_url).expect("other key present");
    let other_names: Vec<String> = other
        .iter()
        .map(|p| p.file_name().expect("path has filename").to_string_lossy().to_string())
        .collect();
    assert_eq!(
        other_names, expected_other,
        "per-source path order for {other_url} must match sorted collect_md_files order"
    );

    let unique = index.get(unique_url).expect("unique key present");
    assert_eq!(unique.len(), 1);
}

// ---- fixtures for the new --fix <kinds> integration tests ----

fn make_vault() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("system").join("views")).expect("mkdir views");
    (tmp, root)
}

fn write_ledger(root: &Path, body: &str) -> PathBuf {
    let path = root.join("system").join("views").join("borg-ledger.md");
    let header = "| Date | Time | Method | Status | Note | Source | Domain | Trace |\n|------|------|--------|--------|------|--------|--------|-------|\n";
    std::fs::write(
        &path,
        format!("---\ntitle: Borg Ledger\n---\n\n# Borg Ledger\n\n{header}{body}\n"),
    )
    .expect("write ledger");
    path
}

fn write_note(root: &Path, rel: &str, source: &str, title: &str) -> PathBuf {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir note parent");
    }
    std::fs::write(
        &path,
        format!("---\ntitle: \"{title}\"\nsource: {source}\ntype: article\n---\n\n# Body\n"),
    )
    .expect("write note");
    path
}

fn set_mtime(path: &Path, seconds_since_epoch: u64) {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds_since_epoch);
    std::fs::File::open(path)
        .and_then(|f| f.set_modified(t))
        .expect("set_modified");
}

fn collect_events<F>(report: &AuditReport, kinds: &[FindingKind], f: F) -> Vec<String>
where
    F: FnOnce(usize),
{
    let mut lines: Vec<String> = Vec::new();
    let fixed = apply_fixes(report, kinds, |event| {
        lines.push(format!("{event:?}"));
    });
    f(fixed);
    lines
}

// ---- new tests ----

#[test]
fn quarantine_key_sanitizes_urls() {
    assert_eq!(quarantine_key("https://example.com/foo"), "https-example-com-foo");
    assert_eq!(quarantine_key("pais-migration"), "pais-migration");
    assert_eq!(quarantine_key("!!!"), "unknown");
    // Cap at 80 chars.
    let long = "x".repeat(200);
    assert!(quarantine_key(&long).len() <= 80);
    // Trailing dashes trimmed.
    assert!(!quarantine_key("https://x/").ends_with('-'));
}

#[test]
fn finding_kind_projection() {
    let f = AuditFinding::Mistype {
        source: "s".into(),
        current_type: "a".into(),
        expected_type: "b".into(),
        note_path: None,
    };
    assert_eq!(f.kind(), FindingKind::Mistype);
    let f = AuditFinding::OrphanReplace {
        source: "s".into(),
        replaced_date: "2026-01-01".into(),
    };
    assert_eq!(f.kind(), FindingKind::OrphanReplace);
}

#[test]
fn apply_fix_orphan_replace_drops_row() {
    let (_tmp, root) = make_vault();
    let src = "https://example.com/abandoned";
    let body = format!(
        "| 2026-03-29 | 10:00 | http | \u{1F504} | [[old]] | {src} | ai | tr-1 |\n\
             | 2026-03-28 | 09:00 | http | \u{2705} | [[other]] | https://example.com/other | ai | tr-0 |\n"
    );
    let ledger_path = write_ledger(&root, &body);

    let report = AuditReport {
        ledger_path: ledger_path.clone(),
        vault_root: root.clone(),
        entries_scanned: 0,
        no_ledger: false,
        findings: vec![AuditFinding::OrphanReplace {
            source: src.to_string(),
            replaced_date: "2026-03-29".to_string(),
        }],
        fixed_count: 0,
    };

    let lines = collect_events(&report, &[FindingKind::OrphanReplace], |fixed| assert_eq!(fixed, 1));
    assert!(
        lines.iter().any(|l| l.contains("RowDropped")),
        "expected RowDropped event in {lines:?}"
    );

    let after = std::fs::read_to_string(&ledger_path).expect("read");
    assert!(!after.contains(src), "orphan row should be gone:\n{after}");
    assert!(after.contains("https://example.com/other"), "unrelated row preserved");
}

#[test]
fn apply_fix_blocked_removes_note_and_drops_row() {
    let (_tmp, root) = make_vault();
    let src = "https://blocked.example.com/post";
    let note_path = write_note(&root, "inbox/blocked-note.md", src, "Just a moment...");
    let body = format!(
        "| 2026-03-20 | 10:00 | http | \u{2705} | [[blocked-note]] | {src} | ai | tr-1 |\n\
             | 2026-03-19 | 09:00 | http | \u{2705} | [[other]] | https://example.com/other | ai | tr-0 |\n"
    );
    let ledger_path = write_ledger(&root, &body);

    let report = AuditReport {
        ledger_path: ledger_path.clone(),
        vault_root: root.clone(),
        entries_scanned: 0,
        no_ledger: false,
        findings: vec![AuditFinding::Blocked {
            source: src.to_string(),
            title: "Just a moment...".to_string(),
            note_path: Some(note_path.clone()),
        }],
        fixed_count: 0,
    };

    let lines = collect_events(&report, &[FindingKind::Blocked], |fixed| assert_eq!(fixed, 1));
    assert!(lines.iter().any(|l| l.contains("NoteRemoved")));

    assert!(!note_path.exists(), "blocked note should be removed");
    let after = std::fs::read_to_string(&ledger_path).expect("read");
    assert!(!after.contains(src), "blocked ledger row should be gone");
    assert!(after.contains("https://example.com/other"));
}

#[test]
fn apply_fix_raw_title_removes_note_and_drops_row() {
    let (_tmp, root) = make_vault();
    let src = "https://example.com/no-title";
    let note_path = write_note(&root, "inbox/no-title.md", src, src);
    let body = format!("| 2026-03-20 | 10:00 | http | \u{2705} | [[no-title]] | {src} | x | tr-1 |\n");
    let ledger_path = write_ledger(&root, &body);

    let report = AuditReport {
        ledger_path: ledger_path.clone(),
        vault_root: root.clone(),
        entries_scanned: 0,
        no_ledger: false,
        findings: vec![AuditFinding::RawTitle {
            source: src.to_string(),
            title: src.to_string(),
            note_path: Some(note_path.clone()),
        }],
        fixed_count: 0,
    };

    let lines = collect_events(&report, &[FindingKind::RawTitle], |fixed| assert_eq!(fixed, 1));
    assert!(lines.iter().any(|l| l.contains("NoteRemoved")));

    assert!(!note_path.exists());
    let after = std::fs::read_to_string(&ledger_path).expect("read");
    assert!(!after.contains(src));
}

#[test]
fn apply_fix_duplicate_keeps_newest_quarantines_rest() {
    let (_tmp, root) = make_vault();
    let src = "https://example.com/dup";

    let oldest = write_note(&root, "inbox/dup-a.md", src, "A");
    let middle = write_note(&root, "inbox/dup-b.md", src, "B");
    let newest = write_note(&root, "inbox/dup-c.md", src, "C");
    set_mtime(&oldest, 1_000_000);
    set_mtime(&middle, 2_000_000);
    set_mtime(&newest, 3_000_000);

    let ledger_path = write_ledger(&root, "");

    let report = AuditReport {
        ledger_path,
        vault_root: root.clone(),
        entries_scanned: 0,
        no_ledger: false,
        findings: vec![AuditFinding::Duplicate {
            source: src.to_string(),
            note_paths: vec![oldest.clone(), middle.clone(), newest.clone()],
        }],
        fixed_count: 0,
    };

    let lines = collect_events(&report, &[FindingKind::Duplicate], |fixed| assert_eq!(fixed, 1));
    assert!(
        lines.iter().any(|l| l.contains("Quarantined")),
        "expected Quarantined event in {lines:?}"
    );

    assert!(newest.exists(), "newest should be kept in place");
    assert!(!oldest.exists(), "oldest should be moved out");
    assert!(!middle.exists(), "middle should be moved out");

    let key = quarantine_key(src);
    let quarantine_root = root.join("system").join("quarantine").join(&key);
    assert!(
        quarantine_root.join("inbox").join("dup-a.md").exists(),
        "oldest should be at quarantine/{key}/inbox/dup-a.md"
    );
    assert!(quarantine_root.join("inbox").join("dup-b.md").exists());
}

#[test]
fn apply_fixes_filters_by_kind() {
    let (_tmp, root) = make_vault();
    let src_orphan = "https://example.com/orphan";
    let src_other = "https://example.com/other";
    let body = format!(
        "| 2026-03-29 | 10:00 | http | \u{1F504} | [[old]] | {src_orphan} | ai | tr-1 |\n\
             | 2026-03-28 | 09:00 | http | \u{2705} | [[other]] | {src_other} | ai | tr-0 |\n"
    );
    let ledger_path = write_ledger(&root, &body);

    let report = AuditReport {
        ledger_path: ledger_path.clone(),
        vault_root: root.clone(),
        entries_scanned: 0,
        no_ledger: false,
        findings: vec![
            AuditFinding::OrphanReplace {
                source: src_orphan.to_string(),
                replaced_date: "2026-03-29".to_string(),
            },
            AuditFinding::Mistype {
                source: src_other.to_string(),
                current_type: "article".to_string(),
                expected_type: "github".to_string(),
                // Note: no note_path -> mistype is unfixable; that's fine
                // since we're verifying the kind filter, not the fix.
                note_path: None,
            },
        ],
        fixed_count: 0,
    };

    // Only request OrphanReplace; the Mistype finding should be ignored.
    let lines = collect_events(&report, &[FindingKind::OrphanReplace], |fixed| {
        assert_eq!(fixed, 1, "only orphan-replace should be fixed");
    });
    assert!(lines.iter().any(|l| l.contains("RowDropped")));
    assert!(!lines.iter().any(|l| l.contains("Fixed {")));

    let after = std::fs::read_to_string(&ledger_path).expect("read");
    assert!(!after.contains(src_orphan), "orphan row gone");
    assert!(after.contains(src_other), "other row preserved");
}

#[test]
fn apply_fixes_empty_kinds_means_all() {
    let (_tmp, root) = make_vault();
    let src = "https://example.com/orphan";
    let body = format!("| 2026-03-29 | 10:00 | http | \u{1F504} | [[old]] | {src} | ai | tr-1 |\n");
    let ledger_path = write_ledger(&root, &body);

    let report = AuditReport {
        ledger_path: ledger_path.clone(),
        vault_root: root.clone(),
        entries_scanned: 0,
        no_ledger: false,
        findings: vec![AuditFinding::OrphanReplace {
            source: src.to_string(),
            replaced_date: "2026-03-29".to_string(),
        }],
        fixed_count: 0,
    };

    // Empty kinds slice means "fix everything".
    collect_events(&report, &[], |fixed| assert_eq!(fixed, 1));
    let after = std::fs::read_to_string(&ledger_path).expect("read");
    assert!(!after.contains(src));
}
