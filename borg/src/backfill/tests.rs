use super::*;
use chrono_tz::America::Los_Angeles;
use std::collections::HashMap;
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

    // 3. skipped_already_present: assisted and already carries a homogeneous
    //    datetime ingested (the final form), so it is left untouched.
    let already_path = root.join("c-already.md");
    std::fs::write(
        &already_path,
        "---\norigin: assisted\ndate: 2026-04-03\ningested: 2026-04-03T00:00:00-07:00\n---\nbody\n",
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

    let report = backfill_on(root, &[], &HashMap::new(), Los_Angeles, true).expect("backfill_on dry_run");

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
    let report = backfill_on(dir.path(), &[], &HashMap::new(), Los_Angeles, true).expect("dry-run on empty vault");
    assert_eq!(report.scanned, 0);
    assert_eq!(report.would_backfill, 0);
    assert_eq!(report.backfilled, 0);
    assert_eq!(report.skipped_origin, 0);
    assert_eq!(report.skipped_already_had, 0);
    assert_eq!(report.skipped_no_date, 0);
    assert_eq!(report.skipped_recent_mtime, 0);
}

/// Write a note and backdate its mtime past the 60s race guard so the backfill
/// will actually consider it (rather than skipping as recently-modified).
fn write_old(path: &Path, content: &str) {
    use filetime::{FileTime, set_file_mtime};
    use std::time::{SystemTime, UNIX_EPOCH};
    std::fs::write(path, content).expect("write note");
    let old = FileTime::from_unix_time(
        (SystemTime::now().duration_since(UNIX_EPOCH).expect("epoch").as_secs() - 7200) as i64,
        0,
    );
    set_file_mtime(path, old).expect("set mtime");
}

#[test]
fn local_from_utc_converts_z_to_local_offset() {
    // 15:27:25Z in LA (PDT, summer) is 08:27:25-07:00.
    let got = local_from_utc("2026-06-05T15:27:25Z", Los_Angeles).expect("parse");
    assert_eq!(got, "2026-06-05T08:27:25-07:00");
}

#[test]
fn local_from_utc_rejects_garbage() {
    assert!(local_from_utc("not-a-timestamp", Los_Angeles).is_none());
}

#[test]
fn local_date_midnight_homogenizes_to_offset_datetime() {
    // April is daylight time in LA -> -07:00; the column stays a single type.
    let got = local_date_midnight("2026-04-01", Los_Angeles).expect("parse");
    assert_eq!(got, "2026-04-01T00:00:00-07:00");
}

#[test]
fn backfill_upgrades_date_only_to_precise_from_receipts() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("note.md");
    // Date-only ingested already present; a receipt match must OVERWRITE it.
    write_old(
        &path,
        "---\norigin: assisted\ndate: 2026-06-04\ningested: 2026-06-04\ntrace: ht-ea9e2a\n---\nbody\n",
    );

    let mut receipts = HashMap::new();
    receipts.insert("ht-ea9e2a".to_string(), "2026-06-05T08:27:25-07:00".to_string());

    let report = backfill_on(dir.path(), &[], &receipts, Los_Angeles, false).expect("backfill");
    assert_eq!(report.backfilled, 1, "the date-only note is upgraded");
    assert_eq!(report.precise, 1, "the upgrade is sourced from a receipt");

    let updated = std::fs::read_to_string(&path).expect("read back");
    assert!(
        updated.contains("ingested: 2026-06-05T08:27:25-07:00"),
        "got: {updated}"
    );
    assert!(
        !updated.contains("ingested: 2026-06-04"),
        "stale date-only value must be gone: {updated}"
    );
}

#[test]
fn backfill_date_fallback_is_homogenized_midnight() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("note.md");
    // No receipt match, no existing ingested: promote date: to local midnight.
    write_old(
        &path,
        "---\norigin: assisted\ndate: 2026-04-01\ntrace: ht-orphan\n---\nbody\n",
    );

    let report = backfill_on(dir.path(), &[], &HashMap::new(), Los_Angeles, false).expect("backfill");
    assert_eq!(report.backfilled, 1);
    assert_eq!(report.precise, 0, "no receipt -> not a precise backfill");

    let updated = std::fs::read_to_string(&path).expect("read back");
    assert!(
        updated.contains("ingested: 2026-04-01T00:00:00-07:00"),
        "got: {updated}"
    );
}

#[test]
fn backfill_covers_source_bearing_note_with_mislabeled_origin() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("note.md");
    // origin: authored (the historical mislabel) but it has a source URL, so it
    // IS a borg-ingested, view-visible note and must get an `ingested:`.
    write_old(
        &path,
        "---\norigin: authored\ndate: 2026-04-01\nsource: \"https://youtu.be/x\"\n---\nbody\n",
    );

    let report = backfill_on(dir.path(), &[], &HashMap::new(), Los_Angeles, false).expect("backfill");
    assert_eq!(
        report.backfilled, 1,
        "source-bearing note is backfilled despite origin: authored"
    );
    assert_eq!(report.skipped_origin, 0);

    let updated = std::fs::read_to_string(&path).expect("read back");
    assert!(
        updated.contains("ingested: 2026-04-01T00:00:00-07:00"),
        "got: {updated}"
    );
}

#[test]
fn backfill_homogenizes_existing_date_only_ingested_without_receipt() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("note.md");
    // Date-only `ingested:` (from the legacy backfill), no receipt match: the
    // value is homogenized in place to local midnight, NOT replaced by date:.
    write_old(
        &path,
        "---\norigin: assisted\ndate: 2026-04-10\ningested: 2026-04-03\ntrace: ht-orphan\n---\nbody\n",
    );

    let report = backfill_on(dir.path(), &[], &HashMap::new(), Los_Angeles, false).expect("backfill");
    assert_eq!(report.backfilled, 1, "date-only ingested is homogenized");
    assert_eq!(report.precise, 0);

    let updated = std::fs::read_to_string(&path).expect("read back");
    // Preserves the ingested date (04-03), not the content date (04-10).
    assert!(
        updated.contains("ingested: 2026-04-03T00:00:00-07:00"),
        "got: {updated}"
    );
}

#[test]
fn backfill_idempotent_when_value_already_matches() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("note.md");
    // Already carries exactly the precise value the receipt would produce.
    write_old(
        &path,
        "---\norigin: assisted\ndate: 2026-06-04\ningested: 2026-06-05T08:27:25-07:00\ntrace: ht-ea9e2a\n---\nbody\n",
    );

    let mut receipts = HashMap::new();
    receipts.insert("ht-ea9e2a".to_string(), "2026-06-05T08:27:25-07:00".to_string());

    let report = backfill_on(dir.path(), &[], &receipts, Los_Angeles, false).expect("backfill");
    assert_eq!(report.backfilled, 0, "no rewrite when the value is unchanged");
    assert_eq!(report.skipped_already_had, 1);
}
