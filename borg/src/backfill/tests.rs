use super::*;
use tempfile::tempdir;

#[test]
fn extract_frontmatter_field_works() {
    let c = "---\ntitle: T\norigin: assisted\ndate: 2026-04-16\n---\nbody\n";
    assert_eq!(extract_frontmatter_field(c, "origin"), Some("assisted".to_string()));
    assert_eq!(extract_frontmatter_field(c, "date"), Some("2026-04-16".to_string()));
    assert_eq!(extract_frontmatter_field(c, "ingested"), None);
}

#[test]
fn extract_frontmatter_field_returns_none_for_unfenced() {
    assert!(extract_frontmatter_field("no frontmatter", "date").is_none());
}

#[test]
fn collect_md_files_skips_listed_folders() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("a.md"), "x").expect("write");
    std::fs::create_dir_all(root.join(".obsidian")).expect("mkdir");
    std::fs::write(root.join(".obsidian/cache.md"), "x").expect("write");
    std::fs::create_dir_all(root.join("inbox")).expect("mkdir");
    std::fs::write(root.join("inbox/b.md"), "x").expect("write");
    let files = collect_md_files(root, &[".obsidian".to_string()]).expect("collect");
    let names: Vec<String> = files
        .iter()
        .map(|p| p.file_name().expect("name").to_string_lossy().to_string())
        .collect();
    assert!(names.contains(&"a.md".to_string()));
    assert!(names.contains(&"b.md".to_string()));
    assert!(!names.contains(&"cache.md".to_string()));
}

/// Architect-flagged Phase 3 deliverable: a known-input fixture produces the same
/// `BackfillReport` field values under the parallel implementation as the sequential baseline
/// would. Five notes exercise every counter:
/// - 1 backfills (origin=assisted, has date, no ingested, old mtime)
/// - 1 skipped_authored (origin=authored)
/// - 1 skipped_already_present (origin=assisted, has ingested)
/// - 1 skipped_no_date (origin=assisted, no date field)
/// - 1 skipped_recently_modified (origin=assisted, fresh mtime)
///
/// Each counter is verified independently so any par_iter/AtomicUsize aggregation bug
/// surfaces with a specific failed assertion, not just a total-count mismatch.
#[test]
fn backfill_on_counter_values_match_known_fixture() {
    use filetime::{FileTime, set_file_mtime};
    use std::time::{SystemTime, UNIX_EPOCH};

    let dir = tempdir().expect("tempdir");
    let root = dir.path();

    let old_mtime = FileTime::from_unix_time(
        (SystemTime::now().duration_since(UNIX_EPOCH).expect("epoch").as_secs() - 7200) as i64,
        0,
    );

    // 1. Will backfill: assisted + date + no ingested + old mtime.
    let backfill_path = root.join("a-backfill.md");
    std::fs::write(&backfill_path, "---\norigin: assisted\ndate: 2026-04-01\n---\nbody\n").expect("write backfill");
    set_file_mtime(&backfill_path, old_mtime).expect("set mtime backfill");

    // 2. skipped_authored: origin authored.
    let authored_path = root.join("b-authored.md");
    std::fs::write(&authored_path, "---\norigin: authored\ndate: 2026-04-02\n---\nbody\n").expect("write authored");
    set_file_mtime(&authored_path, old_mtime).expect("set mtime authored");

    // 3. skipped_already_present: assisted but already has ingested.
    let already_path = root.join("c-already.md");
    std::fs::write(
        &already_path,
        "---\norigin: assisted\ndate: 2026-04-03\ningested: 2026-04-03\n---\nbody\n",
    )
    .expect("write already");
    set_file_mtime(&already_path, old_mtime).expect("set mtime already");

    // 4. skipped_no_date: assisted but no date field.
    let nodate_path = root.join("d-nodate.md");
    std::fs::write(&nodate_path, "---\norigin: assisted\n---\nbody\n").expect("write nodate");
    set_file_mtime(&nodate_path, old_mtime).expect("set mtime nodate");

    // 5. skipped_recently_modified: assisted + date + no ingested but fresh mtime (default).
    let recent_path = root.join("e-recent.md");
    std::fs::write(&recent_path, "---\norigin: assisted\ndate: 2026-04-05\n---\nbody\n").expect("write recent");

    let report = backfill_on(root, &[], true).expect("backfill_on dry_run");

    assert_eq!(report.scanned, 5, "scanned should count every md file");
    assert_eq!(report.would_backfill, 1, "exactly one note is eligible to backfill");
    assert_eq!(report.backfilled, 0, "dry_run path leaves backfilled at zero");
    assert_eq!(report.skipped_origin, 1, "origin=authored is skipped");
    assert_eq!(report.skipped_already_had, 1, "note with existing ingested: is skipped");
    assert_eq!(report.skipped_no_date, 1, "note without date: is skipped");
    assert_eq!(
        report.skipped_recent_mtime, 1,
        "fresh-mtime note is skipped to avoid races"
    );
}

/// Phase 0 deliverable the audit flagged as missing: `borg backfill-ingested --dry-run` runs
/// to completion. Exercises the sync conversion end-to-end via the same `backfill_on`
/// helper that the CLI entry point now calls; assertion is that an empty tempdir returns
/// a zeroed report rather than panicking or propagating an error.
#[test]
fn backfill_ingested_dry_run_empty_vault_smoke() {
    let dir = tempdir().expect("tempdir");
    let report = backfill_on(dir.path(), &[], true).expect("dry-run on empty vault");
    assert_eq!(report.scanned, 0);
    assert_eq!(report.would_backfill, 0);
    assert_eq!(report.backfilled, 0);
    assert_eq!(report.skipped_origin, 0);
    assert_eq!(report.skipped_already_had, 0);
    assert_eq!(report.skipped_no_date, 0);
    assert_eq!(report.skipped_recent_mtime, 0);
}
