#![allow(clippy::unwrap_used)]
use super::*;
use filetime::{FileTime, set_file_mtime};
use tempfile::TempDir;
use vault::receipts::ReceiptKind;
use vault::schema::Method;

/// Write a minimal harvest-session-shaped note. `extra_fm` is raw YAML lines
/// (already newline-terminated) inserted into the frontmatter block; `body`
/// becomes the note body verbatim (a `## Summary` heading is NOT added
/// automatically - tests that need a degradation marker or a real summary
/// supply the whole thing).
fn write_note(root: &Path, rel: &str, extra_fm: &str, body: &str) -> PathBuf {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let content = format!("---\ntitle: \"Test Note\"\ntype: session\n{extra_fm}---\n\n{body}");
    std::fs::write(&path, content).unwrap();
    path
}

fn touch(path: &Path, unix_secs: i64) {
    set_file_mtime(path, FileTime::from_unix_time(unix_secs, 0)).unwrap();
}

const TRACE: &str = "hv-e5d240";
const SOURCE: &str = "clyde://8d6b6ef3-b564-4cea-8414-882fc88e75cf";

// --- is_clean ---------------------------------------------------------------

#[test]
fn is_clean_true_for_a_real_distilled_summary() {
    let dir = TempDir::new().unwrap();
    let path = write_note(
        dir.path(),
        "notes/clean.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\n",
        "## Summary\n\nA real summary of the session.\n",
    );
    let note = note::parse_note(dir.path(), &path).unwrap();
    assert!(is_clean(&note));
}

#[test]
fn is_clean_false_for_missing_summary_marker() {
    let dir = TempDir::new().unwrap();
    let path = write_note(
        dir.path(),
        "notes/degraded.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\n",
        "## Summary\n\n[missing-summary]\n\nsnippet\n",
    );
    let note = note::parse_note(dir.path(), &path).unwrap();
    assert!(!is_clean(&note));
}

#[test]
fn is_clean_false_for_yaml_parse_error_marker() {
    let dir = TempDir::new().unwrap();
    let path = write_note(
        dir.path(),
        "notes/degraded.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\n",
        "## Summary\n\n[yaml-parse-error]\n\nsnippet\n",
    );
    let note = note::parse_note(dir.path(), &path).unwrap();
    assert!(!is_clean(&note));
}

#[test]
fn is_clean_false_for_needs_review_flag_even_with_a_real_summary() {
    let dir = TempDir::new().unwrap();
    let path = write_note(
        dir.path(),
        "notes/flagged.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\ncortex-needs-review: true\n",
        "## Summary\n\nA real summary.\n",
    );
    let note = note::parse_note(dir.path(), &path).unwrap();
    assert!(!is_clean(&note));
}

#[test]
fn is_clean_false_when_distilled_key_is_absent() {
    let dir = TempDir::new().unwrap();
    let path = write_note(
        dir.path(),
        "notes/undistilled.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\n",
        "## Summary\n\nA real summary.\n",
    );
    let note = note::parse_note(dir.path(), &path).unwrap();
    assert!(!is_clean(&note));
}

// --- effective_timestamp -----------------------------------------------------

#[test]
fn effective_timestamp_uses_receipts_terminal_at_when_note_path_matches() {
    let dir = TempDir::new().unwrap();
    let path = write_note(
        dir.path(),
        "notes/a.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\n",
        "body\n",
    );
    touch(&path, 1_000_000_000); // deliberately far in the past

    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, "hv-aaaaaa", Method::Harvest, ReceiptKind::Session, SOURCE).unwrap();
    receipts::mark_succeeded(&conn, "hv-aaaaaa", path.to_str().unwrap(), false).unwrap();
    let receipt = receipts::get(&conn, "hv-aaaaaa").unwrap().unwrap();
    // terminal_at was stamped "now" by mark_succeeded, which is far later than
    // the artificially-old mtime - proving the receipts branch (not the mtime
    // fallback) supplied the value.
    let ts = effective_timestamp(dir.path(), &path, Some(&receipt));
    assert!(ts.unwrap() > 1_000_000_000);
}

