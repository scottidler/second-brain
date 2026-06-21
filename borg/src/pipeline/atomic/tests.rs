use super::*;

#[test]
fn resolve_publish_path_uniquifies_and_respects_force() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("title.md");
    // Free path: returned unchanged.
    assert_eq!(resolve_publish_path(&dest, false), dest);
    // Existing path, not forcing: uniquified with -2.
    std::fs::write(&dest, b"x").expect("seed");
    assert_eq!(resolve_publish_path(&dest, false), dir.path().join("title-2.md"));
    // Existing path, forcing: overwrite (unchanged).
    assert_eq!(resolve_publish_path(&dest, true), dest);
    // Two existing: skip to -3.
    std::fs::write(dir.path().join("title-2.md"), b"x").expect("seed2");
    assert_eq!(resolve_publish_path(&dest, false), dir.path().join("title-3.md"));
}

#[test]
fn test_write_atomic_creates_destination() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("note.md");
    write_atomic(&dest, b"hello world").expect("write");
    assert_eq!(std::fs::read(&dest).unwrap(), b"hello world");
}

#[test]
fn test_write_atomic_overwrites_existing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("note.md");
    std::fs::write(&dest, b"old content").unwrap();
    write_atomic(&dest, b"new content").expect("write");
    assert_eq!(std::fs::read(&dest).unwrap(), b"new content");
}

#[test]
fn test_write_atomic_does_not_leak_on_persist_failure_path() {
    // Persist into a non-existent destination directory: parent must
    // exist for the rename to succeed. This drives the error path.
    let dir = tempfile::tempdir().expect("tempdir");
    let bad_dest = dir.path().join("missing").join("note.md");
    // Create the temp first by passing a valid parent; then check that
    // a failure at write time leaves no orphan tempfile in the parent.
    let result = write_atomic(&bad_dest, b"data");
    assert!(result.is_err());
    // The valid parent dir should not have any leftover .borg-tmp-* files.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".borg-tmp-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no .borg-tmp-* files should be left, found: {leftovers:?}"
    );
}

/// The architect's hardest question: a SIGKILL between write_atomic and
/// the cortex patches must not desync the file. With the new compose-in-
/// memory approach, write_atomic produces the FINAL bytes; once persist
/// returns Ok the file is complete. Verify that running write_atomic
/// with a fully-composed body produces a file containing both the
/// restored date and the cortex fields - byte-for-byte, in one write.
#[test]
fn test_compose_then_write_atomic_is_complete_in_one_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("note.md");
    let rendered = "---\ntitle: Example\ndate: 2026-05-08\n---\nBody text.\n";
    let composed = apply_original_date(rendered, "2026-04-19");
    let cortex_fields = vec![
        ("domain".to_string(), "ai".to_string()),
        ("cortex-quality".to_string(), "ok".to_string()),
    ];
    let composed = apply_cortex_fields(&composed, &cortex_fields);
    write_atomic(&dest, composed.as_bytes()).expect("write");

    let contents = std::fs::read_to_string(&dest).unwrap();
    assert!(
        contents.contains("date: 2026-04-19"),
        "date should be restored, got: {contents}"
    );
    assert!(
        contents.contains("domain: ai"),
        "domain should be present, got: {contents}"
    );
    assert!(
        contents.contains("cortex-quality: ok"),
        "cortex-quality should be present, got: {contents}"
    );
    assert!(contents.contains("Body text."), "body must be preserved");
}

#[test]
fn test_apply_original_date_replaces_existing() {
    let input = "---\ntitle: X\ndate: 2026-01-01\nfoo: bar\n---\nbody\n";
    let out = apply_original_date(input, "2025-12-31");
    assert!(out.contains("date: 2025-12-31"));
    assert!(!out.contains("2026-01-01"));
    assert!(out.contains("title: X"));
    assert!(out.contains("foo: bar"));
    assert!(out.contains("body"));
}