#[test]
fn effective_timestamp_falls_back_to_mtime_when_receipts_note_path_differs() {
    let dir = TempDir::new().unwrap();
    let path = write_note(
        dir.path(),
        "notes/a.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\n",
        "body\n",
    );
    touch(&path, 1_000_000_000);

    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, "hv-aaaaaa", Method::Harvest, ReceiptKind::Session, SOURCE).unwrap();
    // Receipts points at a DIFFERENT path than the one we are asking about.
    receipts::mark_succeeded(&conn, "hv-aaaaaa", "inbox/other.md", false).unwrap();
    let receipt = receipts::get(&conn, "hv-aaaaaa").unwrap().unwrap();
    let ts = effective_timestamp(dir.path(), &path, Some(&receipt));
    assert_eq!(ts, Some(1_000_000_000));
}

// --- plan_groups --------------------------------------------------------------

/// Mirrors the real `hv-e5d240` cohort at a reduced scale: a no-slug degraded
/// note (the `-5` analog), a needs-review degraded note (the `-7` analog),
/// and two clean notes with distinct slugs. The survivor must be a clean note
/// and must NEVER be the no-slug degraded one, per the design's explicit
/// rejection of "earliest ingested".
#[test]
fn plan_groups_picks_a_clean_survivor_never_the_degraded_no_slug_fork() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    let degraded_no_slug = write_note(
        root,
        "notes/review-ci-workflow-security-changes-5.md",
        &format!("trace: {TRACE}\nsource: \"{SOURCE}\"\ndistilled: true\n"),
        "## Summary\n\n[missing-summary]\n\nsnippet\n",
    );
    touch(&degraded_no_slug, 1_753_300_000); // oldest (2026-07-24-ish)

    let degraded_needs_review = write_note(
        root,
        "notes/review-ci-workflow-security-changes-7.md",
        &format!(
            "trace: {TRACE}\nsource: \"{SOURCE}\"\ndistilled: true\nslug: review-ci-workflow-security-changes\ncortex-needs-review: true\n"
        ),
        "## Summary\n\n[yaml-parse-error]\n\nsnippet\n",
    );
    touch(&degraded_needs_review, 1_753_300_100);

    let clean_a = write_note(
        root,
        "notes/ci-yml-public-repo-reusable-workflow-migration.md",
        &format!(
            "trace: {TRACE}\nsource: \"{SOURCE}\"\ndistilled: true\nslug: ci-yml-public-repo-reusable-workflow-migration\n"
        ),
        "## Summary\n\nA real security review summary.\n",
    );
    touch(&clean_a, 1_755_200_000); // 2026-08-15-ish

    let clean_b = write_note(
        root,
        "notes/clyde-ci-public-reusable-workflow-migration.md",
        &format!(
            "trace: {TRACE}\nsource: \"{SOURCE}\"\ndistilled: true\nslug: clyde-ci-public-reusable-workflow-migration\n"
        ),
        "## Summary\n\nAnother real security review summary.\n",
    );
    touch(&clean_b, 1_755_200_500); // latest of the two clean notes

    let conn = receipts::open_memory().unwrap();
    let notes = note::scan_vault(root, &ScanConfig::default()).unwrap();
    let groups = plan_groups(root, &conn, &notes).unwrap();

    assert_eq!(groups.len(), 1);
    let g = &groups[0];
    assert_eq!(g.trace, TRACE);
    assert_eq!(g.survivor, clean_b.strip_prefix(root).unwrap());
    assert!(
        g.tombstoned
            .contains(&degraded_no_slug.strip_prefix(root).unwrap().to_path_buf())
    );
    assert!(
        g.tombstoned
            .contains(&degraded_needs_review.strip_prefix(root).unwrap().to_path_buf())
    );
    assert!(
        g.tombstoned
            .contains(&clean_a.strip_prefix(root).unwrap().to_path_buf())
    );
    assert_eq!(g.tombstoned.len(), 3);
}

#[test]
fn plan_groups_never_groups_across_different_traces() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_note(
        root,
        "notes/one.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\n",
        "## Summary\n\nreal\n",
    );
    write_note(
        root,
        "notes/two.md",
        "trace: hv-bbbbbb\nsource: \"clyde://s2\"\ndistilled: true\n",
        "## Summary\n\nreal\n",
    );
    let conn = receipts::open_memory().unwrap();
    let notes = note::scan_vault(root, &ScanConfig::default()).unwrap();
    let groups = plan_groups(root, &conn, &notes).unwrap();
    assert!(groups.is_empty());
}

#[test]
fn plan_groups_skips_notes_already_tombstoned() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_note(
        root,
        "notes/live.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\n",
        "## Summary\n\nreal\n",
    );
    write_note(
        root,
        "notes/already-tombstoned.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\nsuperseded-by: live\n",
        "Merged into [[live]].\n",
    );
    let conn = receipts::open_memory().unwrap();
    let notes = note::scan_vault(root, &ScanConfig::default()).unwrap();
    let groups = plan_groups(root, &conn, &notes).unwrap();
    // Only one un-tombstoned member remains for this trace - not a group.
    assert!(groups.is_empty());
}

#[test]
fn plan_groups_splits_a_same_trace_different_source_collision() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // A 32-bit trace collision: two DIFFERENT sessions land in the same
    // trace bucket. Neither sub-cohort reaches size 2, so nothing is
    // tombstoned across the collision.
    write_note(
        root,
        "notes/session-one.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\n",
        "## Summary\n\nreal one\n",
    );
    write_note(
        root,
        "notes/session-two.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s2\"\ndistilled: true\n",
        "## Summary\n\nreal two\n",
    );
    let conn = receipts::open_memory().unwrap();
    let notes = note::scan_vault(root, &ScanConfig::default()).unwrap();
    let groups = plan_groups(root, &conn, &notes).unwrap();
    assert!(
        groups.is_empty(),
        "different-source notes sharing a trace must never be cross-tombstoned"
    );
}

// --- apply_group --------------------------------------------------------------

#[test]
fn apply_group_writes_the_tombstone_contract_shape() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let survivor = write_note(
        root,
        "notes/survivor.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\nslug: survivor\n",
        "## Summary\n\nreal\n",
    );
    let loser = write_note(
        root,
        "notes/loser.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\nslug: loser\ndomain: tech\n",
        "## Summary\n\nreal but a fork\n",
    );
    let group = DedupeGroup {
        trace: "hv-aaaaaa".to_string(),
        survivor: survivor.strip_prefix(root).unwrap().to_path_buf(),
        tombstoned: vec![loser.strip_prefix(root).unwrap().to_path_buf()],
    };
    apply_group(root, &group).unwrap();

    let rewritten = std::fs::read_to_string(&loser).unwrap();
    let (fm, body) = vault::frontmatter::parse_frontmatter(&rewritten).unwrap();
    assert!(!fm.extra.contains_key("slug"), "slug must be stripped from a tombstone");
    assert_eq!(fm.extra.get("superseded-by").and_then(|v| v.as_str()), Some("survivor"));
    // A field this design does not own (domain:) survives untouched. It is a
    // PROMOTED Frontmatter field (not `extra`), so `to_yaml()`'s explicit
    // `domain:` emission is what is under test here.
    assert_eq!(fm.domain.as_deref(), Some("tech"));
    assert_eq!(body.trim(), "Merged into [[survivor]].");
}

// --- run_with: dry-run vs apply, and the post-run zero-groups property ------