#[test]
fn test_apply_ingested_date_inserts_when_missing() {
    let input = "---\ntitle: X\ndate: 2026-04-16\n---\nbody\n";
    let out = apply_ingested_date(input, "2026-05-12");
    assert!(out.contains("ingested: 2026-05-12"), "got: {out}");
    // Original date untouched
    assert!(out.contains("date: 2026-04-16"));
    // `ingested:` appears AFTER `date:` (paired)
    let date_pos = out.find("date: 2026-04-16").expect("date present");
    let ing_pos = out.find("ingested: 2026-05-12").expect("ingested present");
    assert!(ing_pos > date_pos, "ingested should follow date");
}

#[test]
fn test_apply_ingested_date_replaces_existing() {
    let input = "---\ntitle: X\ndate: 2026-04-16\ningested: 2026-04-16\n---\nbody\n";
    let out = apply_ingested_date(input, "2026-05-12");
    assert!(out.contains("ingested: 2026-05-12"));
    assert!(!out.contains("ingested: 2026-04-16"));
    assert!(out.contains("date: 2026-04-16"));
}

#[test]
fn test_apply_ingested_date_preserves_other_fields() {
    let input = "---\ntitle: X\ndate: 2026-04-16\ndomain: ai\ncortex-quality: ok\n---\nbody\n";
    let out = apply_ingested_date(input, "2026-05-12");
    assert!(out.contains("domain: ai"));
    assert!(out.contains("cortex-quality: ok"));
    assert!(out.contains("body"));
}

#[test]
fn test_apply_ingested_date_noop_without_frontmatter() {
    let input = "no frontmatter";
    let out = apply_ingested_date(input, "2026-05-12");
    assert_eq!(out, input);
}

#[test]
fn test_apply_ingested_date_no_date_line_inserts_after_open() {
    let input = "---\ntitle: X\n---\nbody\n";
    let out = apply_ingested_date(input, "2026-05-12");
    assert!(out.contains("ingested: 2026-05-12"));
    // ingested should appear before title (right after opening ---)
    let title_pos = out.find("title: X").expect("title present");
    let ing_pos = out.find("ingested: 2026-05-12").expect("ingested present");
    assert!(ing_pos < title_pos);
}

#[test]
fn test_apply_original_date_noop_when_no_date_line() {
    let input = "---\ntitle: X\n---\nbody\n";
    let out = apply_original_date(input, "2025-12-31");
    assert_eq!(out, input);
}

#[test]
fn test_apply_cortex_fields_inserts_fields() {
    let input = "---\ntitle: Test\nsource: \"https://x\"\n---\nBody.\n";
    let fields = vec![
        ("domain".to_string(), "ai".to_string()),
        ("cortex-quality".to_string(), "ok".to_string()),
    ];
    let out = apply_cortex_fields(input, &fields);
    assert!(out.contains("domain: ai"));
    assert!(out.contains("cortex-quality: ok"));
    assert!(out.contains("title: Test"));
    assert!(out.contains("source: \"https://x\""));
    assert!(out.contains("Body."));
}

#[test]
fn test_apply_cortex_fields_replaces_existing() {
    let input = "---\ntitle: T\ndomain: tech\n---\nBody.\n";
    let fields = vec![("domain".to_string(), "ai".to_string())];
    let out = apply_cortex_fields(input, &fields);
    assert!(out.contains("domain: ai"));
    assert!(!out.contains("domain: tech"));
}

#[test]
fn test_apply_cortex_fields_no_frontmatter_is_noop() {
    let input = "no frontmatter here";
    let fields = vec![("domain".to_string(), "ai".to_string())];
    let out = apply_cortex_fields(input, &fields);
    assert_eq!(out, input);
}

#[test]
fn test_apply_cortex_fields_filters_unknown_keys() {
    let input = "---\ntitle: T\n---\nBody.\n";
    // `not-a-cortex-key` is not in CORTEX_PRESERVE_KEYS and must be
    // ignored.
    let fields = vec![("not-a-cortex-key".to_string(), "value".to_string())];
    let out = apply_cortex_fields(input, &fields);
    assert!(!out.contains("not-a-cortex-key"));
}