#[test]
fn run_with_dry_run_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let survivor = write_note(
        root,
        "notes/survivor.md",
        &format!("trace: {TRACE}\nsource: \"{SOURCE}\"\ndistilled: true\nslug: survivor\n"),
        "## Summary\n\nreal\n",
    );
    let loser = write_note(
        root,
        "notes/loser.md",
        &format!("trace: {TRACE}\nsource: \"{SOURCE}\"\ndistilled: true\nslug: loser\n"),
        "## Summary\n\nreal but earlier\n",
    );
    touch(&survivor, 2_000_000_000);
    touch(&loser, 1_000_000_000);
    let before = std::fs::read_to_string(&loser).unwrap();

    let conn = receipts::open_memory().unwrap();
    let report = run_with(root, &conn, &StagingConfig::default(), &DedupeOpts::default()).unwrap();

    assert!(!report.applied);
    assert_eq!(report.groups.len(), 1);
    let after = std::fs::read_to_string(&loser).unwrap();
    assert_eq!(before, after, "dry-run must not write anything");
}

#[test]
fn run_with_apply_then_rerun_reports_zero_groups_and_the_inbound_link_still_resolves() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let survivor = write_note(
        root,
        "notes/survivor.md",
        &format!("trace: {TRACE}\nsource: \"{SOURCE}\"\ndistilled: true\nslug: survivor\n"),
        "## Summary\n\nreal\n",
    );
    let loser = write_note(
        root,
        "notes/loser.md",
        &format!("trace: {TRACE}\nsource: \"{SOURCE}\"\ndistilled: true\nslug: loser\n"),
        "## Summary\n\nreal but earlier\n",
    );
    touch(&survivor, 2_000_000_000);
    touch(&loser, 1_000_000_000);
    // An unrelated note links to the loser by its filename stem - this must
    // still resolve after the loser becomes a tombstone (same filename, same
    // path - only frontmatter/body change).
    write_note(
        root,
        "notes/other.md",
        "type: article\n",
        "See [[loser]] for details.\n",
    );

    let conn = receipts::open_memory().unwrap();
    let opts = DedupeOpts {
        apply: true,
        purge: false,
    };
    let first = run_with(root, &conn, &StagingConfig::default(), &opts).unwrap();
    assert!(first.applied);
    assert_eq!(first.groups.len(), 1);
    assert!(loser.exists(), "a tombstone is a rewrite in place, never a delete");

    let second = run_with(root, &conn, &StagingConfig::default(), &opts).unwrap();
    assert!(
        second.groups.is_empty(),
        "the tombstoned loser must never re-group on a second run"
    );

    // The inbound wikilink still finds a real file at the same path.
    let notes = note::scan_vault(root, &ScanConfig::default()).unwrap();
    let index = build_link_index(&notes);
    let rel_loser = loser.strip_prefix(root).unwrap().to_path_buf();
    let inbound = inbound_links(&index, &rel_loser, &stem_of(&rel_loser));
    assert_eq!(inbound, vec![PathBuf::from("notes/other.md")]);
    assert!(root.join(&rel_loser).exists());
}

// --- plan_backfill ------------------------------------------------------------

#[test]
fn plan_backfill_backfills_from_staged_body_and_reports_uncovered_when_staging_gone() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let staging_dir = TempDir::new().unwrap();
    let staging = StagingConfig {
        root: staging_dir.path().to_path_buf(),
        ..StagingConfig::default()
    };
    let store = FsArtifactStore::from_config(&staging);
    store.write_body("hv-aaaaaa", b"the canonical thread body").unwrap();

    let covered = write_note(
        root,
        "notes/covered.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\n",
        "## Summary\n\nreal\n",
    );
    let uncovered = write_note(
        root,
        "notes/uncovered.md",
        "trace: hv-bbbbbb\nsource: \"clyde://s2\"\ndistilled: true\n",
        "## Summary\n\nreal\n",
    );

    let notes = note::scan_vault(root, &ScanConfig::default()).unwrap();
    let empty: HashSet<&PathBuf> = HashSet::new();
    let report = plan_backfill(root, &staging, &notes, &empty, true).unwrap();

    let covered_rel = covered.strip_prefix(root).unwrap().to_path_buf();
    let uncovered_rel = uncovered.strip_prefix(root).unwrap().to_path_buf();
    assert_eq!(report.backfilled, vec![covered_rel]);
    assert_eq!(report.uncovered, vec![uncovered_rel]);

    let rewritten = std::fs::read_to_string(&covered).unwrap();
    let (fm, _) = vault::frontmatter::parse_frontmatter(&rewritten).unwrap();
    let expected_hash = crate::harvest::watermark::body_hash("the canonical thread body");
    assert_eq!(
        fm.extra.get("harvest-body-hash").and_then(|v| v.as_str()),
        Some(expected_hash.as_str())
    );
}

#[test]
fn plan_backfill_skips_tombstones_and_already_hashed_notes() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let staging_dir = TempDir::new().unwrap();
    let staging = StagingConfig {
        root: staging_dir.path().to_path_buf(),
        ..StagingConfig::default()
    };
    let store = FsArtifactStore::from_config(&staging);
    store.write_body("hv-aaaaaa", b"body").unwrap();
    store.write_body("hv-bbbbbb", b"body2").unwrap();

    write_note(
        root,
        "notes/tombstoned.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\nsuperseded-by: survivor\n",
        "Merged into [[survivor]].\n",
    );
    write_note(
        root,
        "notes/already-hashed.md",
        "trace: hv-bbbbbb\nsource: \"clyde://s2\"\ndistilled: true\nharvest-body-hash: deadbeef\n",
        "## Summary\n\nreal\n",
    );

    let notes = note::scan_vault(root, &ScanConfig::default()).unwrap();
    let empty: HashSet<&PathBuf> = HashSet::new();
    let report = plan_backfill(root, &staging, &notes, &empty, true).unwrap();
    assert!(report.backfilled.is_empty());
    assert!(report.uncovered.is_empty());
}

// --- run_purge ------------------------------------------------------------------

#[test]
fn run_purge_refuses_a_tombstone_with_a_live_inbound_link() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_note(
        root,
        "notes/tombstone.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\nsuperseded-by: survivor\n",
        "Merged into [[survivor]].\n",
    );
    write_note(
        root,
        "notes/linker.md",
        "type: article\n",
        "Still see [[tombstone]] for history.\n",
    );

    let notes = note::scan_vault(root, &ScanConfig::default()).unwrap();
    let report = run_purge(root, &notes, &[], true).unwrap();
    assert!(report.archived.is_empty());
    assert_eq!(report.refused.len(), 1);
    assert_eq!(report.refused[0].0, PathBuf::from("notes/tombstone.md"));
    assert_eq!(report.refused[0].1, vec![PathBuf::from("notes/linker.md")]);
    assert!(
        root.join("notes/tombstone.md").exists(),
        "a refused tombstone is never archived"
    );
}

#[test]
fn run_purge_archives_an_orphaned_tombstone() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_note(
        root,
        "notes/tombstone.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\nsuperseded-by: survivor\n",
        "Merged into [[survivor]].\n",
    );
    // No other note links to it.

    let notes = note::scan_vault(root, &ScanConfig::default()).unwrap();
    let report = run_purge(root, &notes, &[], true).unwrap();
    assert_eq!(report.archived, vec![PathBuf::from("notes/tombstone.md")]);
    assert!(report.refused.is_empty());
    // `cfg!(test)` routes `rkvr::remove` to the non-recoverable fallback
    // (never the real `rkvr` binary / archive store), so the file is simply
    // gone from the tempdir.
    assert!(!root.join("notes/tombstone.md").exists());
}

#[test]
fn run_purge_dry_run_never_archives() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_note(
        root,
        "notes/tombstone.md",
        "trace: hv-aaaaaa\nsource: \"clyde://s1\"\ndistilled: true\nsuperseded-by: survivor\n",
        "Merged into [[survivor]].\n",
    );
    let notes = note::scan_vault(root, &ScanConfig::default()).unwrap();
    let report = run_purge(root, &notes, &[], false).unwrap();
    assert_eq!(report.archived, vec![PathBuf::from("notes/tombstone.md")]);
    assert!(root.join("notes/tombstone.md").exists(), "dry-run must not archive");
}