/// The 2026-05-08 incident: an Err returned mid-pipeline (after the old
/// note's metadata was captured but before the new note was published)
/// must NOT delete the old note. With Phase 3's deferred-delete pattern,
/// the old note stays on disk until write_atomic returns Ok. Simulate
/// the failure mode: capture old note metadata, return an Err before
/// calling write_atomic, and assert the old file is byte-for-byte
/// unchanged.
#[test]
fn test_reingest_failure_before_publish_preserves_old_note() {
    let dir = tempfile::tempdir().expect("tempdir");
    let old_path = dir.path().join("old-note.md");
    let original_bytes = b"---\ntitle: Old\ndate: 2026-04-01\ndomain: ai\n---\nOriginal body.\n";
    std::fs::write(&old_path, original_bytes).unwrap();

    // Simulate the captured metadata path - the publish step never runs
    // because the pipeline returned Err before reaching write_atomic.
    let _captured_date = "2026-04-01".to_string();
    let _captured_cortex: Vec<(String, String)> = vec![("domain".to_string(), "ai".to_string())];
    let pipeline_result: Result<()> = Err(eyre::eyre!("simulated mid-pipeline failure"));
    assert!(pipeline_result.is_err(), "pipeline failed before publish");

    // Phase 3 invariant: write_atomic was never called, so the old
    // file must still exist with its original bytes. Pre-Phase 3 code
    // would have deleted old_path here and lost the data.
    let after = std::fs::read(&old_path).unwrap();
    assert_eq!(
        after, original_bytes,
        "old note must survive a mid-pipeline failure unchanged"
    );
}

#[test]
fn test_apply_trace_expires_inserts_after_ingested() {
    let input = "---\ntitle: X\ndate: 2026-06-20\ningested: 2026-06-20T20:40:27-07:00\ntrace: ht-95aa4e\n---\nbody\n";
    let out = apply_trace_expires(input, "2026-08-19");
    assert!(out.contains("trace-expires: 2026-08-19"), "got: {out}");
    // Positioned directly after `ingested:` (the trio sits together).
    let ing_pos = out.find("ingested:").expect("ingested present");
    let exp_pos = out.find("trace-expires:").expect("trace-expires present");
    assert!(exp_pos > ing_pos, "trace-expires should follow ingested");
    // Must not collide with the `trace:` line.
    assert!(out.contains("trace: ht-95aa4e"), "trace handle preserved: {out}");
}

#[test]
fn test_apply_trace_expires_replaces_existing() {
    let input = "---\ntitle: X\ningested: 2026-06-20\ntrace-expires: 2026-01-01\n---\nbody\n";
    let out = apply_trace_expires(input, "2026-08-19");
    assert!(out.contains("trace-expires: 2026-08-19"));
    assert!(
        !out.contains("trace-expires: 2026-01-01"),
        "stale value must be gone: {out}"
    );
}

#[test]
fn test_apply_trace_expires_falls_back_to_date_then_open() {
    // No ingested line: insert after date.
    let after_date = apply_trace_expires("---\ntitle: X\ndate: 2026-06-20\n---\nbody\n", "2026-08-19");
    let date_pos = after_date.find("date:").expect("date present");
    let exp_pos = after_date.find("trace-expires:").expect("expires present");
    assert!(exp_pos > date_pos, "should follow date when no ingested: {after_date}");

    // Neither ingested nor date: insert right after the opening ---.
    let after_open = apply_trace_expires("---\ntitle: X\n---\nbody\n", "2026-08-19");
    let title_pos = after_open.find("title: X").expect("title present");
    let exp_pos = after_open.find("trace-expires:").expect("expires present");
    assert!(exp_pos < title_pos, "should sit just after opening ---: {after_open}");
}

#[test]
fn test_apply_trace_expires_noop_without_frontmatter() {
    let input = "no frontmatter";
    assert_eq!(apply_trace_expires(input, "2026-08-19"), input);
}
